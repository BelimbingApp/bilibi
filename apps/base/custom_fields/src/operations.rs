//! The custom-field operations agents may call (`base.custom_fields.*`): thin
//! adapters over the `CustomFields` facade with the sealed scope the registry
//! proved. Reads list the record types and the definitions; every write is a
//! reviewed safe write whose preview shares the facade's checks without
//! locking or writing and whose commit writes through the plan's audited
//! transaction (`base/agent_api` owns review, replay and the outer commit).
//! The administration page calls these same contracts.
use crate::{
    declaration::{LABEL_MAX, RECORD_TYPE_MAX},
    definitions::{Checked, CustomFields, FieldError, FieldUpdate, NewField, HELP_MAX, KEY_MAX},
    Kind, MANAGE, VIEW,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    sync::{Arc, OnceLock},
};

struct Bound {
    db: Db,
    fields: Arc<CustomFields>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();
pub(crate) fn bind(db: Db, fields: Arc<CustomFields>) -> bilibi_module_registry::Result<()> {
    BOUND.set(Bound { db, fields }).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate custom-field agent binding".into(),
        )
    })
}
fn bound() -> bilibi_agent_api::Result<&'static Bound> {
    BOUND.get().ok_or(Error::Internal.into())
}
fn invalid(errors: Vec<FieldError>) -> bilibi_agent_api::Error {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for error in errors {
        fields.entry(error.field).or_default().push(error.message);
    }
    bilibi_agent_api::Error::InvalidInput(fields)
}
fn checked<T>(checked: Checked<T>) -> bilibi_agent_api::Result<T> {
    checked.map_err(invalid)
}
fn found(error: Error) -> bilibi_agent_api::Error {
    match error {
        Error::NotFound => bilibi_agent_api::Error::NotFound,
        other => other.into(),
    }
}
fn text(input: &Value, field: &str) -> String {
    input[field].as_str().unwrap_or_default().to_owned()
}
fn version(input: &Value) -> bilibi_agent_api::Result<i64> {
    input["version"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or(Error::Invalid.into())
}
fn new_field(input: &Value) -> NewField {
    NewField {
        record_type: text(input, "record_type"),
        key: text(input, "key"),
        label: text(input, "label"),
        help: input["help"].as_str().map(str::to_owned),
        kind: text(input, "kind"),
        picklist: input["picklist"].as_str().map(str::to_owned),
    }
}
fn update(input: &Value) -> bilibi_agent_api::Result<FieldUpdate> {
    Ok(FieldUpdate {
        label: text(input, "label"),
        help: input["help"].as_str().map(str::to_owned),
        version: version(input)?,
    })
}
fn record(value: Value) -> Data {
    Data::restricted(value, &HashSet::new())
}
fn to_json(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("custom field output serializes")
}

enum Read {
    RecordTypes,
    List,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/custom_fields"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let bound = bound()?;
            let client = bound.db.get().await.map_err(Error::from)?;
            let value = match self {
                Self::RecordTypes => {
                    bilibi_authz::authorize_key(&**client, scope, VIEW).await?;
                    json!({
                        "record_types": bound.fields.record_types().map(|record| json!({"id": record.id, "label": record.label})).collect::<Vec<_>>(),
                        "pick_lists": bound.fields.choice_lists().into_iter().map(|(key, label)| json!({"key": key, "label": label})).collect::<Vec<_>>(),
                    })
                }
                Self::List => to_json(
                    bound
                        .fields
                        .definitions(
                            &**client,
                            scope,
                            input["record_type"].as_str(),
                            input["include_archived"].as_bool().unwrap_or(false),
                        )
                        .await
                        .map_err(found)?,
                ),
            };
            Ok(Output::Data(record(value)))
        })
    }
}

enum Write {
    Define,
    Update,
    Archive,
    Restore,
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/custom_fields"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let bound = bound()?;
            let fields = &bound.fields;
            let client = bound.db.get().await.map_err(Error::from)?;
            let client = &**client;
            let record_type = input["record_type"].as_str().unwrap_or_default();
            let key = input["key"].as_str();
            let value = match self {
                Self::Define => {
                    let field = checked(
                        fields
                            .preview_define(client, scope, &new_field(input))
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Add the {} field “{}” ({}) to {}", field.kind, field.label, field.key, field.record_type),"field":field})
                }
                Self::Update => {
                    let key = key.ok_or(Error::Invalid)?;
                    let (before, after) = checked(
                        fields
                            .preview_update(client, scope, record_type, key, &update(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Change {key} on {record_type}; its key and type stay"),"before":before,"after":after})
                }
                Self::Archive => {
                    let key = key.ok_or(Error::Invalid)?;
                    let before = checked(
                        fields
                            .preview_archive(client, scope, record_type, key, version(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Archive {key} on {record_type}; records keep its values"),"before":before})
                }
                Self::Restore => {
                    let key = key.ok_or(Error::Invalid)?;
                    let before = checked(
                        fields
                            .preview_restore(client, scope, record_type, key, version(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Restore {key} on {record_type}"),"before":before})
                }
            };
            Ok(value.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let fields = &bound()?.fields;
            let record_type = input["record_type"].as_str().unwrap_or_default();
            let key = input["key"].as_str();
            let value = match self {
                Self::Define => to_json(checked(
                    fields
                        .define(tx, scope, &new_field(input))
                        .await
                        .map_err(found)?,
                )?),
                Self::Update => to_json(checked(
                    fields
                        .update(
                            tx,
                            scope,
                            record_type,
                            key.ok_or(Error::Invalid)?,
                            &update(input)?,
                        )
                        .await
                        .map_err(found)?,
                )?),
                Self::Archive => to_json(checked(
                    fields
                        .archive(
                            tx,
                            scope,
                            record_type,
                            key.ok_or(Error::Invalid)?,
                            version(input)?,
                        )
                        .await
                        .map_err(found)?,
                )?),
                Self::Restore => to_json(checked(
                    fields
                        .restore(
                            tx,
                            scope,
                            record_type,
                            key.ok_or(Error::Invalid)?,
                            version(input)?,
                        )
                        .await
                        .map_err(found)?,
                )?),
            };
            Ok(Output::Data(record(value)))
        })
    }
}

fn record_type_schema() -> Value {
    json!({"type":"string","maxLength":RECORD_TYPE_MAX,"description":"The kind of record, from base.custom_fields.record_types (for example company)."})
}
fn key_schema() -> Value {
    json!({"type":"string","maxLength":KEY_MAX,"description":"The field's stable key: a lower-case letter, then letters, digits and underscores. It never changes and is never reused."})
}
fn version_schema() -> Value {
    json!({"type":"integer","minimum":1,"description":"The version you read; a changed field is refused."})
}
fn text_properties() -> Value {
    json!({
        "label":{"type":"string","maxLength":LABEL_MAX,"description":"What people see beside the value."},
        "help":{"type":"string","maxLength":HELP_MAX,"description":"Optional help text; blank removes it."},
    })
}
fn record_types_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn list_schema() -> Value {
    json!({"type":"object","properties":{"record_type":record_type_schema(),"include_archived":{"type":"boolean"}}})
}
fn define_schema() -> Value {
    let mut properties = text_properties();
    properties["record_type"] = record_type_schema();
    properties["key"] = key_schema();
    properties["kind"] = json!({"type":"string","enum":Kind::ALL.map(Kind::as_str),"description":"What the field holds; it never changes. A number is kept exactly as written."});
    properties["picklist"] = json!({"type":"string","maxLength":128,"description":"For a choice field, the pick-list key from base.custom_fields.record_types; omit for any other kind."});
    json!({"type":"object","required":["record_type","key","label","kind"],"properties":properties})
}
fn update_schema() -> Value {
    let mut properties = text_properties();
    properties["record_type"] = record_type_schema();
    properties["key"] = key_schema();
    properties["version"] = version_schema();
    json!({"type":"object","required":["record_type","key","label","version"],"properties":properties})
}
fn status_schema() -> Value {
    json!({"type":"object","required":["record_type","key","version"],"properties":{"record_type":record_type_schema(),"key":key_schema(),"version":version_schema()}})
}
fn record_types_output() -> Value {
    json!({"type":"object","description":"record_types (id and label) that offer custom fields, and pick_lists (key and label) a choice field may use."})
}
fn list_output() -> Value {
    json!({"type":"array","description":"The tenant's fields: id, record_type, key, label, help, kind, picklist, position, archived_on and version."})
}
fn field_output() -> Value {
    json!({"type":"object","description":"A field: id, record_type, key, label, help, kind, picklist, position, archived_on and version."})
}

const MODULE: &str = "base/custom_fields";
inventory::submit! { Contribution::new(DEFINITIONS, "base/custom_fields.agent", Provenance { module: MODULE, source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "base.custom_fields.record_types", title: "List the records that take custom fields", summary: "The kinds of record whose organisation can add its own fields, and the pick-lists a choice field may use.", keywords: &["custom", "field", "attribute", "extra"], capability: VIEW, input: record_types_schema, output: record_types_output, handler: Handler::Read(&Read::RecordTypes), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.custom_fields.list", title: "List custom fields", summary: "The organisation's own fields on a kind of record, with their key, type, label and status; archived ones on request.", keywords: &["custom", "field", "attribute", "extra"], capability: VIEW, input: list_schema, output: list_output, handler: Handler::Read(&Read::List), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.custom_fields.define", title: "Add a custom field", summary: "Add a text, number, date or pick-list field to a kind of record. Its key and type never change.", keywords: &["custom", "field", "attribute", "add", "create"], capability: MANAGE, input: define_schema, output: field_output, handler: Handler::SafeWrite(&Write::Define), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.custom_fields.update", title: "Change a custom field", summary: "Rename a field or rewrite its help text using its current version; its key and type never change.", keywords: &["custom", "field", "rename", "help"], capability: MANAGE, input: update_schema, output: field_output, handler: Handler::SafeWrite(&Write::Update), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.custom_fields.archive", title: "Archive a custom field", summary: "Hide a field from forms, columns and agents; records keep the values already stored under it.", keywords: &["custom", "field", "archive", "hide", "retire"], capability: MANAGE, input: status_schema, output: field_output, handler: Handler::SafeWrite(&Write::Archive), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.custom_fields.restore", title: "Restore a custom field", summary: "Bring an archived field back with the values its records kept.", keywords: &["custom", "field", "restore", "unarchive"], capability: MANAGE, input: status_schema, output: field_output, handler: Handler::SafeWrite(&Write::Restore), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "base.custom_fields", title: "Custom fields", summary: "How an organisation adds its own fields to records: types, stable keys, pick-lists and archiving.", keywords: &["custom", "field", "attribute", "extra"], capability: VIEW,
        body: "A custom field is a typed value an organisation adds to a kind of record, such as a Segment on a company, without a software release. Find the kinds of record that take them, and the pick-lists a choice field may use, with base.custom_fields.record_types; read the fields with base.custom_fields.list.\n\nA field has a stable key (a lower-case letter, then letters, digits and underscores), a label, optional help text and a type: text, number, date or pick-list. The key and the type never change and a key is never reused, even after the field is archived, so decide them with care. A number is kept exactly as written. A pick-list field stores the code of a value from one of the organisation's pick-lists.\n\nArchive a field with base.custom_fields.archive rather than deleting it: forms, columns and agents stop showing it and records keep their values, and base.custom_fields.restore brings it back. Updates, archives and restorations need the version you read. A record type may have a limited number of active fields, set by the organisation's administrators. Defining and changing fields needs base.custom-fields.manage; reading them needs base.custom-fields.view. For each write, run the dry-run check, show the complete preview, wait for explicit approval, then commit the unchanged operations." }],
}) }
