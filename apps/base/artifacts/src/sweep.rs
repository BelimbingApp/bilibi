//! Scheduled retention of an owner's expired documents in every tenant.
//! `Owned::purge_expired` needs a person in the document's tenant and company,
//! so the expired bytes of a company nobody works in again would stay on
//! disk; an owner's scheduled job calls [`Artifacts::sweep_expired`] instead.
//! Each document is purged in its own tenant's audited transaction as the
//! `artifacts-sweeper` system principal, which holds
//! `artifacts.document.purge` in every live tenant unless that tenant denies
//! it (a standing grant, as Media's purger). Only what `purge_expired` would
//! take is taken: ordinary documents past their expiry, or already deleted,
//! never a retained or held one, with the same batch, retry interval and
//! attempt limit, so an operator resolves a stuck purge as for any other.
use crate::{
    evidence, storage, stored, Artifacts, Error, Owner, Recovery, Result, Stored, COLUMNS,
};
use bilibi_authz::SystemScope;
use bilibi_database::{Change, MutationTransaction};
use serde_json::json;
use std::any::type_name;
use tokio_postgres::Client;

/// The system principal a sweep purges as.
pub const SWEEPER: &str = "artifacts-sweeper";
/// What the sweeper holds in every tenant that does not deny it.
pub const PURGE: &str = "artifacts.document.purge";

impl Artifacts {
    /// One bounded batch of `owner`'s expired or deleted documents across
    /// tenants, purged as [`SWEEPER`]. The owner calls this from its own
    /// scheduled job (Schedule's `scheduler` principal); it answers which
    /// documents went and which failed, for the job to report by code.
    pub async fn sweep_expired<O: Owner>(
        &self,
        client: &mut Client,
        owner: &O,
    ) -> Result<Recovery> {
        let batch = self.number(client, "artifacts.purge_batch_size").await?;
        let retry = self.number(client, "artifacts.purge_retry_minutes").await?;
        let max = self.number(client, "artifacts.purge_max_attempts").await?;
        let rows = client.query("SELECT public_id, tenant_id FROM base_artifacts WHERE owner_id=$1 AND owner_adapter=$2 AND purged_at IS NULL AND purge_held_at IS NULL AND (expires_at <= clock_timestamp() OR deleted_at IS NOT NULL) AND (purge_attempted_at IS NULL OR purge_attempted_at <= clock_timestamp() - $3::bigint * interval '1 minute') ORDER BY expires_at, id LIMIT $4",
            &[&owner.owner_id(), &type_name::<O>(), &retry, &batch]).await?;
        let mut report = Recovery::default();
        for candidate in rows {
            let (id, tenant): (String, i64) = (candidate.get(0), candidate.get(1));
            match self.sweep_one(client, owner, tenant, &id).await {
                Ok(true) => report.purged.push(id),
                Ok(false) => {}
                Err(error) => {
                    // A refused tenant (denied grant, closed tenant) is not
                    // this document's failure; anything else is recorded.
                    if !matches!(
                        error,
                        Error::Database(
                            bilibi_database::Error::Forbidden
                                | bilibi_database::Error::Unauthorized
                        )
                    ) {
                        self.sweep_failed(client, owner, tenant, &id, &error, max)
                            .await?;
                    }
                    report.failed.push((id, error));
                }
            }
        }
        Ok(report)
    }

    /// Purges one document as the sweeper in its tenant; false when it no
    /// longer qualifies (purged, held, retained or not yet expired).
    async fn sweep_one<O: Owner>(
        &self,
        client: &mut Client,
        owner: &O,
        tenant: i64,
        id: &str,
    ) -> Result<bool> {
        let system = SystemScope::rehydrate(&*client, tenant, Some(SWEEPER)).await?;
        bilibi_authz::authorize_system(&*client, &system, PURGE).await?;
        let mut tx = self
            .boundary
            .begin(client, system.mutation_context("artifacts.sweep"))
            .await?;
        let Some(row) = lock(&tx, owner, tenant, id).await? else {
            return Ok(false);
        };
        if row.purged || row.refusal().is_some() || !(row.expired || row.deleted) {
            return Ok(false);
        }
        if !row.deleted {
            let row = mark(&mut tx, owner, tenant, &row, "deleted").await?;
            audit(&tx, owner, tenant, &row, "artifacts.delete").await?;
        }
        tx.commit().await?;
        let root = root_of(client, owner, tenant, id).await?;
        storage::io(|| storage::delete(&root, id))?;
        let mut tx = self
            .boundary
            .begin(client, system.mutation_context("artifacts.sweep"))
            .await?;
        let Some(row) = lock(&tx, owner, tenant, id).await? else {
            return Ok(false);
        };
        if !row.purged {
            let row = mark(&mut tx, owner, tenant, &row, "purged").await?;
            audit(&tx, owner, tenant, &row, "artifacts.purge").await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Records a failed sweep as `purge_expired` records its own: the attempt,
    /// its code, and a hold for an operator once the limit is reached.
    async fn sweep_failed<O: Owner>(
        &self,
        client: &mut Client,
        owner: &O,
        tenant: i64,
        id: &str,
        error: &Error,
        max: i64,
    ) -> Result<()> {
        let system = SystemScope::rehydrate(&*client, tenant, Some(SWEEPER)).await?;
        let mut tx = self
            .boundary
            .begin(client, system.mutation_context("artifacts.purge_failed"))
            .await?;
        let Some(row) = lock(&tx, owner, tenant, id).await? else {
            return Ok(());
        };
        if row.purged {
            return Ok(());
        }
        let attempts = row.attempts + 1;
        tx.sql().execute("UPDATE base_artifacts SET purge_attempts=$2::integer,purge_last_error=$3,purge_attempted_at=clock_timestamp(),purge_held_at=CASE WHEN $2::integer >= $4::bigint THEN clock_timestamp() END WHERE id=$1",
            &[&row.key, &attempts, &error.code(), &max]).await?;
        let next = lock(&tx, owner, tenant, id).await?.ok_or(Error::NotFound)?;
        tx.record(Change {
            kind: "base/artifacts.document".into(),
            id: row.key,
            old: evidence(&row),
            new: evidence(&next),
        })
        .await?;
        let facts = json!({"attempts": next.attempts, "reason": error.code(), "swept": true});
        audit_with(
            &tx,
            owner,
            tenant,
            &next,
            "artifacts.purge_failed",
            facts.clone(),
        )
        .await?;
        if next.held_at.is_some() {
            audit_with(&tx, owner, tenant, &next, "artifacts.purge_held", facts).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    async fn number(&self, client: &Client, key: &str) -> Result<i64> {
        self.settings
            .get_global(client, key)
            .await?
            .as_i64()
            .filter(|n| *n > 0)
            .ok_or(Error::Policy)
    }
}

/// The document, locked, in its tenant and owner.
async fn lock<O: Owner>(
    tx: &MutationTransaction<'_>,
    owner: &O,
    tenant: i64,
    id: &str,
) -> Result<Option<Stored>> {
    Ok(tx
        .sql()
        .query_opt(
            &format!("SELECT {COLUMNS}, company_id FROM base_artifacts WHERE public_id=$1 AND tenant_id=$2 AND owner_id=$3 AND owner_adapter=$4 FOR UPDATE"),
            &[&id, &tenant, &owner.owner_id(), &type_name::<O>()],
        )
        .await?
        .map(stored))
}

/// Where the document's bytes are, read outside a transaction for the unlink.
async fn root_of<O: Owner>(client: &Client, owner: &O, tenant: i64, id: &str) -> Result<String> {
    Ok(client
        .query_one(
            "SELECT storage_root FROM base_artifacts WHERE public_id=$1 AND tenant_id=$2 AND owner_id=$3 AND owner_adapter=$4",
            &[&id, &tenant, &owner.owner_id(), &type_name::<O>()],
        )
        .await?
        .get(0))
}

/// Stamps `deleted` or `purged` (chosen here, never by a caller) and records
/// the change, as an owner's own delete does.
async fn mark<O: Owner>(
    tx: &mut MutationTransaction<'_>,
    owner: &O,
    tenant: i64,
    row: &Stored,
    column: &str,
) -> Result<Stored> {
    let query = match column {
        "deleted" => "UPDATE base_artifacts SET deleted_at=clock_timestamp() WHERE id=$1",
        "purged" => "UPDATE base_artifacts SET purged_at=clock_timestamp() WHERE id=$1",
        _ => return Err(Error::Invalid),
    };
    tx.sql().execute(query, &[&row.key]).await?;
    let next = lock(tx, owner, tenant, &row.metadata.id)
        .await?
        .ok_or(Error::NotFound)?;
    tx.record(Change {
        kind: "base/artifacts.document".into(),
        id: row.key,
        old: evidence(row),
        new: evidence(&next),
    })
    .await?;
    Ok(next)
}

async fn audit<O: Owner>(
    tx: &MutationTransaction<'_>,
    owner: &O,
    tenant: i64,
    row: &Stored,
    action: &str,
) -> Result<()> {
    audit_with(tx, owner, tenant, row, action, json!({"swept": true})).await
}

/// Content-free evidence, as an owner's own purge records it.
async fn audit_with<O: Owner>(
    tx: &MutationTransaction<'_>,
    owner: &O,
    tenant: i64,
    row: &Stored,
    action: &str,
    details: serde_json::Value,
) -> Result<()> {
    let company: i64 = tx
        .sql()
        .query_one(
            "SELECT company_id FROM base_artifacts WHERE id=$1 AND tenant_id=$2",
            &[&row.key, &tenant],
        )
        .await?
        .get(0);
    let mut payload = json!({"artifact_id": row.metadata.id, "company_id": company, "owner_id": owner.owner_id()});
    if let (Some(payload), Some(details)) = (payload.as_object_mut(), details.as_object()) {
        payload.extend(details.clone());
    }
    bilibi_audit::record_action(
        tx.sql(),
        &bilibi_audit::Attribution::from_context(tx.context())?,
        None,
        action,
        payload,
        false,
    )
    .await?;
    Ok(())
}
