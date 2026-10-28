//! Import contracts (ADR 01): the Imports pages and agents call the same
//! ones. Reads cover the record types a person may import, their own runs
//! and row results and the saved mappings; reviewed writes map a run and
//! start its dry run or real run, cancel it, and save or delete mappings.
//! Uploading the file is the page's for now (agent-supplied files are a later
//! phase of issue 133), so an agent works on runs its person uploaded.
use crate::{
    mapping::MAX_FIELDS, Action, BulkData, Mapping, Outcome, Pass, WriteError, CREATE, VIEW,
};
use bilibi_agent_api::{
    json::{Data, Output, Page},
    Channels, Definitions, Guide, Handler, Operation, OperationClient, OperationFuture,
    ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::Contribution;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

static BULK: OnceLock<Arc<BulkData>> = OnceLock::new();
pub(crate) fn bind(bulk: Arc<BulkData>) -> bilibi_module_registry::Result<()> {
    BULK.set(bulk).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate import agent binding".into())
    })
}
fn bulk() -> bilibi_agent_api::Result<&'static Arc<BulkData>> {
    BULK.get().ok_or(Error::Internal.into())
}

fn error(error: WriteError) -> bilibi_agent_api::Error {
    match error {
        WriteError::NotFound => bilibi_agent_api::Error::NotFound,
        WriteError::Stale => Error::Conflict.into(),
        WriteError::Invalid(fields) => bilibi_agent_api::Error::InvalidInput(fields),
        WriteError::Access => Error::Forbidden.into(),
        WriteError::Backend(Error::NotFound) => bilibi_agent_api::Error::NotFound,
        WriteError::Backend(error) => error.into(),
    }
}
fn id(input: &Value, key: &str) -> bilibi_agent_api::Result<i64> {
    input[key]
        .as_i64()
        .filter(|id| *id > 0)
        .ok_or(Error::Invalid.into())
}
fn mapping(input: &Value) -> bilibi_agent_api::Result<Mapping> {
    let mapping: Mapping =
        serde_json::from_value(input["mapping"].clone()).map_err(|_| Error::Invalid)?;
    if mapping.fields.len() > MAX_FIELDS {
        return Err(Error::Invalid.into());
    }
    Ok(mapping)
}
fn page(input: &Value) -> (i64, i64) {
    let page = input["page"].as_i64().unwrap_or(1).max(1);
    let size = input["page_size"].as_i64().unwrap_or(25).clamp(1, 100);
    (page, size)
}

enum Read {
    Types,
    List,
    Get,
    Rows,
    Mappings,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/bulk_data"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let bulk = bulk()?;
            let object = bulk.db.get().await.map_err(Error::from)?;
            let client: &tokio_postgres::Client = &object;
            Ok(match self {
                Self::Types => {
                    let types = bulk.types(client, scope).await?;
                    Output::Data(Data::Json(Value::Array(types.iter().map(|info| json!({
                        "key": info.key,
                        "label": info.label,
                        "fields": info.fields.iter().map(|field| json!({
                            "name": field.name,
                            "label": field.label,
                            "type": format!("{:?}", field.kind).to_lowercase(),
                            "required": field.required,
                            "choices": field.choices,
                            "parent": field.reference.and_then(|r| r.parent),
                            "lookup": field.reference.and_then(|r| r.lookup.as_ref()).map(|lookup| json!({"by": lookup.by, "noun": lookup.noun})),
                        })).collect::<Vec<_>>(),
                        "restricted": info.restricted,
                        "matches": info.matches,
                    })).collect())))
                }
                Self::List => {
                    let (page, size) = page(input);
                    let (runs, total) = bulk.runs(client, scope, (page - 1) * size, size).await?;
                    Output::Page(Page {
                        entries: Data::Json(Value::Array(
                            runs.iter()
                                .map(|run| run.json(bulk.label(&run.record_type)))
                                .collect(),
                        )),
                        page: page as u64,
                        page_size: size as u64,
                        total: total as u64,
                    })
                }
                Self::Get => {
                    let run = bulk.run(client, scope, id(input, "run_id")?).await?;
                    let mut answer = run.json(bulk.label(&run.record_type));
                    let fitting = bulk
                        .mappings(client, scope, &run.record_type, Some(&run.header))
                        .await?;
                    answer["saved_mappings"] = json!(fitting
                        .iter()
                        .map(|saved| json!({"mapping_id": saved.id, "name": saved.name}))
                        .collect::<Vec<_>>());
                    let parents = bulk.parents(client, scope, &run.record_type).await?;
                    answer["parent_imports"] = json!(parents.iter().map(|(field, runs)| json!({"field": field, "runs": runs.iter().map(|run| json!({"run_id": run.id, "file_name": run.file_name, "finished_at": run.finished_at.map(|at| at.to_string())})).collect::<Vec<_>>()})).collect::<Vec<_>>());
                    Output::Data(Data::Json(answer))
                }
                Self::Rows => {
                    let (page, size) = page(input);
                    let pass = input["pass"]
                        .as_str()
                        .and_then(Pass::parse)
                        .unwrap_or(Pass::Run);
                    let outcome = input["outcome"].as_str().and_then(Outcome::parse);
                    let (rows, total) = bulk
                        .rows(
                            client,
                            scope,
                            id(input, "run_id")?,
                            pass,
                            outcome,
                            (page - 1) * size,
                            size,
                        )
                        .await?;
                    Output::Page(Page {
                        entries: Data::Json(json!(rows)),
                        page: page as u64,
                        page_size: size as u64,
                        total: total as u64,
                    })
                }
                Self::Mappings => {
                    let record_type = input["record_type"].as_str().ok_or(Error::Invalid)?;
                    Output::Data(Data::Json(json!(
                        bulk.mappings(client, scope, record_type, None).await?
                    )))
                }
            })
        })
    }
}

enum Write {
    Preview,
    Start,
    Cancel,
    SaveMapping,
    DeleteMapping,
}
impl Write {
    fn action(&self, input: &Value) -> bilibi_agent_api::Result<Action> {
        Ok(match self {
            Self::Preview => Action::Preview {
                run: id(input, "run_id")?,
                version: id(input, "version")?,
                mapping: mapping(input)?,
            },
            Self::Start => Action::Start {
                run: id(input, "run_id")?,
                version: id(input, "version")?,
                mapping: mapping(input)?,
            },
            Self::Cancel => Action::Cancel {
                run: id(input, "run_id")?,
                version: id(input, "version")?,
            },
            Self::SaveMapping => Action::SaveMapping {
                record_type: input["record_type"]
                    .as_str()
                    .ok_or(Error::Invalid)?
                    .to_owned(),
                name: input["name"].as_str().ok_or(Error::Invalid)?.to_owned(),
                mapping: mapping(input)?,
            },
            Self::DeleteMapping => Action::DeleteMapping {
                mapping: id(input, "mapping_id")?,
                version: id(input, "version")?,
            },
        })
    }
}
impl WriteHandler for Write {
    fn owner(&self) -> &'static str {
        "base/bulk_data"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let action = self.action(input)?;
            let bulk = bulk()?;
            let client = bulk.db.get().await.map_err(Error::from)?;
            Ok(bulk
                .review(&**client, scope, &action)
                .await
                .map_err(error)?
                .into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let action = self.action(input)?;
            Ok(bulk()?
                .apply(tx, scope, &action)
                .await
                .map_err(error)?
                .into())
        })
    }
}

fn mapping_schema() -> Value {
    json!({"type": "object", "required": ["fields"], "properties": {
        "fields": {"type": "array", "description": "One entry per field: the column that fills it, or a fixed value. A reference field's column is looked up, or with parent_run names a row of that earlier import by its source key.", "items": {"type": "object", "required": ["field"], "properties": {
            "field": {"type": "string", "maxLength": 100},
            "column": {"type": "string", "maxLength": 255},
            "value": {"type": "string", "maxLength": 1000},
            "parent_run": {"type": "integer", "minimum": 1}
        }}},
        "source_key": {"type": "string", "maxLength": 255, "description": "The column whose value later child imports use to name this run's rows."},
        "duplicates": {"type": "string", "enum": ["skip", "update", "create"], "description": "A row matching one existing record: leave it (skip, the default), change it (update) or create a new one anyway (create). Several matches are always an error."}
    }})
}
fn id_schema() -> Value {
    json!({"type": "integer", "minimum": 1})
}
fn no_input() -> Value {
    json!({"type": "object", "properties": {}})
}
fn list_schema() -> Value {
    json!({"type": "object", "properties": {"page": {"type": "integer", "minimum": 1, "maximum": 100000}, "page_size": {"type": "integer", "minimum": 1, "maximum": 100}}})
}
fn get_schema() -> Value {
    json!({"type": "object", "required": ["run_id"], "properties": {"run_id": id_schema()}})
}
fn rows_schema() -> Value {
    json!({"type": "object", "required": ["run_id"], "properties": {
        "run_id": id_schema(),
        "pass": {"type": "string", "enum": ["preview", "run"]},
        "outcome": {"type": "string", "enum": ["create", "update", "skip", "error"]},
        "page": {"type": "integer", "minimum": 1, "maximum": 100000},
        "page_size": {"type": "integer", "minimum": 1, "maximum": 100}
    }})
}
fn mappings_schema() -> Value {
    json!({"type": "object", "required": ["record_type"], "properties": {"record_type": {"type": "string", "maxLength": 200}}})
}
fn pass_schema() -> Value {
    json!({"type": "object", "required": ["run_id", "version", "mapping"], "properties": {"run_id": id_schema(), "version": id_schema(), "mapping": mapping_schema()}})
}
fn cancel_schema() -> Value {
    json!({"type": "object", "required": ["run_id", "version"], "properties": {"run_id": id_schema(), "version": id_schema()}})
}
fn save_schema() -> Value {
    json!({"type": "object", "required": ["record_type", "name", "mapping"], "properties": {"record_type": {"type": "string", "maxLength": 200}, "name": {"type": "string", "maxLength": 120}, "mapping": mapping_schema()}})
}
fn delete_schema() -> Value {
    json!({"type": "object", "required": ["mapping_id", "version"], "properties": {"mapping_id": id_schema(), "version": id_schema()}})
}
fn run_output() -> Value {
    json!({"type": "object", "description": "An import run: file name, record type, columns, rows, state (draft, previewing, previewed, running, completed, cancelled or stopped), progress, mapping, the dry run's and the real run's counts (create, update, skip, error), trace ID and version. Never the file's data."})
}
fn list_output() -> Value {
    json!({"type": "array", "description": "Import runs or row results."})
}

const GUIDE: &str = "# Importing records from CSV\n\nA person uploads a CSV file on the Imports page (`/imports/new`), choosing what the rows are (`base.bulk_data.types` lists the record types they may import, with the fields a column can fill). That opens a run in state `draft`. Agents cannot upload files yet.\n\n1. Read the run with `base.bulk_data.import.get`: its `columns` are the file's header, `saved_mappings` the mappings saved for this record type whose columns the file has (`base.bulk_data.mapping.list` gives their content), and `parent_imports` the person's completed imports a reference field may name.\n2. Build a mapping: for each field, `column` (a header name) or a fixed `value`. A required field must be mapped. A reference field's column is either looked up (the type says by what) or, with `parent_run`, names a row of that earlier import by its source key. `source_key` names the column a later child import will use. `duplicates` says what a row matching one existing record does: `skip` (default), `update` or `create`.\n3. Plan `base.bulk_data.import.preview` with the run's `version` and the mapping. After commit the dry run works in the background and writes nothing; read the run until its state is `previewed`, then `base.bulk_data.import.rows` with `pass: preview` (and `outcome: error`) says what each row would do and why a row fails.\n4. Plan `base.bulk_data.import.start` with the current version and mapping. Rows are written through the record type's own contracts as the person, each in its own transaction, under one trace ID. Read the run until `completed`; the person gets a notice.\n\nA run in `previewing` or `running` can be stopped with `base.bulk_data.import.cancel`: rows already written stay. State `stopped` means the job ended without finishing (the person lost access, or it failed every attempt): cancel it and start again. The file of failed rows downloads from `/imports/{id}/failed-rows` (`?pass=preview` for the dry run's).\n\nSave a mapping for reuse with `base.bulk_data.mapping.save` (by name, shared in the company; saving a name again replaces it).";

macro_rules! provenance {
    () => {
        crate::provenance!()
    };
}
inventory::submit! { Contribution::new(DEFINITIONS, "base/bulk_data.agent", provenance!(), &Definitions {
    operations: &[
        Operation { key: "base.bulk_data.types", title: "List importable record types", summary: "Record types you may import from CSV, with the fields a column can fill, which fields refer to other records, and what rows are matched on.", keywords: &["import", "csv", "upload", "fields", "columns"], capability: VIEW, input: no_input, output: list_output, handler: Handler::Read(&Read::Types), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.import.list", title: "List your imports", summary: "Your CSV import runs, newest first, with state, progress and counts.", keywords: &["import", "csv", "runs"], capability: VIEW, input: list_schema, output: list_output, handler: Handler::Read(&Read::List), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.import.get", title: "Read an import", summary: "One of your import runs: its columns, mapping, state, counts, version, the saved mappings that fit its file and the imports it may name as parents.", keywords: &["import", "csv", "mapping"], capability: VIEW, input: get_schema, output: run_output, handler: Handler::Read(&Read::Get), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.import.rows", title: "Read an import's row results", summary: "What each row of a dry run (pass preview) or the real run did or would do: create, update, skip or error with the reason, the record it wrote and its source key.", keywords: &["import", "errors", "results"], capability: VIEW, input: rows_schema, output: list_output, handler: Handler::Read(&Read::Rows), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.mapping.list", title: "List saved import mappings", summary: "Mappings saved for a record type in your company: which column fills each field, fixed values and the duplicate rule.", keywords: &["import", "mapping", "template"], capability: VIEW, input: mappings_schema, output: list_output, handler: Handler::Read(&Read::Mappings), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.import.preview", title: "Check an import", summary: "Map a run's columns and check every row against the record type's own rules in the background, writing nothing. Replaces an earlier check's results.", keywords: &["import", "dry run", "validate"], capability: CREATE, input: pass_schema, output: run_output, handler: Handler::SafeWrite(&Write::Preview), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.import.start", title: "Run an import", summary: "Map a run's columns and import its rows in the background, each through the record type's own write contract as you.", keywords: &["import", "csv", "load"], capability: CREATE, input: pass_schema, output: run_output, handler: Handler::SafeWrite(&Write::Start), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.import.cancel", title: "Stop an import", summary: "Stop a check or an import that is working. Rows already written stay and are listed.", keywords: &["import", "cancel", "stop"], capability: CREATE, input: cancel_schema, output: run_output, handler: Handler::SafeWrite(&Write::Cancel), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.mapping.save", title: "Save an import mapping", summary: "Save a mapping by name for a record type, shared in your company and offered when a file's header has its columns. Saving a name again replaces it.", keywords: &["import", "mapping", "template"], capability: CREATE, input: save_schema, output: list_output, handler: Handler::SafeWrite(&Write::SaveMapping), channels: Channels::Every, step_up: &[] },
        Operation { key: "base.bulk_data.mapping.delete", title: "Delete a saved import mapping", summary: "Delete a mapping saved for a record type in your company.", keywords: &["import", "mapping"], capability: CREATE, input: delete_schema, output: list_output, handler: Handler::SafeWrite(&Write::DeleteMapping), channels: Channels::Every, step_up: &[] },
    ],
    guides: &[Guide { key: "base.bulk_data", title: "Importing records from CSV", summary: "Mapping an uploaded file, the dry run, the real run, duplicates, parent and child imports and saved mappings.", keywords: &["import", "csv", "bulk"], capability: VIEW, body: GUIDE }],
}) }
