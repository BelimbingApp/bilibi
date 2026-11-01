//! Cached exports are replaceable copies, not business records. The record
//! owner invalidates its type in the same transaction as erasure. No Bilimbi
//! counterpart. The shared export guard covers reading through publication;
//! the exclusive erasure guard lasts until the owner's transaction commits.
use crate::{files::FILES, BulkData};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_personal_data::{Action, Erased, Erasure, Kept, KeptReason, Progress};
use bilibi_tenancy::Scope;
use serde_json::json;
use tokio_postgres::Client;

pub(crate) const EXPORT_GUARD: &str =
    "SELECT pg_advisory_xact_lock_shared(hashtextextended('bulk-export-' || $1::bigint::text || '-' || $2::text, 0))";

impl BulkData {
    /// Discard every cached export of the owner's record type: filters and
    /// linked columns can include the subject without naming their ID. Call
    /// inside the transaction that erases source data, before changing it.
    pub async fn erase_exports_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        grant: &Erasure,
        record_type: &str,
    ) -> Result<()> {
        grant.check(scope)?;
        let person = grant.person().ok_or(Error::Forbidden)?.get();
        let tenant = scope.tenant();
        if tx.context().tenant != tenant {
            return Err(Error::Forbidden);
        }
        tx.sql().query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended('bulk-export-' || $1::bigint::text || '-' || $2::text, 0))",
            &[&tenant, &record_type],
        ).await?;
        if tx.sql().query_opt(
            "SELECT id FROM base_bulk_data_exports WHERE tenant_id=$1 AND record_type=$2 AND NOT ($3=ANY(erased_for)) LIMIT 1",
            &[&tenant, &record_type, &person],
        ).await?.is_none() {
            return Ok(());
        }
        let record_type = record_type.to_owned();
        tx.mutate(move |sql| Box::pin(async move {
            let rows = sql.query(
                "UPDATE base_bulk_data_exports SET erased_for=array_append(erased_for,$3),state='failed', \
                 error='The source records changed after this export. Export the list again.', \
                 filter='{}',ids=NULL,except_ids=NULL,access=NULL,finished_at=coalesce(finished_at,now()),version=version+1 \
                 WHERE tenant_id=$1 AND record_type=$2 AND NOT ($3=ANY(erased_for)) RETURNING id",
                &[&tenant, &record_type, &person],
            ).await?;
            Ok(((), rows.iter().map(|row| Change {
                kind: "bulk_data_export".into(), id: row.get(0), old: serde_json::Value::Null,
                new: json!({"state":"failed","erased_for":person}),
            }).collect()))
        })).await
    }

    /// Finish invalidated-file cleanup through Artifacts' grant-checked path.
    /// A failed delete retries; retention and holds are reported, never hidden.
    pub async fn remove_erased_exports(
        &self,
        client: &mut Client,
        scope: &Scope,
        grant: &Erasure,
        record_type: &str,
        progress: &dyn Progress,
    ) -> Result<()> {
        grant.check(scope)?;
        let person = grant.person().ok_or(Error::Forbidden)?.get();
        let rows = client
            .query(
                "SELECT id FROM base_bulk_data_exports \
             WHERE tenant_id=$1 AND record_type=$2 AND $3=ANY(erased_for) ORDER BY id",
                &[&scope.tenant(), &record_type, &person],
            )
            .await?;
        for row in rows {
            let id: i64 = row.get(0);
            let subject = format!("export:{id}");
            let documents = self
                .artifacts
                .erasable_documents(client, scope, grant, &FILES, &subject)
                .await
                .map_err(crate::artifact)?;
            for (company, document) in documents {
                let artifact = document.id;
                let mut step = Erased::default();
                match self
                    .artifacts
                    .for_erasure(scope, company, &FILES, grant, &subject)
                    .delete(client, &artifact)
                    .await
                {
                    Ok(()) => step.count("bulk_data_export", Action::Deleted, 1),
                    Err(bilibi_artifacts::Error::LegalHold) => step.kept.push(Kept {
                        kind: "bulk_data_export".into(),
                        id: artifact,
                        reason: KeptReason::LegalHold,
                        until: None,
                        hold: None,
                    }),
                    Err(bilibi_artifacts::Error::Retained) => step.kept.push(Kept {
                        kind: "bulk_data_export".into(),
                        id: artifact,
                        reason: KeptReason::RetainedUntil,
                        until: document.retain_until,
                        hold: None,
                    }),
                    Err(bilibi_artifacts::Error::Database(error)) => return Err(error),
                    Err(_) => return Err(Error::Internal),
                }
                progress.record(step).await?;
            }
        }
        Ok(())
    }
}
