//! CSV import for any module that opts in (issue 133). No Bilimbi or
//! Belimbing counterpart: neither imports records in bulk.
//!
//! A module contributes a `RecordType` naming its write contracts; a person
//! uploads a CSV file, maps its columns to the create contract's fields,
//! checks it in a dry run and runs it in the background. Every row is one
//! call to the owner's contract as that person, from a queued job
//! (`bilibi_agent_api::job`, ADR 10), each in its own short transaction with
//! its result recorded beside it, so the owner's validation, permissions and
//! audit apply as when the person edits one record. An export takes a list's
//! filtered records and shown columns out to CSV the same way, reading as the
//! person (`export`). Bulk updates are a later phase.
mod agent;
mod contributions;
mod csv;
mod export;
mod files;
mod import;
mod mapping;
mod record;
mod store;
mod web;

pub use export::{ExportRun, ExportState, EXPORTED};
pub use files::{RunFiles, ARTIFACT_OWNER, FILES};
pub use mapping::{FieldMap, Mapping, Policy};
pub use record::{
    Duplicates, Export, Field, Import, Kind, Lookup, RecordType, Reference, RECORD_TYPES,
};
pub use store::{Counts, Outcome, Pass, RowResult, Run, RunState, SavedMapping};

use bilibi_agent_api::job::JobContracts;
use bilibi_artifacts::Artifacts;
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_notifications::NotificationBoundary;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::Queue;
use bilibi_settings::Settings;
use bilibi_tenancy::{PartyProofs, Scope};
use mapping::FieldErrors;
use record::Registered;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio::sync::broadcast;
use tokio_postgres::GenericClient;

pub const SERVICE: &str = "base/bulk_data.imports";
/// Reach the Imports pages and read one's own runs and the saved mappings.
pub const VIEW: &str = "admin.import.view";
/// Start, check and cancel one's own imports and save mappings. Each record
/// type's own import capability, and each contract's, still apply.
pub const CREATE: &str = "admin.import.create";
/// The largest file accepted, in bytes.
pub const MAX_BYTES: &str = "bulk_data.import_max_bytes";
/// The most data rows one file may hold.
pub const MAX_ROWS: &str = "bulk_data.import_max_rows";
/// The most records one export takes.
pub const EXPORT_MAX_ROWS: &str = "bulk_data.export_max_rows";
/// Days an uploaded import file and an exported file are kept.
pub const FILE_DAYS: &str = "bulk_data.file_days";
pub(crate) const WORKER: &str = "base/bulk_data-import";

/// A record type the person may import, with what they may fill.
#[derive(Clone, Debug)]
pub struct TypeInfo {
    pub key: &'static str,
    pub label: &'static str,
    pub path: Option<&'static str>,
    /// Fields a column or fixed value may fill.
    pub fields: Vec<Field>,
    /// Fields of the contract restricted for this person: not offered.
    pub restricted: Vec<String>,
    /// The fields a row is matched on, when the type finds duplicates.
    pub matches: Vec<&'static str>,
}

/// A refused write, typed for the contracts and pages.
#[derive(Debug)]
pub enum WriteError {
    NotFound,
    /// The run or mapping changed since it was read: reload.
    Stale,
    Invalid(FieldErrors),
    Access,
    Backend(Error),
}
impl From<Error> for WriteError {
    fn from(error: Error) -> Self {
        match error {
            Error::NotFound => Self::NotFound,
            Error::Forbidden => Self::Access,
            Error::Conflict => Self::Stale,
            error => Self::Backend(error),
        }
    }
}
fn refused(key: &str, message: &str) -> WriteError {
    WriteError::Invalid(FieldErrors::from([(
        key.to_owned(),
        vec![message.to_owned()],
    )]))
}

/// A reviewed write the contracts carry: preview it, then apply it.
#[derive(Clone, Debug)]
pub enum Action {
    /// Map the run and check every row, writing nothing (the dry run).
    Preview {
        run: i64,
        version: i64,
        mapping: Mapping,
    },
    /// Map the run and import its rows.
    Start {
        run: i64,
        version: i64,
        mapping: Mapping,
    },
    /// Stop a run that is working; rows already written stay.
    Cancel {
        run: i64,
        version: i64,
    },
    /// Save a mapping by name for a record type, shared in the company.
    SaveMapping {
        record_type: String,
        name: String,
        mapping: Mapping,
    },
    DeleteMapping {
        mapping: i64,
        version: i64,
    },
    /// Take a list's filtered records (or the ticked ones), with the columns
    /// it shows, out to CSV.
    Export {
        record_type: String,
        /// The list contract's filters, without `page` or `page_size`;
        /// empty when `ids` names the records.
        filter: Value,
        /// Ticked records' ids; empty for every record the filters match.
        ids: Vec<String>,
        /// Grid column specs, in the order shown.
        columns: Vec<String>,
    },
}

pub struct BulkData {
    db: Db,
    boundary: Arc<MutationBoundary>,
    settings: Arc<Settings>,
    artifacts: Arc<Artifacts>,
    queue: Arc<Queue>,
    jobs: Arc<JobContracts>,
    notifications: Arc<NotificationBoundary>,
    types: BTreeMap<&'static str, Registered>,
    changes: broadcast::Sender<bilibi_ui::page::Change>,
}

fn company(scope: &Scope) -> std::result::Result<OrganisationId, WriteError> {
    scope.actor().organisation().ok_or_else(|| {
        refused(
            "company",
            "Your account is not filed under a company, so it cannot import.",
        )
    })
}

/// Content-free audit evidence of a run.
fn evidence(run: &Run) -> Value {
    json!({"record_type": run.record_type, "rows": run.row_count, "state": run.state.as_str(), "pass": run.pass.map(Pass::as_str), "version": run.version})
}

impl BulkData {
    /// Wakes the person's open import pages after a commit.
    pub fn subscribe(&self) -> broadcast::Receiver<bilibi_ui::page::Change> {
        self.changes.subscribe()
    }
    fn wake(&self, tenant: i64, user: i64) {
        let _ = self.changes.send(bilibi_ui::page::Change {
            tenant,
            user: Some(user),
        });
    }
    fn registered(&self, key: &str) -> std::result::Result<&Registered, WriteError> {
        self.types
            .get(key)
            .ok_or_else(|| refused("record_type", "is not a record type that can be imported"))
    }
    /// What a record type's records are called; its key when unmounted.
    pub fn label<'a>(&'a self, key: &'a str) -> &'a str {
        self.types
            .get(key)
            .map_or(key, |registered| registered.definition.label)
    }

    /// The fields this person may fill, and those restricted for them.
    async fn offered(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        registered: &Registered,
    ) -> Result<(Vec<Field>, Vec<Field>)> {
        let restricted =
            bilibi_authz::restrictions_for(client, scope, registered.definition.table).await?;
        Ok(registered
            .fields
            .iter()
            .cloned()
            .partition(|field| !restricted.contains(&field.name)))
    }
    async fn info(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        registered: &Registered,
    ) -> Result<TypeInfo> {
        let (fields, restricted) = self.offered(client, scope, registered).await?;
        Ok(TypeInfo {
            key: registered.key,
            label: registered.definition.label,
            path: registered.definition.path,
            fields,
            restricted: restricted.into_iter().map(|field| field.name).collect(),
            matches: registered
                .definition
                .import
                .duplicates
                .as_ref()
                .map(|duplicates| duplicates.fields.to_vec())
                .unwrap_or_default(),
        })
    }

    /// The record types this person may import, by label.
    pub async fn types(&self, client: &impl GenericClient, scope: &Scope) -> Result<Vec<TypeInfo>> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        let held = bilibi_authz::effective_capabilities(client, scope).await?;
        let mut types = Vec::new();
        for registered in self.types.values() {
            if held.contains(registered.definition.import.capability) {
                types.push(self.info(client, scope, registered).await?);
            }
        }
        types.sort_by(|a, b| a.label.cmp(b.label));
        Ok(types)
    }
    /// One record type the person may import.
    pub async fn record_type(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        key: &str,
    ) -> Result<TypeInfo> {
        let registered = self.types.get(key).ok_or(Error::NotFound)?;
        bilibi_authz::authorize_key(client, scope, registered.definition.import.capability).await?;
        self.info(client, scope, registered).await
    }

    /// Stores a file the person chose and opens its run, ready to map:
    /// `Ok(Err(why))` when the file is refused, in words for the page.
    /// The page's `upload` event calls this outside a contract (a listed
    /// allowance): Artifacts takes bytes on a plain connection, and a plan's
    /// input is bounded at 1 MiB.
    pub async fn upload(
        &self,
        scope: &Scope,
        record_type: &str,
        file_name: &str,
        bytes: &[u8],
    ) -> Result<std::result::Result<i64, String>> {
        let registered = self.types.get(record_type).ok_or(Error::NotFound)?;
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        bilibi_authz::authorize_key(&*client, scope, CREATE).await?;
        bilibi_authz::authorize_key(&*client, scope, registered.definition.import.capability)
            .await?;
        let Some(company) = scope.actor().organisation() else {
            return Ok(Err(
                "Your account is not filed under a company, so it cannot import.".into(),
            ));
        };
        let max_bytes = self
            .settings
            .get_global(&*client, MAX_BYTES)
            .await?
            .as_u64()
            .ok_or(Error::Internal)?;
        if bytes.len() as u64 > max_bytes {
            return Ok(Err(format!(
                "The file is larger than {}, the most an import accepts.",
                size(max_bytes)
            )));
        }
        let max_rows = self
            .settings
            .get_global(&*client, MAX_ROWS)
            .await?
            .as_u64()
            .ok_or(Error::Internal)?;
        let parsed = match csv::parse(bytes, max_rows as usize) {
            Ok(parsed) => parsed,
            Err(why) => return Ok(Err(why.message())),
        };
        if parsed.rows.is_empty() {
            return Ok(Err("The file has a header but no rows.".into()));
        }
        let name: String = file_name
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| !c.is_control())
            .take(255)
            .collect();
        let name = if name.trim().is_empty() {
            "import.csv".to_owned()
        } else {
            name
        };
        let rows = parsed.rows.len() as i32;
        let header = json!(parsed.header);
        let mut tx = self
            .boundary
            .begin(
                client,
                scope.mutation_context("base.bulk_data.import.upload"),
            )
            .await?;
        let (tenant, user, kind) = (scope.tenant(), scope.user(), registered.key);
        let id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("INSERT INTO base_bulk_data_runs(tenant_id,company_id,record_type,started_by,file_name,header,row_count) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING {}", store::COLUMNS), &[&tenant, &company.get(), &kind, &user, &name, &header, &rows]).await?;
                    let run = Run::from_row(&row)?;
                    Ok((run.id, vec![Change { kind: "base/bulk_data.run".into(), id: run.id, old: Value::Null, new: evidence(&run) }]))
                })
            })
            .await?;
        tx.commit().await?;
        self.purge(client, scope, company).await;
        let days = self
            .settings
            .get_global(&*client, FILE_DAYS)
            .await?
            .as_i64()
            .ok_or(Error::Internal)?;
        let stored = self
            .artifacts
            .for_owner(scope, company, &FILES)
            .put_expiring(
                client,
                bilibi_artifacts::Reference {
                    subject: id.to_string(),
                    kind: files::SOURCE.into(),
                },
                bytes,
                "text/csv",
                days,
            )
            .await;
        let artifact = match stored {
            Ok(metadata) => metadata.id,
            Err(bilibi_artifacts::Error::Invalid) => return Ok(Err(
                "The file is larger than private documents may be. Split it into smaller files."
                    .into(),
            )),
            Err(bilibi_artifacts::Error::Database(error)) => return Err(error),
            Err(error) => {
                // A content-free code: the folder is missing or unsafe, or the
                // disk refused the write.
                eprintln!(
                    "error: an import file could not be stored as a private document: {}",
                    error.code()
                );
                return Ok(Err(
                    "The file could not be stored: private documents are not available on this server. Ask your administrator."
                        .into(),
                ));
            }
        };
        let mut tx = self
            .boundary
            .begin(
                client,
                scope.mutation_context("base.bulk_data.import.upload"),
            )
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE base_bulk_data_runs SET source_artifact=$3,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 AND source_artifact IS NULL RETURNING {}", store::COLUMNS), &[&tenant, &id, &artifact]).await?;
                let run = Run::from_row(&row)?;
                Ok(((), vec![Change { kind: "base/bulk_data.run".into(), id, old: Value::Null, new: json!({"source": "stored", "version": run.version}) }]))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(Ok(id))
    }

    /// The largest file and the most rows one import accepts.
    pub async fn limits(&self, client: &(impl GenericClient + Sync)) -> Result<(u64, u64)> {
        let get = |key| async move {
            self.settings
                .get_global(client, key)
                .await?
                .as_u64()
                .ok_or(Error::Internal)
        };
        Ok((get(MAX_BYTES).await?, get(MAX_ROWS).await?))
    }

    /// The person's own run.
    pub async fn run(&self, client: &impl GenericClient, scope: &Scope, id: i64) -> Result<Run> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        store::load(client, scope, id, false).await
    }
    /// The person's runs, newest first.
    pub async fn runs(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<Run>, i64)> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        store::list(client, scope, offset, limit.clamp(1, 100)).await
    }
    /// One pass's row results of the person's run.
    #[allow(clippy::too_many_arguments)]
    pub async fn rows(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        run: i64,
        pass: Pass,
        outcome: Option<Outcome>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<RowResult>, i64)> {
        let run = self.run(client, scope, run).await?;
        store::rows(
            client,
            scope.tenant(),
            run.id,
            pass,
            outcome,
            offset.max(0),
            limit.clamp(1, 100),
        )
        .await
    }
    /// Mappings saved for a record type in the person's company; with a
    /// header, only those whose columns the file has.
    pub async fn mappings(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        record_type: &str,
        header: Option<&[String]>,
    ) -> Result<Vec<SavedMapping>> {
        let registered = self.types.get(record_type).ok_or(Error::NotFound)?;
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        bilibi_authz::authorize_key(client, scope, registered.definition.import.capability).await?;
        let Some(company) = scope.actor().organisation() else {
            return Ok(Vec::new());
        };
        let mut saved = store::mappings(client, scope, company.get(), record_type).await?;
        if let Some(header) = header {
            saved.retain(|saved| saved.mapping.fits(header));
        }
        Ok(saved)
    }
    /// The person's completed imports a field of this record type may name
    /// as its parent, by field.
    pub async fn parents(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        record_type: &str,
    ) -> Result<BTreeMap<&'static str, Vec<Run>>> {
        let registered = self.types.get(record_type).ok_or(Error::NotFound)?;
        let mut parents = BTreeMap::new();
        let Some(company) = scope.actor().organisation() else {
            return Ok(parents);
        };
        for reference in registered.definition.import.references {
            if let Some(parent) = reference.parent {
                parents.insert(
                    reference.field,
                    store::completed(client, scope, company.get(), &[parent]).await?,
                );
            }
        }
        Ok(parents)
    }

    /// The file of a pass's failed rows: the file's own columns, then why.
    pub async fn failed_rows(
        &self,
        scope: &Scope,
        id: i64,
        pass: Pass,
    ) -> Result<(String, Vec<u8>)> {
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        let run = self.run(&*client, scope, id).await?;
        let source = run.source.clone().ok_or(Error::NotFound)?;
        let company = OrganisationId::new(run.company).map_err(|_| Error::Internal)?;
        let document = self
            .artifacts
            .for_owner(scope, company, &FILES)
            .read(client, &source)
            .await
            .map_err(artifact)?;
        let parsed = csv::parse(&document.bytes, usize::MAX).map_err(|_| Error::Internal)?;
        let failed = client
            .query(
                "SELECT row_number,message FROM base_bulk_data_rows WHERE tenant_id=$1 AND run_id=$2 AND pass=$3 AND outcome='error' ORDER BY row_number",
                &[&scope.tenant(), &run.id, &pass.as_str()],
            )
            .await?;
        let rows = failed.iter().filter_map(|row| {
            let index = usize::try_from(row.get::<_, i32>(0)).ok()?.checked_sub(1)?;
            let mut cells = parsed.rows.get(index)?.clone();
            cells.push(row.get::<_, Option<String>>(1).unwrap_or_default());
            Some(cells)
        });
        let column = if parsed.header.iter().any(|name| name == "Error") {
            "Import error"
        } else {
            "Error"
        };
        let bytes = csv::write(
            parsed
                .header
                .iter()
                .map(String::as_str)
                .chain(std::iter::once(column)),
            rows,
        );
        let stem = run
            .file_name
            .strip_suffix(".csv")
            .or_else(|| run.file_name.strip_suffix(".CSV"))
            .unwrap_or(&run.file_name);
        let suffix = match pass {
            Pass::Preview => "dry-run-errors",
            Pass::Run => "failed-rows",
        };
        Ok((format!("{stem}-{suffix}.csv"), bytes))
    }

    /// The review of a write: what it will do, writing nothing.
    pub async fn review(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        action: &Action,
    ) -> std::result::Result<Value, WriteError> {
        if let Action::Export {
            record_type,
            filter,
            ids,
            columns,
        } = action
        {
            self.ready_export(client, scope, record_type, filter, ids, columns)
                .await?;
            let label = self.label(record_type).to_lowercase();
            let what = if ids.is_empty() {
                label
            } else {
                format!("{} ticked {label}", ids.len())
            };
            return Ok(
                json!({"summary": format!("Export {what} with the shown columns to CSV"), "record_type": record_type, "columns": columns, "filter": filter, "ids": ids}),
            );
        }
        bilibi_authz::authorize_key(client, scope, CREATE).await?;
        match action {
            Action::Preview {
                run,
                version,
                mapping,
            }
            | Action::Start {
                run,
                version,
                mapping,
            } => {
                let pass = if matches!(action, Action::Preview { .. }) {
                    Pass::Preview
                } else {
                    Pass::Run
                };
                let run = store::load(client, scope, *run, false).await?;
                self.ready(client, scope, &run, *version, mapping).await?;
                let label = self.label(&run.record_type);
                let summary = match pass {
                    Pass::Preview => format!(
                        "Check {} rows of {} as {label}; nothing is written",
                        run.row_count, run.file_name
                    ),
                    Pass::Run => format!(
                        "Import {} rows of {} as {label}",
                        run.row_count, run.file_name
                    ),
                };
                Ok(
                    json!({"summary": summary, "run_id": run.id, "version": run.version, "rows": run.row_count, "pass": pass.as_str(), "mapping": mapping}),
                )
            }
            Action::Cancel { run, version } => {
                let run = store::load(client, scope, *run, false).await?;
                Self::cancellable(&run, *version)?;
                Ok(
                    json!({"summary": format!("Stop {} of {}; rows already written stay", if run.state == RunState::Running { "the import" } else { "the check" }, run.file_name), "run_id": run.id, "version": run.version, "done": run.next_row, "rows": run.row_count}),
                )
            }
            Action::SaveMapping {
                record_type,
                name,
                mapping,
            } => {
                let (name, existing) = self
                    .saveable(client, scope, record_type, name, mapping)
                    .await?;
                Ok(
                    json!({"summary": if existing.is_some() { format!("Replace the saved mapping {name}") } else { format!("Save the mapping {name}") }, "record_type": record_type, "name": name, "columns": mapping.columns(), "replaces": existing.is_some()}),
                )
            }
            // Answered before the import capability is asked.
            Action::Export { .. } => Err(WriteError::Backend(Error::Internal)),
            Action::DeleteMapping { mapping, version } => {
                let saved = self.deletable(client, scope, *mapping, *version).await?;
                Ok(
                    json!({"summary": format!("Delete the saved mapping {}", saved.name), "mapping_id": saved.id, "version": saved.version}),
                )
            }
        }
    }

    /// The write, rechecked and applied on the caller's transaction.
    pub async fn apply(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        action: &Action,
    ) -> std::result::Result<Value, WriteError> {
        let started = match action {
            Action::Export {
                record_type,
                filter,
                ids,
                columns,
            } => Some(json!(
                self.start_export(tx, scope, record_type, filter, ids, columns)
                    .await?
            )),
            _ => None,
        };
        if let Some(answer) = started {
            let changes = self.changes.clone();
            let (tenant, user) = (scope.tenant(), scope.user());
            tx.after_commit(move || {
                let _ = changes.send(bilibi_ui::page::Change {
                    tenant,
                    user: Some(user),
                });
            });
            return Ok(answer);
        }
        bilibi_authz::authorize_key(tx.sql(), scope, CREATE).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let answer = match action {
            Action::Preview {
                run,
                version,
                mapping,
            }
            | Action::Start {
                run,
                version,
                mapping,
            } => {
                let pass = if matches!(action, Action::Preview { .. }) {
                    Pass::Preview
                } else {
                    Pass::Run
                };
                let old = store::load(tx.sql(), scope, *run, true).await?;
                self.ready(tx.sql(), scope, &old, *version, mapping).await?;
                let job = self
                    .queue
                    .enqueue_for(
                        tx,
                        scope,
                        WORKER,
                        json!({"run": old.id, "pass": pass.as_str()}),
                    )
                    .await?;
                let trace = tx.context().trace_id.clone();
                let mapping = serde_json::to_value(mapping).map_err(|_| Error::Internal)?;
                let state = match pass {
                    Pass::Preview => "previewing",
                    Pass::Run => "running",
                };
                let id = old.id;
                let label = self.label(&old.record_type).to_owned();
                let run = tx.mutate(move |sql| Box::pin(async move {
                    let cleared = sql.execute("DELETE FROM base_bulk_data_rows WHERE tenant_id=$1 AND run_id=$2 AND pass=$3", &[&tenant, &id, &pass.as_str()]).await?;
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_runs SET mapping=$3,state=$4,pass=$5,next_row=0,job_id=$6,trace_id=$7,finished_at=NULL,version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {}", store::COLUMNS), &[&tenant, &id, &mapping, &state, &pass.as_str(), &job.id(), &trace]).await?;
                    let run = Run::from_row(&row)?;
                    let mut new = evidence(&run);
                    new["cleared_rows"] = json!(cleared);
                    Ok((run, vec![Change { kind: "base/bulk_data.run".into(), id, old: evidence(&old), new }]))
                })).await?;
                run.json(&label)
            }
            Action::Cancel { run, version } => {
                let old = store::load(tx.sql(), scope, *run, true).await?;
                Self::cancellable(&old, *version)?;
                // A cancelled check returns to mapping; a cancelled import ends.
                let state = if old.state == RunState::Previewing {
                    "draft"
                } else {
                    "cancelled"
                };
                let id = old.id;
                let label = self.label(&old.record_type).to_owned();
                let run = tx.mutate(move |sql| Box::pin(async move {
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_runs SET state=$3,pass=CASE WHEN $3='draft' THEN NULL ELSE pass END,finished_at=clock_timestamp(),version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {}", store::COLUMNS), &[&tenant, &id, &state]).await?;
                    let run = Run::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.run".into(), id, old: evidence(&old), new: evidence(&run) }]))
                })).await?;
                run.json(&label)
            }
            Action::SaveMapping {
                record_type,
                name,
                mapping,
            } => {
                let (name, existing) = self
                    .saveable(tx.sql(), scope, record_type, name, mapping)
                    .await?;
                let company = company(scope)?.get();
                let columns = json!(mapping.columns());
                let value = serde_json::to_value(mapping).map_err(|_| Error::Internal)?;
                let record_type = record_type.clone();
                let saved = tx.mutate(move |sql| Box::pin(async move {
                    let row = match &existing {
                        Some(old) => sql.query_one("UPDATE base_bulk_data_mappings SET columns=$3,mapping=$4,version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING id,record_type,name,columns,mapping,version", &[&tenant, &old.id, &columns, &value]).await?,
                        None => sql.query_one("INSERT INTO base_bulk_data_mappings(tenant_id,company_id,record_type,name,columns,mapping,created_by) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id,record_type,name,columns,mapping,version", &[&tenant, &company, &record_type, &name, &columns, &value, &user]).await?,
                    };
                    let saved = store::saved(&row)?;
                    let old = existing.as_ref().map_or(Value::Null, |old| json!({"record_type": old.record_type, "name": old.name, "columns": old.columns, "version": old.version}));
                    Ok((saved.clone(), vec![Change { kind: "base/bulk_data.mapping".into(), id: saved.id, old, new: json!({"record_type": saved.record_type, "name": saved.name, "columns": saved.columns, "version": saved.version}) }]))
                })).await?;
                json!(saved)
            }
            // Applied before the import capability is asked.
            Action::Export { .. } => return Err(WriteError::Backend(Error::Internal)),
            Action::DeleteMapping { mapping, version } => {
                let saved = self.deletable(tx.sql(), scope, *mapping, *version).await?;
                let id = saved.id;
                tx.mutate(move |sql| Box::pin(async move {
                    sql.execute("DELETE FROM base_bulk_data_mappings WHERE tenant_id=$1 AND id=$2", &[&tenant, &id]).await?;
                    Ok(((), vec![Change { kind: "base/bulk_data.mapping".into(), id, old: json!({"record_type": saved.record_type, "name": saved.name, "columns": saved.columns, "version": saved.version}), new: Value::Null }]))
                })).await?;
                json!({"mapping_id": id, "deleted": true})
            }
        };
        let changes = self.changes.clone();
        tx.after_commit(move || {
            let _ = changes.send(bilibi_ui::page::Change {
                tenant,
                user: Some(user),
            });
        });
        Ok(answer)
    }

    /// The run is the person's, unchanged, waiting for a pass, its file is
    /// stored, the type is theirs to import and the mapping fits.
    async fn ready(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        run: &Run,
        version: i64,
        mapping: &Mapping,
    ) -> std::result::Result<(), WriteError> {
        if run.version != version {
            return Err(WriteError::Stale);
        }
        if !run.state.open() {
            return Err(refused(
                "run_id",
                "is not waiting to be mapped: it is being checked or imported, or it has ended",
            ));
        }
        if run.source.is_none() {
            return Err(refused("run_id", "has no stored file; upload it again"));
        }
        let registered = self.registered(&run.record_type)?;
        bilibi_authz::authorize_key(client, scope, registered.definition.import.capability).await?;
        let (offered, restricted) = self.offered(client, scope, registered).await?;
        let restricted: Vec<&Field> = restricted.iter().collect();
        let mut errors = mapping
            .check(
                &offered,
                &restricted,
                Some(&run.header),
                registered.definition.import.duplicates.is_some(),
            )
            .err()
            .unwrap_or_default();
        for entry in &mapping.fields {
            let (Some(parent), Some(reference)) = (
                entry.parent_run,
                offered
                    .iter()
                    .find(|field| field.name == entry.field)
                    .and_then(|field| field.reference),
            ) else {
                continue;
            };
            let kinds: Vec<&str> = reference.parent.into_iter().collect();
            let found = store::completed(client, scope, run.company, &kinds)
                .await?
                .iter()
                .any(|candidate| candidate.id == parent);
            if !found {
                errors
                    .entry(format!("fields.{}", entry.field))
                    .or_default()
                    .push("names a parent import that is not one of your completed imports of that record type".into());
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(WriteError::Invalid(errors))
        }
    }
    fn cancellable(run: &Run, version: i64) -> std::result::Result<(), WriteError> {
        if run.version != version {
            return Err(WriteError::Stale);
        }
        if !run.state.working() {
            return Err(refused("run_id", "is not being checked or imported"));
        }
        Ok(())
    }
    async fn saveable(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        record_type: &str,
        name: &str,
        mapping: &Mapping,
    ) -> std::result::Result<(String, Option<SavedMapping>), WriteError> {
        let registered = self.registered(record_type)?;
        bilibi_authz::authorize_key(client, scope, registered.definition.import.capability).await?;
        let company = company(scope)?.get();
        let name = name.trim().to_owned();
        if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
            return Err(refused("name", "must be 1 to 120 characters"));
        }
        let (offered, restricted) = self.offered(client, scope, registered).await?;
        let restricted: Vec<&Field> = restricted.iter().collect();
        mapping
            .check(
                &offered,
                &restricted,
                None,
                registered.definition.import.duplicates.is_some(),
            )
            .map_err(WriteError::Invalid)?;
        if mapping
            .fields
            .iter()
            .any(|entry| entry.parent_run.is_some())
        {
            return Err(refused(
                "mapping",
                "names a parent import, which belongs to one run; save it without the parent",
            ));
        }
        let existing = client
            .query_opt(
                "SELECT id,record_type,name,columns,mapping,version FROM base_bulk_data_mappings WHERE tenant_id=$1 AND company_id=$2 AND record_type=$3 AND lower(name)=lower($4) FOR UPDATE",
                &[&scope.tenant(), &company, &record_type, &name],
            )
            .await
            .map_err(Error::from)?
            .as_ref()
            .map(store::saved)
            .transpose()?;
        Ok((name, existing))
    }
    async fn deletable(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> std::result::Result<SavedMapping, WriteError> {
        let company = company(scope)?.get();
        let saved = client
            .query_opt(
                "SELECT id,record_type,name,columns,mapping,version FROM base_bulk_data_mappings WHERE tenant_id=$1 AND company_id=$2 AND id=$3 FOR UPDATE",
                &[&scope.tenant(), &company, &id],
            )
            .await
            .map_err(Error::from)?
            .as_ref()
            .map(store::saved)
            .transpose()?
            .ok_or(WriteError::NotFound)?;
        let registered = self.registered(&saved.record_type)?;
        bilibi_authz::authorize_key(client, scope, registered.definition.import.capability).await?;
        if saved.version != version {
            return Err(WriteError::Stale);
        }
        Ok(saved)
    }
}

/// A byte count as people read it: "512 KB", "10 MB".
pub(crate) fn size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 {
        format!("{} MB", bytes.div_ceil(1024 * 1024))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

fn artifact(error: bilibi_artifacts::Error) -> Error {
    match error {
        bilibi_artifacts::Error::Database(error) => error,
        bilibi_artifacts::Error::NotFound => Error::NotFound,
        _ => Error::Internal,
    }
}

macro_rules! provenance {
    () => {
        bilibi_module_registry::Provenance {
            module: "base/bulk_data",
            source: file!(),
            line: line!(),
        }
    };
}
pub(crate) use provenance;

inventory::submit! { bilibi_module_registry::Module {
    id: "base/bulk_data", layer: bilibi_module_registry::Layer::Base,
    dependencies: &["base/module_registry", "base/agent_api", "base/artifacts", "base/authz", "base/database", "base/datetime", "base/grid", "base/locale", "base/menu", "base/notifications", "base/queue", "base/settings", "base/tenancy", "base/ui"],
    migrations: &[
        ("base-bulk-data-001-imports", include_str!("../migrations/001-imports.sql")),
        ("base-bulk-data-002-exports", include_str!("../migrations/002-exports.sql")),
    ],
    services: &[
        bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            let jobs = services.get::<JobContracts>(bilibi_agent_api::job::SERVICE)?;
            let catalog = bilibi_module_registry::Catalog::discover(&bilibi_module_registry::discover()?)?;
            let types = record::register(&catalog, &jobs)?;
            files::bind(services.get::<PartyProofs>("base/tenancy.parties")?)?;
            let bulk = Arc::new(BulkData {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                boundary: services.get("base/database.mutations")?,
                settings: services.get("base/settings.store")?,
                artifacts: services.get("base/artifacts.store")?,
                queue: services.get("base/queue.store")?,
                jobs,
                notifications: services.get("base/notifications.sink")?,
                types,
                changes: broadcast::channel(64).0,
            });
            agent::bind(bulk.clone())?;
            Ok(bulk)
        }},
        bilibi_module_registry::Service { id: import::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(import::ImportJob(services.get(SERVICE)?)))))
        }},
        bilibi_module_registry::Service { id: export::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(export::ExportJob(services.get(SERVICE)?)))))
        }},
        bilibi_module_registry::Service { id: web::PAGES, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages::new(services)?))))
        }},
        bilibi_module_registry::Service { id: web::FILES, factory: |services| {
            Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(files::FailedRows(services.get(SERVICE)?)))))
        }},
        bilibi_module_registry::Service { id: web::EXPORT_FILES, factory: |services| {
            Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(files::ExportFile(services.get(SERVICE)?)))))
        }},
    ],
} }
