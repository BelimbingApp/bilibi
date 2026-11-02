//! The pick-list operations agents may call (`base.picklists.*`): thin
//! adapters over the `PickLists` facade with the sealed scope the registry
//! proved. Reads list the mounted lists and one list's values; every write is
//! a reviewed safe write whose preview shares the facade's checks without
//! locking or writing and whose commit writes through the plan's audited
//! transaction (`base/agent_api` owns review, replay and the outer commit).
//! Same shape as `core/company`'s relationship operations.
use crate::{
    declaration::{Reach, CODE_MAX, HINT_MAX, LABEL_MAX},
    values::{Checked, FieldError, NewValue, PickLists, ValueUpdate},
    VIEW,
};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Db, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use serde_json::{json, Map, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    sync::{Arc, OnceLock},
};

struct Bound {
    db: Db,
    lists: Arc<PickLists>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();
pub(crate) fn bind(db: Db, lists: Arc<PickLists>) -> bilibi_module_registry::Result<()> {
    BOUND.set(Bound { db, lists }).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate pick-list agent binding".into())
    })
}
fn bound() -> bilibi_agent_api::Result<&'static Bound> {
    BOUND.get().ok_or(Error::Internal.into())
}
fn invalid(errors: Vec<FieldError>) -> bilibi_agent_api::Error {
    let mut fields: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for error in errors {
        fields
            .entry(error.field.to_owned())
            .or_default()
            .push(error.message);
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
fn texts(input: &Value, field: &str) -> Vec<String> {
    input[field]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
fn version(input: &Value) -> bilibi_agent_api::Result<i64> {
    input["version"]
        .as_i64()
        .filter(|value| *value > 0)
        .ok_or(Error::Invalid.into())
}
fn labels(input: &Value) -> BTreeMap<String, String> {
    input["labels"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(language, label)| Some((language.clone(), label.as_str()?.to_owned())))
        .collect()
}
fn hint(input: &Value) -> Option<String> {
    input["hint"].as_str().map(str::to_owned)
}
fn new_value(input: &Value) -> NewValue {
    NewValue {
        code: text(input, "code"),
        label: text(input, "label"),
        labels: labels(input),
        hint: hint(input),
        parents: texts(input, "parents"),
    }
}
fn update(input: &Value) -> bilibi_agent_api::Result<ValueUpdate> {
    Ok(ValueUpdate {
        label: text(input, "label"),
        labels: labels(input),
        hint: hint(input),
        parents: texts(input, "parents"),
        version: version(input)?,
    })
}
fn record(value: Value) -> Data {
    Data::restricted(value, &HashSet::new())
}
fn to_json(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).expect("pick-list output serializes")
}

enum Read {
    Lists,
    Values,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/picklists"
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
                Self::Lists => {
                    // Only the lists whose own capability the caller holds.
                    let mut lists = Vec::new();
                    for list in bound.lists.lists() {
                        if let Some(capability) = list.capability {
                            match bilibi_authz::authorize_key(&**client, scope, capability).await {
                                Ok(()) => {}
                                Err(Error::Forbidden) => continue,
                                Err(error) => return Err(error.into()),
                            }
                        }
                        lists.push(json!({
                            "key": list.key,
                            "label": list.label,
                            "reach": match list.reach { Reach::Tenant => "tenant", Reach::Installation => "installation" },
                            "parent": list.parent,
                            "capability": list.capability,
                            "locked": list.values.iter().filter(|value| value.locked).map(|value| value.code).collect::<Vec<_>>(),
                        }));
                    }
                    Value::Array(lists)
                }
                Self::Values => to_json(
                    bound
                        .lists
                        .values(
                            &**client,
                            scope,
                            input["list"].as_str().ok_or(Error::Invalid)?,
                            input["include_retired"].as_bool().unwrap_or(false),
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
    Create,
    Update,
    Reorder,
    Default,
    Retire,
    Restore,
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/picklists"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let bound = bound()?;
            let lists = &bound.lists;
            let client = bound.db.get().await.map_err(Error::from)?;
            let client = &**client;
            let list = input["list"].as_str().ok_or(Error::Invalid)?;
            let code = input["code"].as_str();
            let value = match self {
                Self::Create => {
                    let value = checked(
                        lists
                            .preview_create(client, scope, list, &new_value(input))
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Add “{}” ({}) to {list}", value.label, value.code),"value":value})
                }
                Self::Update => {
                    let code = code.ok_or(Error::Invalid)?;
                    let (before, after) = checked(
                        lists
                            .preview_update(client, scope, list, code, &update(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Change {code} in {list}; its code stays"),"before":before,"after":after})
                }
                Self::Reorder => {
                    let moves = checked(
                        lists
                            .preview_reorder(client, scope, list, &texts(input, "codes"))
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Reorder {list}: {} value(s) move", moves.len()),
                        "moves":moves.iter().map(|(code, from, to)| json!({"code":code,"from":from,"to":to})).collect::<Vec<_>>()})
                }
                Self::Default => {
                    let (before, after) = checked(
                        lists
                            .preview_default(client, scope, list, code)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":match &after { Some(code) => format!("Make {code} the default of {list}"), None => format!("Clear the default of {list}") },"before":before,"after":after})
                }
                Self::Retire => {
                    let code = code.ok_or(Error::Invalid)?;
                    let before = checked(
                        lists
                            .preview_retire(client, scope, list, code, version(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Retire {code} in {list}; records holding it keep it"),"before":before})
                }
                Self::Restore => {
                    let code = code.ok_or(Error::Invalid)?;
                    let before = checked(
                        lists
                            .preview_restore(client, scope, list, code, version(input)?)
                            .await
                            .map_err(found)?,
                    )?;
                    json!({"summary":format!("Restore {code} in {list}"),"before":before})
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
            let lists = &bound()?.lists;
            let list = input["list"].as_str().ok_or(Error::Invalid)?;
            let code = input["code"].as_str();
            let value = match self {
                Self::Create => to_json(checked(
                    lists
                        .create(tx, scope, list, &new_value(input))
                        .await
                        .map_err(found)?,
                )?),
                Self::Update => to_json(checked(
                    lists
                        .update(
                            tx,
                            scope,
                            list,
                            code.ok_or(Error::Invalid)?,
                            &update(input)?,
                        )
                        .await
                        .map_err(found)?,
                )?),
                Self::Reorder => to_json(checked(
                    lists
                        .reorder(tx, scope, list, &texts(input, "codes"))
                        .await
                        .map_err(found)?,
                )?),
                Self::Default => to_json(checked(
                    lists
                        .set_default(tx, scope, list, code)
                        .await
                        .map_err(found)?,
                )?),
                Self::Retire => to_json(checked(
                    lists
                        .retire(
                            tx,
                            scope,
                            list,
                            code.ok_or(Error::Invalid)?,
                            version(input)?,
                        )
                        .await
                        .map_err(found)?,
                )?),
                Self::Restore => to_json(checked(
                    lists
                        .restore(
                            tx,
                            scope,
                            list,
                            code.ok_or(Error::Invalid)?,
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

fn list_key() -> Value {
    json!({"type":"string","maxLength":128,"description":"The list's key, from base.picklists.lists."})
}
fn code() -> Value {
    json!({"type":"string","maxLength":CODE_MAX,"description":"The value's stable code."})
}
fn version_schema() -> Value {
    json!({"type":"integer","minimum":1,"description":"The version you read; a changed value is refused."})
}
fn codes() -> Value {
    json!({"type":"array","items":{"type":"string","maxLength":CODE_MAX}})
}
/// One optional property per language `base/locale` supports.
fn labels_schema() -> Value {
    let languages: BTreeSet<&str> = bilibi_locale::SUPPORTED
        .iter()
        .map(|entry| entry.language)
        .collect();
    let properties: Map<String, Value> = languages
        .into_iter()
        .map(|language| {
            (
                language.to_owned(),
                json!({"type":"string","maxLength":LABEL_MAX}),
            )
        })
        .collect();
    json!({"type":"object","description":"Translations by language; a blank one is removed.","properties":properties})
}
fn value_fields(required: &[&str], update: bool) -> Value {
    let mut properties = json!({
        "list":list_key(),
        "label":{"type":"string","maxLength":LABEL_MAX,"description":"The default label, shown when the reader's language has none."},
        "labels":labels_schema(),
        "hint":{"type":"string","maxLength":HINT_MAX,"description":"Optional help text; blank removes it."},
        "parents":{"type":"array","items":{"type":"string","maxLength":CODE_MAX},"description":"In a dependent list, the parent codes this value belongs to (at least one)."},
    });
    properties["code"] = code();
    if update {
        properties["version"] = version_schema();
    }
    json!({"type":"object","required":required,"properties":properties})
}
fn lists_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn values_schema() -> Value {
    json!({"type":"object","required":["list"],"properties":{"list":list_key(),"include_retired":{"type":"boolean"}}})
}
fn create_schema() -> Value {
    value_fields(&["list", "code", "label"], false)
}
fn update_schema() -> Value {
    value_fields(&["list", "code", "label", "version"], true)
}
fn reorder_schema() -> Value {
    json!({"type":"object","required":["list","codes"],"properties":{"list":list_key(),"codes":codes()}})
}
fn default_schema() -> Value {
    json!({"type":"object","required":["list"],"properties":{"list":list_key(),"code":code()}})
}
fn status_schema() -> Value {
    json!({"type":"object","required":["list","code","version"],"properties":{"list":list_key(),"code":code(),"version":version_schema()}})
}
fn lists_output() -> Value {
    json!({"type":"array","description":"The mounted lists you may see: key, label, reach (tenant or installation), parent list, extra capability and locked codes."})
}
fn value_output() -> Value {
    json!({"type":"object","description":"A value: id, list, code, label, labels, hint, position, default, parents, locked, retired_on and version."})
}
fn values_output() -> Value {
    json!({"type":"array","description":"A list's values in order."})
}

const MODULE: &str = "base/picklists";
/// The registry checks one fixed capability per operation, but who may
/// change a list depends on its reach (`values::required`): a tenant list
/// needs `base.picklists.manage`, an installation-wide list
/// `base.picklists.installation.manage` in the platform-operator tenant. So
/// the writes open to every curator (`view`) and the facade decides by
/// reach inside the write's transaction, as the page does.
const WRITE: &str = VIEW;
inventory::submit! { Contribution::new(DEFINITIONS, "base/picklists.agent", Provenance { module: MODULE, source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: "base.picklists.lists", title: "List pick-lists", summary: "The value lists mounted modules offer (categories, reasons, sources), with their reach, parent list and locked codes.", keywords: &["dropdown", "category", "reference", "options"], capability: VIEW, input: lists_schema, output: lists_output, handler: Handler::Read(&Read::Lists), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.values", title: "Read a pick-list's values", summary: "One list's values in order, with labels, translations, parents, default and status; retired values on request.", keywords: &["dropdown", "category", "options", "values"], capability: VIEW, input: values_schema, output: values_output, handler: Handler::Read(&Read::Values), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.value.create", title: "Add a pick-list value", summary: "Add a value with a new stable code, a label, optional translations, hint and parent codes. It goes after the list's other values.", keywords: &["dropdown", "option", "add"], capability: WRITE, input: create_schema, output: value_output, handler: Handler::SafeWrite(&Write::Create), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.value.update", title: "Change a pick-list value", summary: "Rename, translate, re-hint or re-parent a value using its current version; its code never changes.", keywords: &["rename", "translate", "option"], capability: WRITE, input: update_schema, output: value_output, handler: Handler::SafeWrite(&Write::Update), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.value.reorder", title: "Reorder a pick-list", summary: "Put a list in a new order by naming every code, retired ones included, once each.", keywords: &["order", "sort", "option"], capability: WRITE, input: reorder_schema, output: values_output, handler: Handler::SafeWrite(&Write::Reorder), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.value.default", title: "Set a pick-list's default", summary: "Choose the active value that pre-fills a new record, or omit code to clear the default.", keywords: &["default", "option"], capability: WRITE, input: default_schema, output: value_output, handler: Handler::SafeWrite(&Write::Default), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.value.retire", title: "Retire a pick-list value", summary: "Stop new records choosing a value; records holding it keep it. Locked values cannot be retired.", keywords: &["retire", "archive", "option"], capability: WRITE, input: status_schema, output: value_output, handler: Handler::SafeWrite(&Write::Retire), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.picklists.value.restore", title: "Restore a pick-list value", summary: "Bring a retired value back into new choices.", keywords: &["restore", "option"], capability: WRITE, input: status_schema, output: value_output, handler: Handler::SafeWrite(&Write::Restore), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "base.picklists", title: "Pick-lists", summary: "How the value lists modules offer are curated: stable codes, retirement, locked values, dependent lists and translations.", keywords: &["dropdown", "category", "reference", "options"], capability: VIEW,
        body: "A pick-list is a list of values a module offers in a dropdown, such as case categories or loss reasons; your tenant's administrators curate it. Find lists with base.picklists.lists and read one with base.picklists.values before changing it.\n\nRecords store a value's code, so a code never changes: rename or translate a value with base.picklists.value.update, never by creating a new code for the same meaning. Values are retired, never deleted (base.picklists.value.retire): new records cannot choose a retired value, records holding it keep it, and base.picklists.value.restore brings it back. A value listed as locked is one the module's own code relies on; it can be relabelled, translated and reordered but not retired.\n\nIn a dependent list (one with a parent list), every value names at least one active parent code. Translations are keyed by language (ms, zh, …) and a blank one removes it. Reorder by naming every code of the list once. Updates, retirements and restorations need the version you read. Changing a value needs base.picklists.manage for your tenant's lists; an installation-wide list belongs to the platform operator, needs base.picklists.installation.manage there, and is read-only to other tenants. A list may also need its own capability. For each write, run the dry-run check, show the complete preview, wait for explicit approval, then commit the unchanged operations." }],
}) }
