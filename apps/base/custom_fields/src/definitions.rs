//! Tenant definition service, mirroring Bilimbi `CustomFields` and its owner
//! validation calls. Definitions are data; owners keep record values.
use crate::{
    model::{registered, Definition, FieldType, RecordType, RECORD_TYPES},
    references::{self, ReferenceOption, ReferenceTargetSourceRef, ReferenceTargetType},
};
use bilibi_authz::{authorize_key, restrictions_for};
use bilibi_database::{Change, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Catalog, RegistryError};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::{Authentication, ReadClient, Scope};
use serde_json::{json, Map, Value};
use std::{collections::BTreeMap, str::FromStr, sync::Arc};
use tokio_postgres::GenericClient;

pub const MANAGE: &str = "admin.custom.fields.manage";
const MAX_LABEL: usize = 120;
const MAX_HELP: usize = 500;
const DEFAULT_MAX_TEXT_LENGTH: usize = 100_000;

#[derive(Clone, Copy)]
pub struct Limits {
    pub max_fields_per_record_type: usize,
    pub max_text_length: usize,
    pub max_json_bytes: usize,
}

#[derive(Clone, Debug)]
pub struct FieldError {
    pub field: String,
    pub message: String,
}
impl FieldError {
    fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            field: field.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct FieldInput {
    pub key: String,
    pub label: String,
    pub field_type: FieldType,
    pub required: bool,
    pub help_text: Option<String>,
    pub position: i32,
    pub options: Value,
    pub reference_table: Option<String>,
    pub text_limit: Option<i32>,
    pub format: Option<String>,
}

#[derive(Clone, Debug)]
pub enum Outcome<T> {
    Saved(T),
    Refused(Vec<FieldError>),
    NotFound,
    Stale,
}

pub struct ServiceError;

pub struct CustomFields {
    db: Db,
    boundary: Arc<MutationBoundary>,
    authentication: Arc<Authentication>,
    settings: Arc<Settings>,
    record_types: Vec<RecordType>,
    reference_targets: Vec<ReferenceTargetSourceRef>,
}
impl CustomFields {
    pub fn new(
        db: Db,
        boundary: Arc<MutationBoundary>,
        authentication: Arc<Authentication>,
        settings: Arc<Settings>,
    ) -> std::result::Result<Self, RegistryError> {
        let modules = bilibi_module_registry::discover()?;
        let catalog = Catalog::discover(&modules)?;
        let record_types = registered(catalog.entries::<RecordType>(RECORD_TYPES)?)?;
        if record_types.is_empty() {
            return Err(RegistryError::Contribution(
                "base/custom_fields requires at least one opted-in record type".into(),
            ));
        }
        let reference_targets = references::providers()?;
        for target in &reference_targets {
            if !record_types
                .iter()
                .any(|record_type| record_type.grid_table == target.table)
            {
                return Err(RegistryError::Contribution(format!(
                    "reference target {} is not an opted-in record type",
                    target.table
                )));
            }
        }
        Ok(Self {
            db,
            boundary,
            authentication,
            settings,
            record_types,
            reference_targets,
        })
    }

    pub fn record_types(&self) -> &[RecordType] {
        &self.record_types
    }

    pub fn reference_target_types(&self) -> Vec<ReferenceTargetType> {
        self.reference_targets
            .iter()
            .filter_map(|target| {
                self.record_types
                    .iter()
                    .find(|record_type| record_type.grid_table == target.table)
                    .map(|record_type| ReferenceTargetType {
                        table: target.table.into(),
                        label: record_type.label.into(),
                    })
            })
            .collect()
    }

    /// Readable targets for a definition's opted-in record type, loaded fresh
    /// for this scope. Empty is safe when the actor cannot read any targets.
    pub async fn reference_choices(
        &self,
        scope: &Scope,
        table: &str,
    ) -> Result<Vec<ReferenceOption>> {
        let client = self.db.get().await?;
        self.authentication.reprove(&**client, scope).await?;
        let target = self
            .reference_targets
            .iter()
            .find(|target| target.table == table)
            .ok_or(Error::Invalid)?;
        let choices = (target.provider.choices(&**client, scope)).await?;
        let mut seen = std::collections::HashSet::new();
        if choices.iter().any(|choice| {
            choice.key.is_empty()
                || choice.key.len() > 200
                || choice.label.trim().is_empty()
                || !seen.insert(choice.key.as_str())
        }) {
            return Err(Error::Internal);
        }
        Ok(choices)
    }

    /// A target name only when the current actor may read the live target.
    /// Missing, archived, cross-tenant and unreadable targets share `None`.
    pub async fn reference_label(
        &self,
        scope: &Scope,
        table: &str,
        key: &str,
    ) -> Result<Option<String>> {
        let client = self.db.get().await?;
        self.authentication.reprove(&**client, scope).await?;
        let target = self
            .reference_targets
            .iter()
            .find(|target| target.table == table)
            .ok_or(Error::Invalid)?;
        (target.provider.resolve(&**client, scope, key)).await
    }

    /// Whether the current tenant actor may define fields for this opted-in
    /// record type. The page uses this only to hide unavailable write controls;
    /// every definition mutation still rechecks the capability in its transaction.
    pub async fn can_manage_record_type(&self, scope: &Scope, record_type: &str) -> Result<bool> {
        let decl = self.require_record_type(record_type)?;
        let client = self.db.get().await?;
        self.administrator(&**client, scope).await?;
        Ok(bilibi_authz::effective_capabilities(&**client, scope)
            .await?
            .contains(decl.capability))
    }

    pub async fn list(
        &self,
        scope: &Scope,
        record_type: &str,
        archived: bool,
    ) -> Result<Vec<Definition>> {
        let client = self.db.get().await?;
        self.administrator(&**client, scope).await?;
        self.require_record_type(record_type)?;
        definitions(&**client, scope.tenant(), record_type, archived).await
    }

    /// Active definitions for an opted-in record owner's already-authorized
    /// page. Unlike administration listing, this does not require manage
    /// permission; the owner route authorizes the record operation first.
    pub async fn active_for_record(
        &self,
        scope: &Scope,
        record_type: &str,
    ) -> Result<Vec<Definition>> {
        let client = self.db.get().await?;
        self.authentication.reprove(&**client, scope).await?;
        self.require_record_type(record_type)?;
        definitions(&**client, scope.tenant(), record_type, false).await
    }

    /// Active definitions on the caller's existing read transaction. The
    /// owner must authorize the corresponding record operation before calling;
    /// this is used by Agent API scoped input schemas and plan revalidation.
    pub async fn active_for_record_on(
        &self,
        client: &(impl ReadClient + ?Sized),
        scope: &Scope,
        record_type: &str,
    ) -> Result<Vec<Definition>> {
        self.require_record_type(record_type)?;
        definitions_on_read(client, scope.tenant(), record_type).await
    }

    /// Tenant limits for a scoped contract or other owner read. Values are
    /// resolved through Settings on the caller's current transaction.
    pub async fn limits_on(&self, client: &dyn ReadClient, scope: &Scope) -> Result<Limits> {
        self.limits(client, scope).await
    }

    pub async fn define(
        &self,
        scope: &Scope,
        record_type: &str,
        input: &FieldInput,
    ) -> Result<Outcome<Definition>> {
        let decl = self.require_record_type(record_type)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("custom_fields.define"))
            .await?;
        self.administrator(tx.sql(), scope).await?;
        authorize_key(tx.sql(), scope, decl.capability).await?;
        lock_definition_set(tx.sql(), scope.tenant(), record_type).await?;
        let limits = self.limits(tx.sql(), scope).await?;
        if let Some(errors) = validate_definition_input(input, limits.max_text_length) {
            drop(tx);
            return Ok(Outcome::Refused(errors));
        }
        if !self.valid_reference_type(input) {
            drop(tx);
            return Ok(Outcome::Refused(vec![FieldError::new(
                "reference_table",
                "must name an opted-in record type with readable targets",
            )]));
        }
        let input = input.clone();
        let record_type = record_type.to_owned();
        let tenant = scope.tenant();
        let existing: bool = tx.sql().query_one(
            "SELECT EXISTS(SELECT 1 FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND field_key=$3)",
            &[&tenant, &record_type, &input.key],
        ).await?.get(0);
        if existing {
            drop(tx);
            return Ok(Outcome::Refused(vec![FieldError::new(
                "key",
                "is already in use",
            )]));
        }
        let active: i64 = tx.sql().query_one(
            "SELECT count(*) FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND archived_at IS NULL",
            &[&tenant, &record_type],
        ).await?.get(0);
        if active >= limits.max_fields_per_record_type as i64 {
            drop(tx);
            return Ok(Outcome::Refused(vec![FieldError::new(
                "key",
                format!(
                    "cannot be added because the active field limit of {} has been reached",
                    limits.max_fields_per_record_type
                ),
            )]));
        }
        let result = tx.mutate(move |sql| Box::pin(async move {
            let row = sql.query_one("INSERT INTO base_custom_field_definitions(tenant_id,record_type,field_key,label,field_type,required,help_text,position,options,reference_table,text_limit,format) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING *", &[&tenant,&record_type,&input.key,&input.label,&input.field_type.as_str(),&input.required,&input.help_text,&input.position,&input.options,&input.reference_table,&input.text_limit,&input.format]).await?;
            let def = from_row(&row);
            Ok((def.clone(), vec![Change { kind:"custom_field_definition".into(), id:def.id, old:Value::Null, new:to_json(&def) }]))
        })).await;
        match result {
            Ok(def) => {
                tx.commit().await?;
                Ok(Outcome::Saved(def))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn update(
        &self,
        scope: &Scope,
        record_type: &str,
        key: &str,
        version: i64,
        input: &FieldInput,
    ) -> Result<Outcome<Definition>> {
        let decl = self.require_record_type(record_type)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("custom_fields.update"))
            .await?;
        self.administrator(tx.sql(), scope).await?;
        authorize_key(tx.sql(), scope, decl.capability).await?;
        lock_definition_set(tx.sql(), scope.tenant(), record_type).await?;
        let limits = self.limits(tx.sql(), scope).await?;
        if let Some(errors) = validate_definition_input(input, limits.max_text_length) {
            drop(tx);
            return Ok(Outcome::Refused(errors));
        }
        if !self.valid_reference_type(input) {
            drop(tx);
            return Ok(Outcome::Refused(vec![FieldError::new(
                "reference_table",
                "must name an opted-in record type with readable targets",
            )]));
        }
        if key != input.key {
            drop(tx);
            return Ok(Outcome::Refused(vec![FieldError::new(
                "key",
                "cannot be changed",
            )]));
        }
        let record_type = record_type.to_owned();
        let key = key.to_owned();
        let tenant = scope.tenant();
        let input = input.clone();
        let old_reference_table = input.reference_table.clone();
        let saved = tx.mutate(move |sql| Box::pin(async move {
            let old_row = sql.query_opt("SELECT * FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND field_key=$3 AND archived_at IS NULL FOR UPDATE", &[&tenant,&record_type,&key]).await?.ok_or(Error::NotFound)?;
            let old = from_row(&old_row);
            if old.version != version { return Err(Error::Conflict); }
            if old.field_type != input.field_type { return Err(Error::Invalid); }
            if old.reference_table != old_reference_table { return Err(Error::Invalid); }
            let row = sql.query_one("UPDATE base_custom_field_definitions SET label=$1,required=$2,help_text=$3,position=$4,options=$5,reference_table=$6,text_limit=$7,format=$8,version=version+1,updated_at=now() WHERE tenant_id=$9 AND record_type=$10 AND field_key=$11 RETURNING *", &[&input.label,&input.required,&input.help_text,&input.position,&input.options,&input.reference_table,&input.text_limit,&input.format,&tenant,&record_type,&key]).await?;
            let new = from_row(&row);
            Ok((new.clone(), vec![Change { kind:"custom_field_definition".into(), id:new.id, old:to_json(&old), new:to_json(&new) }]))
        })).await;
        match saved {
            Ok(saved) => {
                tx.commit().await?;
                Ok(Outcome::Saved(saved))
            }
            Err(Error::NotFound) => {
                drop(tx);
                Ok(Outcome::NotFound)
            }
            Err(Error::Conflict) => {
                drop(tx);
                Ok(Outcome::Stale)
            }
            Err(Error::Invalid) => {
                drop(tx);
                Ok(Outcome::Refused(vec![FieldError::new(
                    "reference_table",
                    "cannot be changed after creation",
                )]))
            }
            Err(e) => Err(e),
        }
    }

    pub async fn archive(
        &self,
        scope: &Scope,
        record_type: &str,
        key: &str,
        version: i64,
    ) -> Result<Outcome<Definition>> {
        self.set_archived(scope, record_type, key, version, true)
            .await
    }
    pub async fn restore(
        &self,
        scope: &Scope,
        record_type: &str,
        key: &str,
        version: i64,
    ) -> Result<Outcome<Definition>> {
        self.set_archived(scope, record_type, key, version, false)
            .await
    }
    async fn set_archived(
        &self,
        scope: &Scope,
        record_type: &str,
        key: &str,
        version: i64,
        archive: bool,
    ) -> Result<Outcome<Definition>> {
        let decl = self.require_record_type(record_type)?;
        let action = if archive {
            "custom_fields.archive"
        } else {
            "custom_fields.restore"
        };
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context(action))
            .await?;
        self.administrator(tx.sql(), scope).await?;
        authorize_key(tx.sql(), scope, decl.capability).await?;
        lock_definition_set(tx.sql(), scope.tenant(), record_type).await?;
        let tenant = scope.tenant();
        let rt = record_type.to_owned();
        let key = key.to_owned();
        let restoring = !archive;
        let field_limit = self
            .limits(tx.sql(), scope)
            .await?
            .max_fields_per_record_type;
        let saved=tx.mutate(move |sql| Box::pin(async move {
            let old_row=sql.query_opt("SELECT * FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND field_key=$3 FOR UPDATE", &[&tenant,&rt,&key]).await?.ok_or(Error::NotFound)?;
            let old=from_row(&old_row); if old.version!=version { return Err(Error::Conflict); }
            if restoring && old.archived {
                let active: i64 = sql.query_one("SELECT count(*) FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND archived_at IS NULL", &[&tenant,&rt]).await?.get(0);
                if active >= field_limit as i64 { return Err(Error::Invalid); }
            }
            let row=sql.query_one(if archive {"UPDATE base_custom_field_definitions SET archived_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND record_type=$2 AND field_key=$3 RETURNING *"} else {"UPDATE base_custom_field_definitions SET archived_at=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND record_type=$2 AND field_key=$3 RETURNING *"}, &[&tenant,&rt,&key]).await?;
            let new=from_row(&row); Ok((new.clone(),vec![Change{kind:"custom_field_definition".into(),id:new.id,old:to_json(&old),new:to_json(&new)}]))
        })).await;
        match saved {
            Ok(saved) => {
                tx.commit().await?;
                Ok(Outcome::Saved(saved))
            }
            Err(Error::NotFound) => {
                drop(tx);
                Ok(Outcome::NotFound)
            }
            Err(Error::Conflict) => {
                drop(tx);
                Ok(Outcome::Stale)
            }
            Err(Error::Invalid) if restoring => {
                drop(tx);
                Ok(Outcome::Refused(vec![FieldError::new(
                    "key",
                    "cannot be restored because the active field limit has been reached",
                )]))
            }
            Err(e) => Err(e),
        }
    }

    /// Validate a custom JSON patch on the owner's mutation transaction. The
    /// owner then writes it into its own row and includes field keys in Change.
    pub async fn validate(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        record_type: &str,
        patch: &Value,
        existing: &Value,
        creating: bool,
    ) -> Result<std::result::Result<Value, Vec<FieldError>>> {
        self.authentication.reprove(client, scope).await?;
        let decl = self.require_record_type(record_type)?;
        let Some(object) = patch.as_object() else {
            return Ok(Err(vec![FieldError::new("custom", "must be an object")]));
        };
        let limits = self.limits(client, scope).await?;
        let current = existing.as_object();
        let defs = definitions(client, scope.tenant(), record_type, false).await?;
        let known: BTreeMap<&str, &Definition> = defs.iter().map(|d| (d.key.as_str(), d)).collect();
        let restricted = restrictions_for(client, scope, decl.grid_table).await?;
        let mut errors = Vec::new();
        let mut out = Map::new();
        for key in object.keys() {
            if !known.contains_key(key.as_str()) {
                errors.push(FieldError::new(
                    format!("custom.{key}"),
                    "is unknown or archived",
                ));
            }
            if restricted.contains(&format!("cf_{key}")) {
                errors.push(FieldError::new(format!("custom.{key}"), "is restricted"));
            }
        }
        for def in &defs {
            let value = object.get(&def.key);
            if value.is_none() {
                if creating && def.required {
                    errors.push(FieldError::new(
                        format!("custom.{}", def.key),
                        "is required",
                    ));
                }
                continue;
            }
            let value = value.unwrap();
            if value.is_null() {
                if def.required {
                    errors.push(FieldError::new(
                        format!("custom.{}", def.key),
                        "is required",
                    ));
                } else {
                    out.insert(def.key.clone(), Value::Null);
                }
                continue;
            }
            if def.required
                && (value.as_str().is_some_and(|text| text.trim().is_empty())
                    || value.as_array().is_some_and(Vec::is_empty))
            {
                errors.push(FieldError::new(
                    format!("custom.{}", def.key),
                    "is required",
                ));
                continue;
            }
            if matches!(def.field_type, FieldType::Text | FieldType::LongText) {
                if let Some(text) = value.as_str() {
                    let limit = effective_text_limit(def, limits.max_text_length);
                    if text.chars().count() > limit {
                        errors.push(FieldError::new(
                            format!("custom.{}", def.key),
                            format!("must be at most {limit} characters"),
                        ));
                        continue;
                    }
                }
            }
            let previous = current.and_then(|m| m.get(&def.key));
            let valid = valid_value(def, value, previous, limits.max_text_length)
                && if def.field_type == FieldType::Reference && previous != Some(value) {
                    match (def.reference_table.as_deref(), value.as_str()) {
                        (Some(table), Some(key)) => self
                            .reference_targets
                            .iter()
                            .find(|target| target.table == table)
                            .ok_or(Error::Internal)?
                            .provider
                            .resolve(client as &dyn ReadClient, scope, key)
                            .await?
                            .is_some(),
                        _ => false,
                    }
                } else {
                    true
                };
            if !valid {
                errors.push(FieldError::new(
                    format!("custom.{}", def.key),
                    "has an invalid value",
                ));
            } else {
                out.insert(def.key.clone(), value.clone());
            }
        }
        if !errors.is_empty() {
            return Ok(Err(errors));
        }
        // Patches preserve values that were not submitted.
        let mut merged = current.cloned().unwrap_or_default();
        merged.extend(out);
        if serde_json::to_vec(&merged).map_or(true, |bytes| bytes.len() > limits.max_json_bytes) {
            return Ok(Err(vec![FieldError::new(
                "custom",
                format!("must be at most {} bytes", limits.max_json_bytes),
            )]));
        }
        Ok(Ok(Value::Object(merged)))
    }

    async fn administrator(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<()> {
        self.authentication.reprove(client, scope).await?;
        authorize_key(client, scope, MANAGE).await
    }
    async fn limits(&self, client: &dyn ReadClient, scope: &Scope) -> Result<Limits> {
        let keys = [
            crate::MAX_FIELDS_SETTING,
            crate::MAX_TEXT_LENGTH_SETTING,
            crate::MAX_JSON_BYTES_SETTING,
        ];
        let resolved = self
            .settings
            .resolve_many(client, scope, Target::Tenant, &keys)
            .await?;
        let value = |key: &str| -> Result<usize> {
            let number = resolved
                .get(key)
                .and_then(|resolved| resolved.value.as_u64())
                .ok_or(Error::Invalid)?;
            usize::try_from(number).map_err(|_| Error::Invalid)
        };
        Ok(Limits {
            max_fields_per_record_type: value(crate::MAX_FIELDS_SETTING)?,
            max_text_length: value(crate::MAX_TEXT_LENGTH_SETTING)?,
            max_json_bytes: value(crate::MAX_JSON_BYTES_SETTING)?,
        })
    }
    fn require_record_type(&self, key: &str) -> Result<&RecordType> {
        self.record_types
            .iter()
            .find(|ty| ty.key == key)
            .ok_or(Error::Invalid)
    }

    fn valid_reference_type(&self, input: &FieldInput) -> bool {
        match input.field_type {
            FieldType::Reference => input.reference_table.as_deref().is_some_and(|table| {
                self.reference_targets
                    .iter()
                    .any(|target| target.table == table)
            }),
            _ => input.reference_table.is_none(),
        }
    }
}

fn validate_definition_input(
    input: &FieldInput,
    max_text_length: usize,
) -> Option<Vec<FieldError>> {
    let mut errors = Vec::new();
    if input.key.is_empty()
        || input.key.len() > 63
        || !input
            .key
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase())
        || !input
            .key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        errors.push(FieldError::new(
            "key",
            "must start with a lowercase letter and use lowercase letters, digits or underscores",
        ));
    }
    if input.label.trim().is_empty() || input.label.chars().count() > MAX_LABEL {
        errors.push(FieldError::new(
            "label",
            "is required and must be at most 120 characters",
        ));
    }
    if input
        .help_text
        .as_ref()
        .is_some_and(|s| s.chars().count() > MAX_HELP)
    {
        errors.push(FieldError::new(
            "help_text",
            "must be at most 500 characters",
        ));
    }
    if input.position < 0 {
        errors.push(FieldError::new("position", "must be zero or greater"));
    }
    if input
        .text_limit
        .is_some_and(|n| n < 1 || n as usize > max_text_length)
    {
        errors.push(FieldError::new(
            "text_limit",
            format!("must be between 1 and {max_text_length}"),
        ));
    }
    if input.text_limit.is_some()
        && !matches!(input.field_type, FieldType::Text | FieldType::LongText)
    {
        errors.push(FieldError::new(
            "text_limit",
            "is only available for text fields",
        ));
    }
    if input.format.as_deref().is_some_and(|format| {
        !matches!(format, "email" | "url" | "phone")
            || !matches!(input.field_type, FieldType::Text | FieldType::LongText)
    }) {
        errors.push(FieldError::new(
            "format",
            "is unsupported for this field type",
        ));
    }
    if matches!(input.field_type, FieldType::Choice | FieldType::MultiChoice) {
        if !valid_options(&input.options) {
            errors.push(FieldError::new(
                "options",
                "must contain unique stable codes and labels",
            ));
        }
    } else if input.options.as_array().is_none_or(|a| !a.is_empty()) {
        errors.push(FieldError::new(
            "options",
            "are only available for choice fields",
        ));
    }
    if input.field_type == FieldType::Reference
        && input.reference_table.as_deref().is_none_or(str::is_empty)
    {
        errors.push(FieldError::new(
            "reference_table",
            "is required for a reference field",
        ));
    }
    if input.field_type != FieldType::Reference && input.reference_table.is_some() {
        errors.push(FieldError::new(
            "reference_table",
            "is only available for reference fields",
        ));
    }
    if errors.is_empty() {
        None
    } else {
        Some(errors)
    }
}
fn valid_options(value: &Value) -> bool {
    let Some(items) = value.as_array() else {
        return false;
    };
    if items.is_empty() || items.len() > 500 {
        return false;
    }
    let mut keys = std::collections::HashSet::new();
    items.iter().all(|item| {
        let key = item.get("code").and_then(Value::as_str).unwrap_or_default();
        let label = item
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or_default();
        !key.is_empty()
            && key.len() <= 64
            && key.bytes().all(|b| {
                b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
            })
            && !label.trim().is_empty()
            && keys.insert(key.to_owned())
    })
}
fn valid_value(
    def: &Definition,
    value: &Value,
    current: Option<&Value>,
    max_text_length: usize,
) -> bool {
    match def.field_type {
        FieldType::Text | FieldType::LongText => value.as_str().is_some_and(|s| {
            s.chars().count() <= effective_text_limit(def, max_text_length)
                && match def.format.as_deref() {
                    Some("email") => s.is_empty() || s.contains('@'),
                    Some("url") => {
                        s.is_empty() || s.starts_with("http://") || s.starts_with("https://")
                    }
                    Some("phone") => s.chars().all(|c| c.is_ascii_digit() || "+() -".contains(c)),
                    _ => true,
                }
        }),
        FieldType::Integer => value.as_i64().is_some(),
        FieldType::Decimal => value
            .as_str()
            .is_some_and(|s| s.parse::<rust_decimal::Decimal>().is_ok()),
        FieldType::Boolean => value.is_boolean(),
        FieldType::Date => value
            .as_str()
            .is_some_and(|s| jiff::civil::Date::from_str(s).is_ok()),
        FieldType::DateTime => value
            .as_str()
            .is_some_and(|s| jiff::Timestamp::from_str(s).is_ok()),
        FieldType::Choice => value.as_str().is_some_and(|code| {
            option_exists(&def.options, code)
                && (option_active(&def.options, code) || current == Some(value))
        }),
        FieldType::MultiChoice => value.as_array().is_some_and(|codes| {
            let mut seen = std::collections::HashSet::new();
            let old = current.and_then(Value::as_array);
            codes.iter().all(|value| {
                value.as_str().is_some_and(|code| {
                    seen.insert(code.to_owned())
                        && (option_active(&def.options, code)
                            || old.is_some_and(|old| {
                                old.iter().any(|prior| prior.as_str() == Some(code))
                            }))
                })
            })
        }),
        FieldType::Reference => value
            .as_str()
            .is_some_and(|s| !s.is_empty() && s.len() <= 200),
    }
}

fn effective_text_limit(def: &Definition, configured: usize) -> usize {
    def.text_limit
        .map(|limit| limit as usize)
        .unwrap_or(if def.field_type == FieldType::LongText {
            DEFAULT_MAX_TEXT_LENGTH
        } else {
            255
        })
        .min(configured)
}
fn option_exists(options: &Value, code: &str) -> bool {
    options
        .as_array()
        .is_some_and(|a| a.iter().any(|o| o["code"].as_str() == Some(code)))
}
fn option_active(options: &Value, code: &str) -> bool {
    options.as_array().is_some_and(|a| {
        a.iter()
            .any(|o| o["code"].as_str() == Some(code) && o["active"].as_bool() != Some(false))
    })
}
async fn definitions(
    client: &impl GenericClient,
    tenant: i64,
    record_type: &str,
    archived: bool,
) -> Result<Vec<Definition>> {
    let rows=client.query("SELECT * FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND ($3 OR archived_at IS NULL) ORDER BY position,field_key", &[&tenant,&record_type,&archived]).await?;
    Ok(rows.iter().map(from_row).collect())
}

async fn definitions_on_read(
    client: &(impl ReadClient + ?Sized),
    tenant: i64,
    record_type: &str,
) -> Result<Vec<Definition>> {
    client
        .query(
            "SELECT pg_advisory_xact_lock(hashtextextended($1::bigint::text || ':' || $2::text, 0))",
            &[&tenant, &record_type],
        )
        .await?;
    let rows = client
        .query(
            "SELECT * FROM base_custom_field_definitions WHERE tenant_id=$1 AND record_type=$2 AND archived_at IS NULL ORDER BY position,field_key",
            &[&tenant, &record_type],
        )
        .await?;
    Ok(rows.iter().map(from_row).collect())
}

async fn lock_definition_set(
    client: &(impl GenericClient + Sync),
    tenant: i64,
    record_type: &str,
) -> Result<()> {
    client
        .query_one(
            "SELECT pg_advisory_xact_lock(hashtextextended($1::bigint::text || ':' || $2::text, 0))",
            &[&tenant, &record_type],
        )
        .await?;
    Ok(())
}

pub(crate) async fn grid_fields(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Vec<bilibi_grid::RuntimeField>> {
    let mut fields = Vec::new();
    for record_type in &registered_runtime_types()? {
        for def in active_definitions(client, scope.tenant(), record_type.key).await? {
            let readable_values = if def.field_type == FieldType::Reference {
                let table = def.reference_table.as_deref().ok_or(Error::Internal)?;
                let target = references::providers()?
                    .into_iter()
                    .find(|target| target.table == table)
                    .ok_or(Error::Internal)?;
                Some(
                    target
                        .provider
                        .choices(client as &dyn ReadClient, scope)
                        .await?
                        .into_iter()
                        .map(|choice| choice.key)
                        .collect(),
                )
            } else {
                None
            };
            fields.push(bilibi_grid::RuntimeField {
                table: record_type.grid_table.into(),
                column: record_type.custom_column,
                key: def.key,
                label: def.label,
                ty: grid_type(def.field_type),
                values: def
                    .options
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|option| {
                        option
                            .get("code")
                            .and_then(Value::as_str)
                            .map(str::to_owned)
                    })
                    .collect(),
                readable_values,
            });
        }
    }
    Ok(fields)
}

pub(crate) async fn authz_fields(
    client: &(impl GenericClient + Sync),
    scope: &Scope,
) -> Result<Vec<bilibi_authz::field_catalog::CatalogTable>> {
    let mut tables: BTreeMap<String, bilibi_authz::field_catalog::CatalogTable> = BTreeMap::new();
    for record_type in &registered_runtime_types()? {
        let fields = active_definitions(client, scope.tenant(), record_type.key).await?;
        if fields.is_empty() {
            continue;
        }
        let table = tables
            .entry(record_type.grid_table.into())
            .or_insert_with(|| bilibi_authz::field_catalog::CatalogTable {
                id: record_type.grid_table.into(),
                label: record_type.label.into(),
                record_types: vec![record_type.audit_kind.into()],
                fields: Vec::new(),
            });
        for def in fields {
            let reference_values = if def.field_type == FieldType::Reference {
                let table = def.reference_table.as_deref().ok_or(Error::Internal)?;
                let target = references::providers()?
                    .into_iter()
                    .find(|target| target.table == table)
                    .ok_or(Error::Internal)?;
                Some(
                    target
                        .provider
                        .choices(client as &dyn ReadClient, scope)
                        .await?
                        .into_iter()
                        .map(|choice| choice.key)
                        .collect(),
                )
            } else {
                None
            };
            table
                .fields
                .push(bilibi_authz::field_catalog::CatalogField {
                    id: format!("cf_{}", def.key),
                    label: def.label,
                    reference_values,
                });
        }
    }
    Ok(tables.into_values().collect())
}

fn registered_runtime_types() -> Result<Vec<RecordType>> {
    let modules = bilibi_module_registry::discover().map_err(|_| Error::Internal)?;
    let catalog = Catalog::discover(&modules).map_err(|_| Error::Internal)?;
    registered(catalog.entries::<RecordType>(RECORD_TYPES)?).map_err(|_| Error::Internal)
}

fn grid_type(ty: FieldType) -> bilibi_grid::FieldType {
    use bilibi_grid::FieldType as G;
    match ty {
        FieldType::Text | FieldType::LongText | FieldType::Reference | FieldType::MultiChoice => {
            G::String
        }
        FieldType::Integer => G::Integer,
        FieldType::Decimal => G::Decimal,
        FieldType::Date => G::Date,
        FieldType::DateTime => G::DateTime,
        FieldType::Boolean => G::Boolean,
        FieldType::Choice => G::Enum,
    }
}

pub(crate) async fn active_definitions(
    client: &impl GenericClient,
    tenant: i64,
    record_type: &str,
) -> Result<Vec<Definition>> {
    definitions(client, tenant, record_type, false).await
}
fn from_row(row: &tokio_postgres::Row) -> Definition {
    let ty: String = row.get("field_type");
    Definition {
        id: row.get("id"),
        tenant_id: row.get("tenant_id"),
        record_type: row.get("record_type"),
        key: row.get("field_key"),
        label: row.get("label"),
        field_type: FieldType::parse(&ty).unwrap_or(FieldType::Text),
        required: row.get("required"),
        help_text: row.get("help_text"),
        position: row.get("position"),
        options: row.get("options"),
        reference_table: row.get("reference_table"),
        text_limit: row.get("text_limit"),
        format: row.get("format"),
        archived: row
            .get::<_, Option<std::time::SystemTime>>("archived_at")
            .is_some(),
        version: row.get("version"),
    }
}
fn to_json(def: &Definition) -> Value {
    json!({"id":def.id,"tenant_id":def.tenant_id,"record_type":def.record_type,"key":def.key,"label":def.label,"field_type":def.field_type.as_str(),"required":def.required,"options":def.options,"archived":def.archived,"version":def.version})
}
