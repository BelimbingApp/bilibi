//! Static fields contributed to Company by an Extension (Bilimbi
//! `Bilimbi.Core.Company` owns the record; a contributing module owns storage).
use bilibi_authz::field_catalog::{CatalogField, CatalogSource, CatalogTable, FIELD_CATALOG};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{Consumer, Entry, RegistryError};
use bilibi_tenancy::Scope;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    future::Future,
    pin::Pin,
    sync::{Arc, OnceLock},
};
use tokio_postgres::{Client, Transaction};

/// Static field kinds supported by the first Company field contribution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CompanyFieldKind {
    Text {
        max_length: usize,
    },
    Date,
    /// A stable code from a pick-list declared by the contributing module.
    Choice {
        picklist: &'static str,
    },
}

/// Stable metadata supplied by the field's owning Extension.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CompanyField {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: CompanyFieldKind,
    /// The Extension's scalar Grid path, also declared in its RecordFields.
    pub grid_column: &'static str,
}

pub type CompanyFieldValues = BTreeMap<String, Value>;
pub type CompanyFieldReadFuture<'a> =
    Pin<Box<dyn Future<Output = Result<BTreeMap<i64, CompanyFieldValues>>> + Send + 'a>>;
pub type CompanyFieldWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(Value, Value)>> + Send + 'a>>;
pub type CompanyFieldWrite = for<'a, 'tx> fn(
    &'a Transaction<'tx>,
    &'a Scope,
    i64,
    &'a CompanyFieldValues,
) -> CompanyFieldWriteFuture<'a>;

/// One Extension's static field set and owner callbacks. Callbacks receive the
/// caller's sealed scope and, for writes, the same mutation transaction as
/// Company. The contributor owns the tables and SQL behind each callback.
/// Write callbacks must store and report the submitted field values unchanged;
/// Company refuses and rolls back a callback that normalizes them, so reviewed
/// agent input and stored/audited values cannot diverge.
pub struct CompanyFieldContribution {
    pub fields: &'static [CompanyField],
    pub read: for<'a> fn(&'a Client, &'a Scope, &'a [i64]) -> CompanyFieldReadFuture<'a>,
    pub create: CompanyFieldWrite,
    pub update: CompanyFieldWrite,
}

/// The Company-owned registry contract for static Extension fields.
pub const COMPANY_FIELDS: &str = "core/company.company_fields";
/// Keep every valid field value inside the live event frame, including JSON
/// escaping and the surrounding Company form payload.
pub const MAX_TEXT_FIELD_LENGTH: usize = 10_000;
static PICKLISTS: OnceLock<Arc<bilibi_picklists::PickLists>> = OnceLock::new();

pub(crate) fn bind_picklists(
    lists: Arc<bilibi_picklists::PickLists>,
) -> bilibi_module_registry::Result<()> {
    PICKLISTS
        .set(lists)
        .map_err(|_| RegistryError::Service("duplicate Company pick-list binding".into()))
}

/// Names a browser exposes as properties or methods of a form element. A control
/// named like one shadows it on `form.<name>` (`attributes`, `elements`,
/// `submit` ...), which breaks the form object, so no field may use them.
const FORM_ELEMENT_NAMES: &[&str] = &[
    "action",
    "attributes",
    "autocomplete",
    "children",
    "click",
    "elements",
    "encoding",
    "enctype",
    "focus",
    "form",
    "id",
    "length",
    "method",
    "name",
    "reset",
    "style",
    "submit",
    "target",
    "title",
];

/// Validate static definitions before Company starts serving pages or writes.
pub fn validate(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    let builtins: BTreeSet<&str> = crate::RESTRICTABLE
        .into_iter()
        .chain([
            "id",
            "tenant_id",
            "parent_id",
            "name",
            "code",
            "status",
            "legal_name",
            "legal_entity_type_id",
            "registration_number",
            "tax_id",
            "jurisdiction",
            "email",
            "phone",
            "website",
            "revenue",
            "scope_activities",
            "metadata",
            "version",
            // Live-page transport and form control keys (Bilimbi Base UI forms).
            "type",
            "intent",
            "outcome",
            "latitude",
            "longitude",
            "accuracy",
            // Company agent request and response envelope keys.
            "create_anyway",
            "primary",
            "parent_name",
            "path",
        ])
        .collect();
    let mut ids = builtins;
    for entry in entries {
        let contribution = entry.get::<CompanyFieldContribution>()?;
        if contribution.fields.is_empty() {
            return Err(RegistryError::Contribution(format!(
                "Company field contribution {} from {} must declare at least one field",
                entry.id, entry.provenance.module
            )));
        }
        for field in contribution.fields {
            if !valid_id(field.id)
                || field.label.trim().is_empty()
                || FORM_ELEMENT_NAMES.contains(&field.id)
                || !ids.insert(field.id)
                || matches!(field.kind, CompanyFieldKind::Text { max_length: 0 })
            {
                return Err(RegistryError::Contribution(format!(
                    "invalid, required, duplicate, or reserved Company page/agent field {} (reserved: Company's own keys and browser form-element property names such as attributes, elements, submit) from {}",
                    field.id, entry.provenance.module
                )));
            }
            if let CompanyFieldKind::Text { max_length } = field.kind {
                if max_length > MAX_TEXT_FIELD_LENGTH {
                    return Err(RegistryError::Contribution(format!(
                        "Company text field {} from {} exceeds the {MAX_TEXT_FIELD_LENGTH} character live-input bound",
                        field.id, entry.provenance.module,
                    )));
                }
            }
        }
    }
    Ok(())
}

pub(crate) fn contributions(
) -> bilibi_module_registry::Result<Vec<(&'static str, &'static CompanyFieldContribution)>> {
    inventory::iter::<bilibi_module_registry::Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == COMPANY_FIELDS)
        .map(|entry| {
            entry
                .get::<CompanyFieldContribution>()
                .map(|contribution| (entry.provenance.module, contribution))
        })
        .collect()
}

pub(crate) fn catalog_snapshot() -> std::result::Result<Vec<CatalogTable>, String> {
    let contributions = contributions().map_err(|error| error.to_string())?;
    let fields = contributions
        .iter()
        .flat_map(|(_, contribution)| contribution.fields)
        .map(|field| {
            let (table, target, record_types) = resolved_grid_alias(field.grid_column)?;
            Ok(CatalogField {
                id: field.id,
                label: field.label,
                // A company field contributed by an Extension is not marked personal.
                personal: None,
                restriction_aliases: vec![bilibi_authz::field_catalog::RestrictionAlias {
                    table,
                    field: target,
                    record_types,
                }],
            })
        })
        .collect::<std::result::Result<_, String>>()?;
    Ok(vec![CatalogTable {
        id: "companies",
        label: "Companies",
        record_types: &["company"],
        fields,
    }])
}

fn resolved_grid_alias(
    path: &str,
) -> std::result::Result<(&'static str, &'static str, &'static [&'static str]), String> {
    let installed = bilibi_grid::installed().map_err(|error| error.to_string())?;
    let root = installed
        .table("companies")
        .ok_or_else(|| "Company's Grid table is not installed".to_owned())?;
    let column = bilibi_grid::column::resolve(path, root, &|id| installed.table(id))
        .map_err(|error| format!("invalid Company Extension field Grid path {path:?}: {error}"))?;
    if column.kind != bilibi_grid::ColumnKind::Field
        || column.many.is_some()
        || column.agg.is_some()
    {
        return Err(format!(
            "Company Extension field Grid path {path:?} must resolve to a scalar field"
        ));
    }
    let field = column
        .field
        .ok_or_else(|| format!("Company Extension field Grid path {path:?} has no field"))?;
    let target = installed
        .table(column.table)
        .ok_or_else(|| format!("Company Extension field Grid path {path:?} has no target table"))?;
    Ok((column.table, field.id, target.decl.record_types))
}

/// Company field restrictions include the Extension table identity that stores
/// each contributed value. Resolve them from current role grants on every read
/// and mutation (Bilimbi Core Company field policy).
pub(crate) async fn effective_restrictions(
    client: &impl tokio_postgres::GenericClient,
    scope: &Scope,
) -> Result<std::collections::HashSet<String>> {
    let restrictions = bilibi_authz::fields::restricted(client, scope).await?;
    let mut company = restrictions.get("companies").cloned().unwrap_or_default();
    for (_, field) in definitions()? {
        let (table, target, _) =
            resolved_grid_alias(field.grid_column).map_err(|_| Error::Internal)?;
        if restrictions
            .get(table)
            .is_some_and(|fields| fields.contains(target))
        {
            company.insert(field.id.to_owned());
        }
    }
    Ok(company)
}

pub(crate) async fn read(
    client: &Client,
    scope: &Scope,
    ids: &[i64],
) -> Result<BTreeMap<i64, CompanyFieldValues>> {
    let contributions = contributions()?;
    let mut values = BTreeMap::new();
    let requested: BTreeSet<i64> = ids.iter().copied().collect();
    for (_, contribution) in contributions {
        let allowed_fields: BTreeSet<&str> =
            contribution.fields.iter().map(|field| field.id).collect();
        for (company_id, returned_fields) in (contribution.read)(client, scope, ids).await? {
            if !requested.contains(&company_id) {
                return Err(Error::Internal);
            }
            let entry = values.entry(company_id).or_insert_with(BTreeMap::new);
            for (key, value) in returned_fields {
                if !allowed_fields.contains(key.as_str())
                    || !(value.is_string() || value.is_null())
                    || entry.contains_key(&key)
                {
                    return Err(Error::Internal);
                }
                entry.insert(key, value);
            }
        }
    }
    Ok(values)
}

pub(crate) fn definitions() -> Result<Vec<(&'static str, CompanyField)>> {
    Ok(contributions()?
        .into_iter()
        .flat_map(|(module, contribution)| {
            contribution
                .fields
                .iter()
                .copied()
                .map(move |field| (module, field))
        })
        .collect())
}

pub(crate) fn field(id: &str) -> Result<Option<(&'static str, CompanyField)>> {
    Ok(definitions()?.into_iter().find(|(_, field)| field.id == id))
}

pub(crate) fn validate_grid() -> bilibi_module_registry::Result<()> {
    let installed =
        bilibi_grid::installed().map_err(|error| RegistryError::Contribution(error.to_string()))?;
    let root = installed.table("companies").ok_or_else(|| {
        RegistryError::Contribution("Company's Grid table is not installed".into())
    })?;
    for (module, field) in definitions().map_err(|error| {
        RegistryError::Contribution(format!("Company field catalog unavailable: {error}"))
    })? {
        let record = root
            .record_fields()
            .iter()
            .find(|record| record.id == field.id)
            .ok_or_else(|| {
                RegistryError::Contribution(format!(
                    "Company field {} is missing its Grid RecordField contribution",
                    field.id
                ))
            })?;
        if record.label != field.label || record.column != Some(field.grid_column) {
            return Err(RegistryError::Contribution(format!(
                "Company field {} and its Grid RecordField disagree on label or column path",
                field.id
            )));
        }
        let column =
            bilibi_grid::column::resolve(field.grid_column, root, &|id| installed.table(id))
                .map_err(|error| {
                    RegistryError::Contribution(format!(
                        "Company field {} has an invalid Grid path: {error}",
                        field.id
                    ))
                })?;
        let expected = match field.kind {
            CompanyFieldKind::Text { .. } => bilibi_grid::FieldType::String,
            CompanyFieldKind::Date => bilibi_grid::FieldType::Date,
            CompanyFieldKind::Choice { .. } => bilibi_grid::FieldType::String,
        };
        if column.kind != bilibi_grid::ColumnKind::Field
            || column.many.is_some()
            || column.agg.is_some()
            || column.ty != expected
        {
            return Err(RegistryError::Contribution(format!(
                "Company field {} must resolve to a scalar Grid {:?} field",
                field.id, expected
            )));
        }
        let target_owner = installed.table(column.table).ok_or_else(|| {
            RegistryError::Contribution(format!(
                "Company field {} resolves to an unavailable Grid table",
                field.id
            ))
        })?;
        if column.hops.is_empty() || target_owner.owner != module {
            return Err(RegistryError::Contribution(format!(
                "Company field {} must resolve through a link to a scalar field owned by its contributing module {}",
                field.id, module
            )));
        }
    }
    Ok(())
}

fn validate_picklists(
    _services: &bilibi_module_registry::Services,
) -> bilibi_module_registry::Result<()> {
    let declarations = inventory::iter::<bilibi_module_registry::Contribution>
        .into_iter()
        .filter(|entry| entry.consumer == bilibi_picklists::LISTS)
        .collect::<Vec<_>>();
    for (owner, field) in definitions().map_err(|error| {
        RegistryError::Contribution(format!("Company field catalog unavailable: {error}"))
    })? {
        let CompanyFieldKind::Choice { picklist } = field.kind else {
            continue;
        };
        let declared = declarations.iter().any(|entry| {
            entry.provenance.module == owner
                && entry.get::<bilibi_picklists::PickList>().is_ok_and(|list| {
                    list.key == picklist
                        && list.reach == bilibi_picklists::Reach::Tenant
                        && list.parent.is_none()
                })
        });
        if !declared {
            return Err(RegistryError::Contribution(format!(
                "Company choice field {} from {owner} must name a standalone tenant pick-list declared by that Extension: {picklist}",
                field.id
            )));
        }
    }
    Ok(())
}

pub(crate) fn validate_restrictions(
    values: &CompanyFieldValues,
    restricted: &std::collections::HashSet<String>,
) -> Vec<crate::FieldError> {
    let Ok(definitions) = definitions() else {
        return Vec::new();
    };
    values
        .keys()
        .filter(|key| restricted.contains(*key))
        .filter_map(|key| {
            definitions
                .iter()
                .find(|(_, field)| field.id == key)
                .map(|(_, field)| crate::FieldError::new(field.id, "is restricted"))
        })
        .collect()
}

pub(crate) fn validate_create(values: &CompanyFieldValues) -> Vec<crate::FieldError> {
    validate_values_for(values)
}

pub(crate) fn validate_update(values: &CompanyFieldValues) -> Vec<crate::FieldError> {
    validate_values_for(values)
}

pub(crate) async fn validate_choices(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    values: &CompanyFieldValues,
) -> Vec<crate::FieldError> {
    let Some(lists) = PICKLISTS.get() else {
        return vec![crate::FieldError::new(
            "name",
            "pick-list service is unavailable",
        )];
    };
    let mut errors = Vec::new();
    for (_, field) in definitions().unwrap_or_default() {
        let CompanyFieldKind::Choice { picklist } = field.kind else {
            continue;
        };
        let Some(code) = values.get(field.id).and_then(Value::as_str) else {
            continue;
        };
        match lists.check(tx, scope, picklist, code, None).await {
            Ok(Ok(())) => {}
            Ok(Err(refusal)) => errors.push(crate::FieldError::new(field.id, refusal.message())),
            Err(_) => errors.push(crate::FieldError::new(
                field.id,
                "could not validate this value",
            )),
        }
    }
    errors
}

pub(crate) async fn validate_choice_snapshot(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    values: &CompanyFieldValues,
) -> Vec<crate::FieldError> {
    let Some(lists) = PICKLISTS.get() else {
        return vec![crate::FieldError::new(
            "name",
            "pick-list service is unavailable",
        )];
    };
    let mut errors = Vec::new();
    for (_, field) in definitions().unwrap_or_default() {
        let CompanyFieldKind::Choice { picklist } = field.kind else {
            continue;
        };
        let Some(code) = values.get(field.id).and_then(Value::as_str) else {
            continue;
        };
        match lists.choices(client, scope, picklist, None).await {
            Ok(options) if options.iter().any(|option| option.value == code) => {}
            Ok(_) => errors.push(crate::FieldError::new(field.id, "is not an active choice")),
            Err(_) => errors.push(crate::FieldError::new(
                field.id,
                "could not validate this value",
            )),
        }
    }
    errors
}

fn validate_values_for(values: &CompanyFieldValues) -> Vec<crate::FieldError> {
    let Ok(contributions) = contributions() else {
        return vec![crate::FieldError::new(
            "name",
            "field catalog is unavailable",
        )];
    };
    let mut errors = Vec::new();
    let mut known = BTreeSet::new();
    for (_, contribution) in contributions {
        for field in contribution.fields {
            known.insert(field.id);
            let value = values.get(field.id);
            let Some(value) = value else { continue };
            match (field.kind, value) {
                (_, Value::Null) => {}
                (CompanyFieldKind::Text { max_length }, Value::String(text))
                    if text.chars().count() <= max_length => {}
                (CompanyFieldKind::Date, Value::String(text)) if valid_date(text) => {}
                (CompanyFieldKind::Choice { .. }, Value::String(code))
                    if !code.is_empty() && code.len() <= bilibi_picklists::CODE_MAX => {}
                _ => errors.push(crate::FieldError::new(field.id, "has an invalid value")),
            }
        }
    }
    if values.keys().any(|key| !known.contains(key.as_str())) {
        errors.push(crate::FieldError::new("name", "contains an unknown field"));
    }
    errors
}

fn valid_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| index == 4 || index == 7 || byte.is_ascii_digit())
        && value
            .parse::<jiff::civil::Date>()
            .is_ok_and(|date| date.year() > 0)
}

pub(crate) async fn create(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    values: &CompanyFieldValues,
) -> Result<()> {
    for (_, contribution) in contributions()? {
        let fields = only_fields(values, contribution.fields);
        if fields.is_empty() {
            continue;
        }
        let scope = scope.clone();
        let old = Value::Null;
        let callback = contribution.create;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let (_, new) = callback(sql, &scope, company_id, &fields).await?;
                if !change_matches_values(&new, &fields) {
                    return Err(Error::Internal);
                }
                Ok((
                    (),
                    vec![Change {
                        kind: "company".into(),
                        id: company_id,
                        old,
                        new,
                    }],
                ))
            })
        })
        .await?;
    }
    Ok(())
}

pub(crate) async fn update(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    company_id: i64,
    version: i64,
    values: &CompanyFieldValues,
) -> Result<()> {
    let mut writes = Vec::new();
    for (_, contribution) in contributions()? {
        let fields = only_fields(values, contribution.fields);
        if !fields.is_empty() {
            writes.push((contribution.update, fields));
        }
    }
    if writes.is_empty() {
        return Ok(());
    }
    let picklists = PICKLISTS.get().cloned().ok_or(Error::Internal)?;
    let scope = scope.clone();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row = sql.query_opt(
                "SELECT version,status FROM companies WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE",
                &[&scope.tenant(), &company_id],
            ).await?.ok_or(Error::NotFound)?;
            if row.get::<_, i64>(0) != version { return Err(Error::Conflict); }
            if row.get::<_, String>(1) == "archived" { return Err(Error::Forbidden); }
            let updated = sql.query_one(
                "UPDATE companies SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 RETURNING version",
                &[&scope.tenant(), &company_id],
            ).await?.get::<_, i64>(0);
            let mut old = serde_json::Map::new();
            let mut new = serde_json::Map::new();
            for (callback, fields) in writes {
                let (before, after) = callback(sql, &scope, company_id, &fields).await?;
                let before = before.as_object().ok_or(Error::Internal)?;
                let after = after.as_object().ok_or(Error::Internal)?;
                if before.keys().any(|key| !fields.contains_key(key))
                    || !change_matches_values(&Value::Object(after.clone()), &fields)
                {
                    return Err(Error::Internal);
                }
                for key in fields.keys() {
                    let Some((_, definition)) = field(key)? else { return Err(Error::Internal) };
                    let CompanyFieldKind::Choice { picklist } = definition.kind else { continue };
                    let previous = before.get(key).filter(|value| !value.is_null());
                    let proposed = after.get(key).filter(|value| !value.is_null());
                    if previous == proposed { continue; }
                    if let Some(code) = proposed.and_then(Value::as_str) {
                        if picklists.check_in(sql, &scope, picklist, code, None).await?.is_err() {
                            return Err(Error::Invalid);
                        }
                    }
                }
                old.extend(before.clone());
                new.extend(after.clone());
            }
            old.insert("version".into(), Value::from(version));
            new.insert("version".into(), Value::from(updated));
            Ok(((), vec![Change { kind: "company".into(), id: company_id, old: Value::Object(old), new: Value::Object(new) }]))
        })
    }).await?;
    Ok(())
}

fn change_matches_values(change: &Value, fields: &CompanyFieldValues) -> bool {
    let Some(object) = change.as_object() else {
        return false;
    };
    object.len() == fields.len()
        && fields
            .iter()
            .all(|(field, value)| object.get(field) == Some(value))
}

fn only_fields(values: &CompanyFieldValues, fields: &[CompanyField]) -> CompanyFieldValues {
    fields
        .iter()
        .filter_map(|field| {
            values
                .get(field.id)
                .map(|value| (field.id.to_owned(), value.clone()))
        })
        .collect()
}

fn valid_id(value: &str) -> bool {
    let mut bytes = value.bytes();
    value.len() <= 48
        && bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
        && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
}

inventory::submit! {
    Consumer::new::<CompanyFieldContribution>(COMPANY_FIELDS, "core/company", validate)
}
inventory::submit! {
    bilibi_module_registry::Ready {
        module: "core/company",
        check: |services| {
            validate_picklists(services)?;
            validate_grid()
        },
    }
}
inventory::submit! {
    bilibi_module_registry::Contribution::new(
        FIELD_CATALOG,
        "core/company.company_fields",
        bilibi_module_registry::Provenance { module: "core/company", source: file!(), line: line!() },
        &CatalogSource { snapshot: catalog_snapshot },
    )
}
