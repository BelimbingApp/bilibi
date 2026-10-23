//! Field-restriction administration (`base/authz.field_restrictions`): the
//! operator's runtime choice of which roles may not read which catalog fields.
//!
//! Every field is visible by default. A restriction names one field of one
//! [`field_catalog`](crate::field_catalog) table and the roles it is restricted
//! for; a reader holding any of them reads the field as withheld wherever
//! `fields::restricted`/`restrictions_for` is consulted, even alongside a
//! grant-all role. Storage is one `field_restrictions` row per role, so a
//! restriction is identified by its `table_id` and `field_id`.
//!
//! Writes require a live administrator holding `admin.authz.field.manage`,
//! checked inside the write's own transaction; only fields the installed catalog
//! offers may be restricted, and only roles of the actor's tenant named. Each
//! write records one mutation per role row and the action itself, committing
//! together; a capture failure prevents the write. Reads are fresh: a role
//! revoked or a restriction removed takes effect at the next check.
use crate::{authorize_key, field_catalog::FieldCatalog};
use bilibi_database::{Change, Db, Error, MutationBoundary, Result};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    time::SystemTime,
};
use tokio_postgres::{GenericClient, Transaction};

/// Capability every read and write of this service requires.
pub const MANAGE: &str = "admin.authz.field.manage";
/// At most this many fields in one bulk restriction.
pub const MAX_FIELDS: usize = 200;
/// At most this many roles named by one restriction.
pub const MAX_ROLES: usize = 100;

/// Stable read model of one restricted field with the roles it is restricted
/// for. Labels fall back to ids when the catalog no longer offers the field
/// (`restrictable` is then false: the row is stale and can only be removed).
#[derive(Clone, Debug)]
pub struct RestrictionSummary {
    pub table_id: String,
    pub table_label: String,
    pub field_id: String,
    pub field_label: String,
    pub restrictable: bool,
    /// When the earliest of its role rows was written.
    pub since: SystemTime,
    pub role_ids: Vec<i64>,
    pub role_names: Vec<String>,
}

/// One role a restriction may name: every role of the actor's tenant.
#[derive(Clone, Debug)]
pub struct RoleChoice {
    pub id: i64,
    pub name: String,
    pub is_system: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Removed {
    Removed,
    NotFound,
}

pub struct FieldRestrictions {
    db: Db,
    boundary: Arc<MutationBoundary>,
    authentication: Arc<bilibi_tenancy::Authentication>,
}
impl FieldRestrictions {
    pub fn new(
        db: Db,
        boundary: Arc<MutationBoundary>,
        authentication: Arc<bilibi_tenancy::Authentication>,
    ) -> Self {
        Self {
            db,
            boundary,
            authentication,
        }
    }

    /// What may be restricted for this actor; runtime definitions are fresh.
    pub async fn catalog(&self, actor: &Scope) -> Result<FieldCatalog> {
        let client = self.db.get().await?;
        self.administrator(&**client, actor).await?;
        crate::field_catalog::for_scope((&**client).into(), actor).await
    }

    /// The tenant's roles, by name, for choosing whom a field is restricted for.
    pub async fn roles(&self, actor: &Scope) -> Result<Vec<RoleChoice>> {
        let client = self.db.get().await?;
        self.administrator(&**client, actor).await?;
        Ok(client
            .query(
                "SELECT id,name,is_system FROM roles WHERE tenant_id=$1 ORDER BY lower(name),id",
                &[&actor.tenant()],
            )
            .await?
            .iter()
            .map(|row| RoleChoice {
                id: row.get(0),
                name: row.get(1),
                is_system: row.get(2),
            })
            .collect())
    }

    /// The tenant's restrictions, ordered by table label, field label and ids.
    pub async fn list(&self, actor: &Scope) -> Result<Vec<RestrictionSummary>> {
        let client = self.db.get().await?;
        self.administrator(&**client, actor).await?;
        let catalog = crate::field_catalog::for_scope((&**client).into(), actor).await?;
        list_in(&**client, actor.tenant(), &catalog).await
    }

    /// Restrict `field` of `table` for exactly `roles`, creating the restriction
    /// or replacing its role set. No roles is `Error::Invalid`: a restriction
    /// for nobody restricts nothing (remove it instead).
    pub async fn put(
        &self,
        actor: &Scope,
        table: &str,
        field: &str,
        roles: &[i64],
    ) -> Result<RestrictionSummary> {
        let mut written = self
            .put_many(actor, &[(table.to_owned(), field.to_owned())], roles)
            .await?;
        written.pop().ok_or(Error::Internal)
    }

    /// Restrict several fields for the same `roles` in one transaction: every
    /// field is restricted or none is. Every field and role is validated before
    /// anything is written; duplicates collapse. Each field gets exactly `roles`.
    pub async fn put_many(
        &self,
        actor: &Scope,
        fields: &[(String, String)],
        roles: &[i64],
    ) -> Result<Vec<RestrictionSummary>> {
        let fields: BTreeSet<(String, String)> = fields.iter().cloned().collect();
        let roles: BTreeSet<i64> = roles.iter().copied().collect();
        if fields.is_empty()
            || fields.len() > MAX_FIELDS
            || roles.is_empty()
            || roles.len() > MAX_ROLES
            || roles.iter().any(|id| *id <= 0)
        {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                actor.mutation_context("authz.field_restriction.set"),
            )
            .await?;
        self.administrator(tx.sql(), actor).await?;
        let catalog = crate::field_catalog::for_scope(tx.sql().into(), actor).await?;
        if !fields
            .iter()
            .all(|(table, field)| catalog.restrictable(table, field))
        {
            return Err(Error::Invalid);
        }
        let tenant = actor.tenant();
        let keys: Vec<(String, String)> = fields.iter().cloned().collect();
        let roles: Vec<i64> = roles.into_iter().collect();
        tx.mutate(|sql| {
            Box::pin(async move {
                lock(sql, tenant).await?;
                let known: i64 = sql
                    .query_one(
                        "SELECT count(*) FROM roles WHERE tenant_id=$1 AND id=ANY($2)",
                        &[&tenant, &roles],
                    )
                    .await?
                    .get(0);
                if usize::try_from(known).ok() != Some(roles.len()) {
                    return Err(Error::Invalid);
                }
                let mut changes = Vec::new();
                for (table, field) in &keys {
                    let held: BTreeSet<i64> = sql
                        .query(
                            "SELECT role_id FROM field_restrictions WHERE tenant_id=$1 AND table_id=$2 AND field_id=$3",
                            &[&tenant, table, field],
                        )
                        .await?
                        .iter()
                        .map(|row| row.get(0))
                        .collect();
                    let wanted: BTreeSet<i64> = roles.iter().copied().collect();
                    for role in held.difference(&wanted) {
                        sql.execute(
                            "DELETE FROM field_restrictions WHERE tenant_id=$1 AND role_id=$2 AND table_id=$3 AND field_id=$4",
                            &[&tenant, role, table, field],
                        )
                        .await?;
                        changes.push(change(*role, table, field, true, false));
                    }
                    for role in wanted.difference(&held) {
                        sql.execute(
                            "INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES($1,$2,$3,$4)",
                            &[&tenant, role, table, field],
                        )
                        .await?;
                        changes.push(change(*role, table, field, false, true));
                    }
                }
                Ok(((), changes))
            })
        })
        .await?;
        let summaries = list_in(tx.sql(), tenant, &catalog).await?;
        tx.commit().await?;
        Ok(summaries
            .into_iter()
            .filter(|s| fields.contains(&(s.table_id.clone(), s.field_id.clone())))
            .collect())
    }

    /// Remove every role's restriction of `field` of `table`. A field that is
    /// not restricted in this tenant is `NotFound` and records nothing. A stale
    /// restriction (no longer in the catalog) can still be removed.
    pub async fn remove(&self, actor: &Scope, table: &str, field: &str) -> Result<Removed> {
        if table.is_empty() || table.len() > 200 || field.is_empty() || field.len() > 200 {
            return Err(Error::Invalid);
        }
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                actor.mutation_context("authz.field_restriction.removed"),
            )
            .await?;
        self.administrator(tx.sql(), actor).await?;
        let tenant = actor.tenant();
        let (table, field) = (table.to_owned(), field.to_owned());
        let removed = tx
            .mutate(|sql| {
                Box::pin(async move {
                    lock(sql, tenant).await?;
                    let roles: Vec<i64> = sql
                        .query(
                            "DELETE FROM field_restrictions WHERE tenant_id=$1 AND table_id=$2 AND field_id=$3 RETURNING role_id",
                            &[&tenant, &table, &field],
                        )
                        .await?
                        .iter()
                        .map(|row| row.get(0))
                        .collect();
                    let changes = roles
                        .iter()
                        .map(|role| change(*role, &table, &field, true, false))
                        .collect();
                    Ok((!roles.is_empty(), changes))
                })
            })
            .await?;
        if removed {
            tx.commit().await?;
            Ok(Removed::Removed)
        } else {
            // Nothing changed: no action is recorded for a no-op.
            drop(tx);
            Ok(Removed::NotFound)
        }
    }

    /// The live administrator gate: an authenticated account of the tenant that
    /// holds `admin.authz.field.manage` now (the decision is logged).
    async fn administrator(
        &self,
        client: &(impl GenericClient + Sync),
        actor: &Scope,
    ) -> Result<()> {
        self.authentication.reprove(client, actor).await?;
        authorize_key(client, actor, MANAGE).await
    }
}

fn change(role: i64, table: &str, field: &str, old: bool, new: bool) -> Change {
    Change {
        kind: "authz.field_restriction".into(),
        id: role,
        old: json!({"table_id": table, "field_id": field, "restricted": old}),
        new: json!({"table_id": table, "field_id": field, "restricted": new}),
    }
}

/// Serializes restriction writes per tenant so concurrent role-set
/// replacements of one field cannot interleave.
async fn lock(sql: &Transaction<'_>, tenant: i64) -> Result<()> {
    sql.execute(
        "SELECT pg_advisory_xact_lock(841320,$1::integer)",
        &[&i32::try_from(tenant).map_err(|_| Error::Invalid)?],
    )
    .await?;
    Ok(())
}

async fn list_in(
    client: &impl GenericClient,
    tenant: i64,
    catalog: &FieldCatalog,
) -> Result<Vec<RestrictionSummary>> {
    let rows = client
        .query(
            "SELECT f.table_id,f.field_id,f.role_id,r.name,f.created_at FROM field_restrictions f JOIN roles r ON r.tenant_id=f.tenant_id AND r.id=f.role_id WHERE f.tenant_id=$1 ORDER BY f.table_id,f.field_id,f.role_id",
            &[&tenant],
        )
        .await?;
    let mut grouped: BTreeMap<(String, String), RestrictionSummary> = BTreeMap::new();
    for row in rows {
        let (table, field): (String, String) = (row.get(0), row.get(1));
        let since: SystemTime = row.get(4);
        let summary = grouped
            .entry((table.clone(), field.clone()))
            .or_insert_with(|| {
                let known = catalog.field(&table, &field);
                RestrictionSummary {
                    table_label: catalog
                        .table(&table)
                        .map_or_else(|| table.clone(), |t| t.label.to_owned()),
                    field_label: known.map_or_else(|| field.clone(), |(_, f)| f.label.to_owned()),
                    restrictable: known.is_some(),
                    table_id: table,
                    field_id: field,
                    since,
                    role_ids: Vec::new(),
                    role_names: Vec::new(),
                }
            });
        summary.since = summary.since.min(since);
        summary.role_ids.push(row.get(2));
        summary.role_names.push(row.get(3));
    }
    let mut summaries: Vec<RestrictionSummary> = grouped.into_values().collect();
    // Names read alphabetically; ids stay paired with their names.
    for summary in &mut summaries {
        let mut roles: Vec<(String, i64)> = summary
            .role_names
            .drain(..)
            .zip(summary.role_ids.drain(..))
            .collect();
        roles.sort_by_key(|(name, id)| (name.to_lowercase(), *id));
        (summary.role_names, summary.role_ids) = roles.into_iter().unzip();
    }
    summaries.sort_by(|a, b| {
        (
            a.table_label.as_str(),
            a.field_label.as_str(),
            &a.table_id,
            &a.field_id,
        )
            .cmp(&(
                b.table_label.as_str(),
                b.field_label.as_str(),
                &b.table_id,
                &b.field_id,
            ))
    });
    Ok(summaries)
}
