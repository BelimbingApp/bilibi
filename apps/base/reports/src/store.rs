//! The report tables. Every statement binds its values and carries the tenant
//! predicate. Who may read a report is one SQL condition, [`VISIBLE`]: its
//! author, everyone in the tenant when shared that way, or a holder of a
//! role it is shared with (Authz's `effective_principal_roles`, as Tiling's
//! shared layouts read it).
use crate::definition::{Definition, Kind};
use bilibi_database::{Error, Result};
use serde::Serialize;
use serde_json::Value;
use tokio_postgres::{GenericClient, Row};

/// `$1` tenant, `$2` reader; the report row is `r`.
const VISIBLE: &str = "r.tenant_id=$1 AND (r.owner_id=$2 OR r.shared_with_everyone OR EXISTS (SELECT 1 FROM base_report_shares s JOIN effective_principal_roles p ON p.tenant_id=s.tenant_id AND p.role_id=s.role_id WHERE s.tenant_id=r.tenant_id AND s.report_id=r.id AND p.user_id=$2))";
const COLUMNS: &str = "r.id, r.owner_id, r.name, r.description, r.record_type, r.kind, r.definition, r.shared_with_everyone, r.version, to_char(r.updated_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), COALESCE((SELECT array_agg(s.role_id ORDER BY s.role_id) FROM base_report_shares s WHERE s.tenant_id=r.tenant_id AND s.report_id=r.id), '{}')";

/// A stored report as its reader sees it.
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub id: i64,
    pub owner_id: i64,
    /// The reader wrote it, so may change, share or delete it.
    pub owned: bool,
    pub name: String,
    pub description: String,
    pub record_type: String,
    pub kind: Kind,
    pub definition: Definition,
    pub shared_with_everyone: bool,
    pub role_ids: Vec<i64>,
    pub version: i64,
    pub updated_at: String,
    /// A built-in report's key; `None` for a stored one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub builtin: Option<&'static str>,
}

fn report(row: &Row, reader: i64) -> Result<Report> {
    let definition: Value = row.get(6);
    let kind: String = row.get(5);
    Ok(Report {
        id: row.get(0),
        owner_id: row.get(1),
        owned: row.get::<_, i64>(1) == reader,
        name: row.get(2),
        description: row.get(3),
        record_type: row.get(4),
        kind: Kind::from_key(&kind).ok_or(Error::Internal)?,
        definition: serde_json::from_value(definition).map_err(|_| Error::Internal)?,
        shared_with_everyone: row.get(7),
        version: row.get(8),
        updated_at: row.get(9),
        role_ids: row.get(10),
        builtin: None,
    })
}

/// One page of the reports the reader may run, by name, whose name or
/// description holds `search` (any case; empty matches all), and how many
/// match in all: the search and the page are one query, so no report is
/// out of reach behind a cutoff.
pub async fn page(
    sql: &impl GenericClient,
    tenant: i64,
    reader: i64,
    search: &str,
    offset: i64,
    limit: i64,
) -> Result<(Vec<Report>, i64)> {
    const MATCH: &str = "($3 = '' OR position(lower($3) in lower(r.name)) > 0 OR position(lower($3) in lower(r.description)) > 0)";
    let total: i64 = sql
        .query_one(
            &format!("SELECT count(*) FROM base_reports r WHERE {VISIBLE} AND {MATCH}"),
            &[&tenant, &reader, &search],
        )
        .await?
        .get(0);
    let rows = sql
        .query(
            &format!("SELECT {COLUMNS} FROM base_reports r WHERE {VISIBLE} AND {MATCH} ORDER BY lower(r.name), r.id OFFSET $4 LIMIT $5"),
            &[&tenant, &reader, &search, &offset, &limit],
        )
        .await?;
    let reports = rows
        .iter()
        .map(|row| report(row, reader))
        .collect::<Result<_>>()?;
    Ok((reports, total))
}

/// Placed dashboard reports plus a bounded offer list, using the same visibility
/// predicate as every other report read and without one query per pin.
pub async fn dashboard(
    sql: &impl GenericClient,
    tenant: i64,
    reader: i64,
    placed: &[String],
    offers: bool,
) -> Result<Vec<Report>> {
    let ids: Vec<i64> = placed.iter().filter_map(|id| id.parse().ok()).collect();
    let limit = if offers {
        bilibi_dashboard::MAX_PROVIDED as i64
    } else {
        0
    };
    sql.query(
        &format!("SELECT {COLUMNS} FROM base_reports r WHERE {VISIBLE} AND (r.id=ANY($3) OR r.id IN (SELECT r.id FROM base_reports r WHERE {VISIBLE} AND NOT (r.id=ANY($3)) ORDER BY lower(r.name),r.id LIMIT $4)) ORDER BY lower(r.name),r.id"),
        &[&tenant, &reader, &ids, &limit],
    ).await?.iter().map(|row| report(row, reader)).collect()
}

/// One report the reader may run; `lock` holds it for a write.
pub async fn read(
    sql: &impl GenericClient,
    tenant: i64,
    reader: i64,
    id: i64,
    lock: bool,
) -> Result<Report> {
    let lock = if lock { " FOR UPDATE OF r" } else { "" };
    let row = sql
        .query_opt(
            &format!("SELECT {COLUMNS} FROM base_reports r WHERE {VISIBLE} AND r.id=$3{lock}"),
            &[&tenant, &reader, &id],
        )
        .await?
        .ok_or(Error::NotFound)?;
    report(&row, reader)
}

pub struct Write<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub record_type: &'a str,
    pub kind: Kind,
    pub definition: &'a Definition,
}

pub async fn insert(
    sql: &impl GenericClient,
    tenant: i64,
    owner: i64,
    write: &Write<'_>,
) -> Result<i64> {
    let definition = serde_json::to_value(write.definition).map_err(|_| Error::Internal)?;
    Ok(sql
        .query_one(
            "INSERT INTO base_reports(tenant_id,owner_id,name,description,record_type,kind,definition) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",
            &[&tenant, &owner, &write.name, &write.description, &write.record_type, &write.kind.key(), &definition],
        )
        .await?
        .get(0))
}

/// Replaces the definition when `version` still matches; `false` when stale.
pub async fn update(
    sql: &impl GenericClient,
    tenant: i64,
    id: i64,
    version: i64,
    write: &Write<'_>,
) -> Result<bool> {
    let definition = serde_json::to_value(write.definition).map_err(|_| Error::Internal)?;
    Ok(sql
        .execute(
            "UPDATE base_reports SET name=$4,description=$5,record_type=$6,kind=$7,definition=$8,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND version=$3",
            &[&tenant, &id, &version, &write.name, &write.description, &write.record_type, &write.kind.key(), &definition],
        )
        .await?
        == 1)
}

/// Replaces who the report is shared with when `version` still matches.
/// Role ids outside the tenant are refused by the foreign key.
pub async fn share(
    sql: &impl GenericClient,
    tenant: i64,
    id: i64,
    version: i64,
    everyone: bool,
    roles: &[i64],
) -> Result<bool> {
    let touched = sql
        .execute(
            "UPDATE base_reports SET shared_with_everyone=$4,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND version=$3",
            &[&tenant, &id, &version, &everyone],
        )
        .await?;
    if touched != 1 {
        return Ok(false);
    }
    sql.execute(
        "DELETE FROM base_report_shares WHERE tenant_id=$1 AND report_id=$2",
        &[&tenant, &id],
    )
    .await?;
    sql.execute(
        "INSERT INTO base_report_shares(tenant_id,report_id,role_id) SELECT $1,$2,role FROM unnest($3::bigint[]) role",
        &[&tenant, &id, &roles],
    )
    .await?;
    Ok(true)
}

pub async fn delete(sql: &impl GenericClient, tenant: i64, id: i64, version: i64) -> Result<bool> {
    Ok(sql
        .execute(
            "DELETE FROM base_reports WHERE tenant_id=$1 AND id=$2 AND version=$3",
            &[&tenant, &id, &version],
        )
        .await?
        == 1)
}

/// Which of `roles` exist in the tenant, so a share names only real roles.
pub async fn known_roles(sql: &impl GenericClient, tenant: i64, roles: &[i64]) -> Result<Vec<i64>> {
    Ok(sql
        .query(
            "SELECT id FROM roles WHERE tenant_id=$1 AND id=ANY($2) ORDER BY id",
            &[&tenant, &roles],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// The tenant's roles, by name, for choosing whom to share with.
pub async fn roles(sql: &impl GenericClient, tenant: i64) -> Result<Vec<(i64, String)>> {
    Ok(sql
        .query(
            "SELECT id, name FROM roles WHERE tenant_id=$1 ORDER BY lower(name), id LIMIT 500",
            &[&tenant],
        )
        .await?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}
