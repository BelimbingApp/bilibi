//! Generalized field withholding, independent of Grid and presentation. Never
//! serialize an original value beside a restricted placeholder.
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use tokio_postgres::GenericClient;

/// All restrictions for an actor, in one fresh database read. Any restricting
/// role wins, including alongside an admin/grant_all role.
pub async fn restricted(
    client: &impl GenericClient,
    scope: &Scope,
) -> Result<BTreeMap<String, HashSet<String>>> {
    let rows=client.query("SELECT DISTINCT f.table_id,f.field_id FROM field_restrictions f JOIN effective_principal_roles p ON p.tenant_id=f.tenant_id AND p.role_id=f.role_id WHERE f.tenant_id=$1 AND p.user_id=$2",&[&scope.tenant(),&scope.user()]).await?;
    let mut result: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for row in rows {
        result.entry(row.get(0)).or_default().insert(row.get(1));
    }
    Ok(result)
}
/// The fields of each audit record type the scope may not see, keyed by the
/// names a recorded value uses (Bilimbi `Authz.withheld_fields_by_type/2`). A
/// restriction is declared on a catalog table; the table's `record_types` are
/// the audit kinds its rows are recorded under, so the audit screens withhold
/// what the record's own page withholds. A type with nothing withheld is
/// absent; a table outside the catalog reaches no audit kind.
pub async fn withheld_by_record_type(
    client: &impl GenericClient,
    scope: &Scope,
    types: &[&str],
) -> Result<BTreeMap<String, HashSet<String>>> {
    let restricted = restricted(client, scope).await?;
    let mut result: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    if restricted.is_empty() || types.is_empty() {
        return Ok(result);
    }
    for table in crate::field_catalog::field_catalog()?.tables() {
        let Some(fields) = restricted.get(table.id).filter(|fields| !fields.is_empty()) else {
            continue;
        };
        for kind in table
            .record_types
            .iter()
            .filter(|kind| types.contains(kind))
        {
            result
                .entry((*kind).to_owned())
                .or_default()
                .extend(fields.iter().cloned());
        }
    }
    Ok(result)
}
/// Remove withheld fields before serializing a record, array of records, audit
/// old/new values, or API projection. Presentational markers belong to UI.
pub fn mask(value: &mut Value, fields: &HashSet<String>) {
    match value {
        Value::Object(object) => {
            for field in fields {
                object.remove(field);
            }
        }
        Value::Array(rows) => {
            for row in rows {
                mask(row, fields);
            }
        }
        _ => {}
    }
}
/// Refuse every attempt to supply a restricted field, including null, unchanged
/// values, and guesses. Validation does not depend on the stored value.
pub fn refuse_attempts(value: &Value, fields: &HashSet<String>) -> Result<()> {
    let object = value.as_object().ok_or(Error::Invalid)?;
    if object.keys().any(|key| fields.contains(key)) {
        Err(Error::Forbidden)
    } else {
        Ok(())
    }
}
pub async fn mask_for(
    client: &impl GenericClient,
    scope: &Scope,
    table: &str,
    value: &mut Value,
) -> Result<()> {
    mask(value, &crate::restrictions_for(client, scope, table).await?);
    Ok(())
}
pub async fn refuse_for(
    client: &impl GenericClient,
    scope: &Scope,
    table: &str,
    value: &Value,
) -> Result<()> {
    refuse_attempts(value, &crate::restrictions_for(client, scope, table).await?)
}
