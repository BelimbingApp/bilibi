//! Generalized field withholding, independent of Grid and presentation. Never
//! serialize an original value beside a restricted placeholder.
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
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
    if scope.principal().is_some() {
        let rows = client
            .query(
                "SELECT DISTINCT table_id,field_id FROM field_restrictions WHERE tenant_id=$1",
                &[&scope.tenant()],
            )
            .await?;
        let mut result: BTreeMap<String, HashSet<String>> = BTreeMap::new();
        for row in rows {
            result.entry(row.get(0)).or_default().insert(row.get(1));
        }
        return Ok(result);
    }
    let rows=client.query("SELECT DISTINCT f.table_id,f.field_id FROM field_restrictions f JOIN effective_principal_roles p ON p.tenant_id=f.tenant_id AND p.role_id=f.role_id WHERE f.tenant_id=$1 AND p.user_id=$2",&[&scope.tenant(),&scope.user()]).await?;
    let mut result: BTreeMap<String, HashSet<String>> = BTreeMap::new();
    for row in rows {
        result.entry(row.get(0)).or_default().insert(row.get(1));
    }
    Ok(result)
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
/// old/new values, or API projection. Presentational markers belong to UI. To
/// show masked fields as partial values instead, call [`apply_masks`] first and
/// remove only the fields left that it does not mask.
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

/// What a masked field shows instead of its value (ADR 05), by its personal
/// kind: `a•••@example.com`, `•••• 1234`, the year of a date, initials of a
/// name, the last four characters of an identifier, and nothing of free text.
/// An empty value stays empty; nothing of the original is shown beyond this.
pub fn partial(kind: &str, value: &str) -> String {
    let value = value.trim();
    if value.is_empty() {
        return String::new();
    }
    let last = |count: usize| {
        let chars: Vec<char> = value.chars().filter(|c| !c.is_whitespace()).collect();
        let tail: String = chars[chars.len().saturating_sub(count)..].iter().collect();
        if chars.len() <= count {
            "••••".to_owned()
        } else {
            format!("•••• {tail}")
        }
    };
    match kind {
        "email" => match value.split_once('@') {
            Some((local, domain)) if !local.is_empty() && !domain.is_empty() => {
                let first: String = local.chars().take(1).collect();
                format!("{first}•••@{domain}")
            }
            _ => "•••".to_owned(),
        },
        "phone" => {
            let digits: String = value.chars().filter(char::is_ascii_digit).collect();
            if digits.len() <= 4 {
                "••••".to_owned()
            } else {
                format!("•••• {}", &digits[digits.len() - 4..])
            }
        }
        "date" => value
            .get(..4)
            .filter(|year| year.bytes().all(|b| b.is_ascii_digit()))
            .map_or_else(|| "•••".to_owned(), str::to_owned),
        "name" => value
            .split_whitespace()
            .map(|word| format!("{}•••", word.chars().take(1).collect::<String>()))
            .collect::<Vec<_>>()
            .join(" "),
        "identifier" => last(4),
        _ => "•••".to_owned(),
    }
}

/// The fields of `table` this scope sees masked, with each one's personal
/// kind: restricted in mask mode for a role it holds and withheld by none,
/// and marked personal in the catalog. Every masked field is also in
/// [`crate::restrictions_for`], so a caller that only removes restricted
/// fields still never shows a masked value in full.
pub async fn masks_for(
    client: &impl GenericClient,
    scope: &Scope,
    table: &str,
) -> Result<BTreeMap<String, &'static str>> {
    Ok(masked(client, scope)
        .await?
        .remove(table)
        .unwrap_or_default())
}
/// [`masks_for`] for every table, in one read.
pub async fn masked(
    client: &impl GenericClient,
    scope: &Scope,
) -> Result<BTreeMap<String, BTreeMap<String, &'static str>>> {
    let rows = client
        .query(
            "SELECT f.table_id,f.field_id FROM field_restrictions f JOIN effective_principal_roles p ON p.tenant_id=f.tenant_id AND p.role_id=f.role_id \
             WHERE f.tenant_id=$1 AND p.user_id=$2 GROUP BY f.table_id,f.field_id HAVING bool_and(f.mode='mask')",
            &[&scope.tenant(), &scope.user()],
        )
        .await?;
    let catalog = crate::field_catalog::field_catalog()?;
    let mut result: BTreeMap<String, BTreeMap<String, &'static str>> = BTreeMap::new();
    for row in rows {
        let (table, field): (String, String) = (row.get(0), row.get(1));
        // A field no longer marked personal falls back to withholding.
        if let Some(kind) = catalog.field(&table, &field).and_then(|(_, f)| f.personal) {
            result.entry(table).or_default().insert(field, kind);
        }
    }
    Ok(result)
}
/// [`withheld_by_record_type`] for masked fields: by audit record type, the
/// fields its values show as partial, with their kinds.
pub async fn masked_by_record_type(
    client: &impl GenericClient,
    scope: &Scope,
    types: &[&str],
) -> Result<BTreeMap<String, BTreeMap<String, &'static str>>> {
    let masked = masked(client, scope).await?;
    let mut result: BTreeMap<String, BTreeMap<String, &'static str>> = BTreeMap::new();
    if masked.is_empty() || types.is_empty() {
        return Ok(result);
    }
    for table in crate::field_catalog::field_catalog()?.tables() {
        let Some(fields) = masked.get(table.id) else {
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
                .extend(fields.iter().map(|(field, k)| (field.clone(), *k)));
        }
    }
    Ok(result)
}
/// Replace masked fields' string values with their partial form, in a record,
/// an array of records, audit old/new values or an API projection. A masked
/// field holding anything but a string or null is removed.
pub fn apply_masks(value: &mut Value, masks: &BTreeMap<String, &'static str>) {
    match value {
        Value::Object(object) => {
            for (field, kind) in masks {
                match object.get(field) {
                    Some(Value::String(text)) => {
                        let shown = partial(kind, text);
                        object.insert(field.clone(), Value::String(shown));
                    }
                    Some(Value::Null) | None => {}
                    Some(_) => {
                        object.remove(field);
                    }
                }
            }
        }
        Value::Array(rows) => {
            for row in rows {
                apply_masks(row, masks);
            }
        }
        _ => {}
    }
}
