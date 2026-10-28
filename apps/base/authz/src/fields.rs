//! Generalized field withholding, independent of Grid and presentation. Never
//! serialize an original value beside a restricted placeholder.
//! Mirrors Bilimbi `Base.Authz` field withholding.
use bilibi_database::{Error, Result};
use bilibi_tenancy::{ReadClient, Scope};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};
use tokio_postgres::GenericClient;

/// All restrictions for an actor, in one fresh database read. Any restricting
/// role wins, including alongside an admin/grant_all role. A named principal
/// holds no roles, so every restriction in its tenant applies to it.
pub async fn restricted(
    client: &impl GenericClient,
    scope: &Scope,
) -> Result<BTreeMap<String, HashSet<String>>> {
    let rows = if scope.principal().is_some() {
        client
            .query(SYSTEM_RESTRICTIONS, &[&scope.tenant()])
            .await?
    } else {
        client
            .query(USER_RESTRICTIONS, &[&scope.tenant(), &scope.user()])
            .await?
    };
    Ok(restriction_rows(rows))
}
/// The same live restrictions on a reviewed operation's read connection.
/// Queries stay on the caller's connection, including an uncommitted transaction.
pub async fn restricted_read(
    client: &dyn ReadClient,
    scope: &Scope,
) -> Result<BTreeMap<String, HashSet<String>>> {
    let rows = if scope.principal().is_some() {
        client
            .query(SYSTEM_RESTRICTIONS, &[&scope.tenant()])
            .await?
    } else {
        client
            .query(USER_RESTRICTIONS, &[&scope.tenant(), &scope.user()])
            .await?
    };
    Ok(restriction_rows(rows))
}

const SYSTEM_RESTRICTIONS: &str =
    "SELECT DISTINCT table_id,field_id FROM field_restrictions WHERE tenant_id=$1";
const USER_RESTRICTIONS: &str = "SELECT DISTINCT f.table_id,f.field_id FROM field_restrictions f JOIN effective_principal_roles p ON p.tenant_id=f.tenant_id AND p.role_id=f.role_id WHERE f.tenant_id=$1 AND p.user_id=$2";
fn restriction_rows(rows: Vec<tokio_postgres::Row>) -> BTreeMap<String, HashSet<String>> {
    let mut result: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for row in rows {
        result.entry(row.get(0)).or_default().insert(row.get(1));
    }
    result
}
/// The fields of each audit record type the scope may not see, keyed by the
/// names a recorded value uses (Bilimbi `Authz.withheld_fields_by_type/2`). A
/// restriction on either identity of one catalog field withholds both names
/// under every audit record type declared by those identities. A type with
/// nothing withheld is absent; an identity outside the catalog reaches no
/// audit kind.
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
    let catalog = crate::field_catalog::field_catalog()?;
    for table in catalog.tables() {
        for kind in table
            .record_types
            .iter()
            .filter(|kind| types.contains(kind))
        {
            let withheld = result.entry((*kind).to_owned()).or_default();
            withheld.extend(restricted.get(table.id).into_iter().flatten().cloned());
        }
        for field in &table.fields {
            let owner_restricted = restricted
                .get(table.id)
                .is_some_and(|fields| fields.contains(field.id));
            let alias_restricted = field.restriction_aliases.iter().any(|alias| {
                restricted
                    .get(alias.table)
                    .is_some_and(|fields| fields.contains(alias.field))
            });
            if !owner_restricted && !alias_restricted {
                continue;
            }
            for kind in table
                .record_types
                .iter()
                .filter(|kind| types.contains(kind))
            {
                result
                    .entry((*kind).to_owned())
                    .or_default()
                    .insert(field.id.to_owned());
            }
            for alias in &field.restriction_aliases {
                for kind in alias
                    .record_types
                    .iter()
                    .filter(|kind| types.contains(kind))
                {
                    result
                        .entry((*kind).to_owned())
                        .or_default()
                        .insert(alias.field.to_owned());
                }
            }
        }
    }
    result.retain(|_, fields| !fields.is_empty());
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
