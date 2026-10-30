//! The import job: one pass over a run's file as the person who started it.
//! Each row becomes the record type's create (or, for a match, update)
//! contract input; the dry run asks the owner's preview and writes nothing,
//! the real run commits each row in its own short transaction
//! (`Execution::begin`, so the lease, person and trace hold) with the row's
//! result beside it. A pass works in bounded slices: when its time is up it
//! queues its own continuation from the next unrecorded row, so it fits the
//! Queue's attempt timeout, and a retried attempt never records a row twice.
use crate::{
    csv, files::FILES, mapping::convert, record::Registered, store, BulkData, Field, Mapping,
    Outcome, Pass, Policy, Run, WORKER,
};
use bilibi_agent_api::Error as ContractError;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_notifications::Notice;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome as JobOutcome};
use bilibi_tenancy::Scope;
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

pub(crate) const SERVICE: &str = "base/bulk_data.import-job";
/// How long one attempt works before it queues its continuation.
const SLICE: Duration = Duration::from_secs(120);
/// Dry-run results recorded per transaction.
const BATCH: usize = 50;

pub(crate) struct ImportJob(pub Arc<BulkData>);

impl Handler for ImportJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let (Some(run), Some(pass)) = (
                args["run"].as_i64(),
                args["pass"].as_str().and_then(Pass::parse),
            ) else {
                return JobOutcome::Cancel("worker_args_refused");
            };
            match self.0.work(execution, run, pass).await {
                Ok(()) => JobOutcome::Completed,
                Err(Error::Unauthorized | Error::Forbidden) => JobOutcome::Cancel("actor_refused"),
                Err(Error::NotFound) => JobOutcome::Cancel("run_missing"),
                Err(Error::Conflict) => JobOutcome::Retry("lease_lost"),
                Err(_) => JobOutcome::Retry("import_unavailable"),
            }
        })
    }
}

/// Queue's argument check: a run id and a pass, nothing else.
pub(crate) fn arguments(args: Value) -> Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    let valid = object.len() == 2
        && object
            .get("run")
            .and_then(Value::as_i64)
            .is_some_and(|id| id > 0)
        && object
            .get("pass")
            .and_then(Value::as_str)
            .and_then(Pass::parse)
            .is_some();
    if valid {
        Ok(args)
    } else {
        Err(Error::Invalid)
    }
}

/// What one row will do.
enum Planned {
    Write {
        contract: &'static str,
        input: Value,
        outcome: Outcome,
    },
    Skip(String),
    Refused(String),
}

/// One row's recorded result.
struct Recorded {
    row: i32,
    outcome: Outcome,
    record: Option<String>,
    key: Option<String>,
    message: Option<String>,
}

/// One pass's working state.
struct Work<'a> {
    bulk: &'a BulkData,
    execution: &'a Execution,
    scope: Scope,
    registered: &'a Registered,
    mapping: Mapping,
    header: Vec<String>,
    offered: Vec<Field>,
    run: i64,
    pass: Pass,
    company: i64,
    found: HashMap<(String, String), std::result::Result<String, String>>,
}

/// A refusal one row can carry, in words; `None` when the cause is the
/// system (the database), so the attempt stops and is retried.
fn refusal(error: &ContractError, offered: &[Field]) -> Option<String> {
    Some(match error {
        ContractError::InvalidInput(fields) => fields
            .iter()
            .map(|(path, messages)| {
                let name = path.rsplit('.').next().unwrap_or(path);
                let label = offered
                    .iter()
                    .find(|field| field.name == name)
                    .map_or(path.as_str(), |field| field.label.as_str());
                format!("{label}: {}", messages.join(", "))
            })
            .collect::<Vec<_>>()
            .join("; "),
        ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => {
            "You do not have permission to make this change.".into()
        }
        ContractError::NotFound | ContractError::Backend(Error::NotFound) => {
            "A record it names was not found.".into()
        }
        ContractError::Backend(Error::Conflict) => {
            "The record changed after it was matched, so it was not overwritten.".into()
        }
        ContractError::Backend(Error::Invalid) => "The record was refused.".into(),
        ContractError::Backend(Error::StepUpRequired { .. }) => {
            "This change needs a confirmation code, which an import cannot give.".into()
        }
        ContractError::UnknownOperation
        | ContractError::WritesNotEnabled
        | ContractError::ChannelNotAllowed => {
            "This record type can no longer be imported this way.".into()
        }
        _ => return None,
    })
}

fn bounded(text: String) -> String {
    if text.len() <= 2000 {
        return text;
    }
    let mut end = 1996;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &text[..end])
}

impl Work<'_> {
    fn cell<'c>(&self, cells: &'c [String], column: &str) -> &'c str {
        let text = self
            .header
            .iter()
            .position(|name| name == column)
            .and_then(|index| cells.get(index))
            .map_or("", String::as_str);
        csv::unguard(text)
    }

    /// The row as the create contract's input, or why it cannot be.
    async fn plan(&mut self, client: &tokio_postgres::Client, cells: &[String]) -> Result<Planned> {
        let mut input = Map::new();
        let mut problems = Vec::new();
        for entry in self.mapping.fields.clone() {
            let Some(field) = self
                .offered
                .iter()
                .find(|field| field.name == entry.field)
                .cloned()
            else {
                continue;
            };
            let text = match (&entry.column, &entry.value) {
                (Some(column), _) => self.cell(cells, column).to_owned(),
                (None, Some(value)) => value.clone(),
                (None, None) => continue,
            };
            let value = match (field.reference, entry.parent_run, &entry.column) {
                (Some(_), Some(parent), Some(_)) => {
                    self.parent(client, &field, parent, &text).await?
                }
                (Some(reference), None, Some(_)) if reference.lookup.is_some() => {
                    self.lookup(client, &field, &text).await?
                }
                _ => convert(&field, &text),
            };
            match value {
                Ok(Some(value)) => {
                    input.insert(field.name.clone(), value);
                }
                Ok(None) => {}
                Err(message) => problems.push(format!("{}: {message}", field.label)),
            }
        }
        if !problems.is_empty() {
            return Ok(Planned::Refused(problems.join("; ")));
        }
        let import = &self.registered.definition.import;
        let create = |input: Map<String, Value>| Planned::Write {
            contract: import.create,
            input: Value::Object(input),
            outcome: Outcome::Create,
        };
        let Some(duplicates) = &import.duplicates else {
            return Ok(create(input));
        };
        if self.mapping.duplicates == Policy::Create {
            return Ok(create(input));
        }
        let mut query: Map<String, Value> = duplicates
            .fields
            .iter()
            .filter_map(|name| Some(((*name).to_owned(), input.get(*name)?.clone())))
            .collect();
        if query.is_empty() {
            return Ok(create(input));
        }
        if self.registered.limited.get(duplicates.read) == Some(&true) {
            query.insert("limit".into(), json!(2));
        }
        let answer = match self
            .bulk
            .jobs
            .read(
                client,
                self.execution,
                duplicates.read,
                &Value::Object(query),
            )
            .await
        {
            Ok(answer) => answer,
            Err(error) => {
                return match refusal(&error, &self.offered) {
                    Some(message) => Ok(Planned::Refused(format!(
                        "Could not look for duplicates: {message}"
                    ))),
                    None => Err(error_of(error)),
                }
            }
        };
        let candidates = answer["result"].as_array().cloned().unwrap_or_default();
        let id_key = import.id;
        match candidates.as_slice() {
            [] => Ok(create(input)),
            [candidate] => {
                let id = text_id(&candidate[id_key]).ok_or(Error::Internal)?;
                if self.mapping.duplicates == Policy::Skip {
                    return Ok(Planned::Skip(id));
                }
                let mut fields = Map::new();
                for name in &self.registered.update_fields {
                    let value = input.get(name).cloned().or_else(|| {
                        candidate
                            .get(name)
                            .filter(|value| !value.is_null() && value.get("restricted").is_none())
                            .cloned()
                    });
                    if let Some(value) = value {
                        fields.insert(name.clone(), value);
                    }
                }
                let mut update = Map::new();
                update.insert(duplicates.update_id.into(), candidate[id_key].clone());
                update.insert(duplicates.update_version.into(), candidate["version"].clone());
                if duplicates.update_fields.is_empty() {
                    update.extend(fields);
                } else {
                    update.insert(duplicates.update_fields.into(), Value::Object(fields));
                }
                Ok(Planned::Write {
                    contract: duplicates.update,
                    input: Value::Object(update),
                    outcome: Outcome::Update,
                })
            }
            _ => Ok(Planned::Refused(
                "Matches more than one existing record; a row may match only one. Fix the file or import with \"Create anyway\".".into(),
            )),
        }
    }

    /// A row of the parent import, by its source key.
    async fn parent(
        &mut self,
        client: &tokio_postgres::Client,
        field: &Field,
        parent: i64,
        text: &str,
    ) -> Result<std::result::Result<Option<Value>, String>> {
        let key = text.trim();
        if key.is_empty() {
            return Ok(Ok(None));
        }
        let rows = client
            .query(
                "SELECT r.record_id FROM base_bulk_data_rows r JOIN base_bulk_data_runs p ON p.tenant_id=r.tenant_id AND p.id=r.run_id WHERE r.tenant_id=$1 AND r.run_id=$2 AND r.pass='run' AND r.source_key=$3 AND r.record_id IS NOT NULL AND p.started_by=$4 AND p.company_id=$5 AND p.state='completed' LIMIT 2",
                &[&self.scope.tenant(), &parent, &key, &self.scope.user(), &self.company],
            )
            .await?;
        Ok(match rows.as_slice() {
            [] => Err(format!("no row of import {parent} has the key \"{key}\"")),
            [row] => id_value(field, row.get(0)),
            _ => Err(format!(
                "the key \"{key}\" names more than one row of import {parent}"
            )),
        })
    }

    /// The one record a lookup read finds for the cell's value.
    async fn lookup(
        &mut self,
        client: &tokio_postgres::Client,
        field: &Field,
        text: &str,
    ) -> Result<std::result::Result<Option<Value>, String>> {
        let value = text.trim().to_owned();
        if value.is_empty() {
            return Ok(Ok(None));
        }
        let Some(lookup) = field
            .reference
            .and_then(|reference| reference.lookup.as_ref())
        else {
            return Ok(Err("cannot be looked up".into()));
        };
        let cache = (field.name.clone(), value.clone());
        if let Some(found) = self.found.get(&cache) {
            return Ok(found
                .clone()
                .map(|id| id_value(field, id))
                .and_then(|id| id));
        }
        let mut query = Map::new();
        query.insert(lookup.by.into(), Value::String(value.clone()));
        if self.registered.limited.get(lookup.read) == Some(&true) {
            query.insert("limit".into(), json!(2));
        }
        let found = match self
            .bulk
            .jobs
            .read(client, self.execution, lookup.read, &Value::Object(query))
            .await
        {
            Ok(answer) => match answer["result"].as_array().map(Vec::as_slice) {
                Some([record]) => text_id(&record["id"])
                    .ok_or_else(|| format!("no {} matches \"{value}\"", lookup.noun)),
                Some([]) | None => Err(format!("no {} matches \"{value}\"", lookup.noun)),
                Some(_) => Err(format!("\"{value}\" matches more than one {}", lookup.noun)),
            },
            Err(error) => match refusal(&error, &self.offered) {
                Some(message) => Err(format!("could not look up \"{value}\": {message}")),
                None => return Err(error_of(error)),
            },
        };
        self.found.insert(cache, found.clone());
        Ok(found.and_then(|id| id_value(field, id)))
    }

    fn source_key(&self, cells: &[String]) -> Option<String> {
        let column = self.mapping.source_key.as_deref()?;
        let key: String = self.cell(cells, column).trim().chars().take(255).collect();
        (!key.is_empty()).then_some(key)
    }
}

fn error_of(error: ContractError) -> Error {
    match error {
        ContractError::Backend(error) => error,
        _ => Error::Internal,
    }
}
pub(crate) fn text_id(value: &Value) -> Option<String> {
    match value {
        Value::Number(number) => Some(number.to_string()),
        Value::String(text) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}
fn id_value(field: &Field, id: String) -> std::result::Result<Option<Value>, String> {
    match field.kind {
        crate::Kind::Integer => id
            .parse::<i64>()
            .map(|id| Some(Value::from(id)))
            .map_err(|_| "names a record whose id this field cannot hold".into()),
        _ => Ok(Some(Value::String(id))),
    }
}

/// Locks the run and says whether this pass may still record.
async fn current(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    run: i64,
    pass: Pass,
) -> Result<Option<Run>> {
    let run = store::load(tx.sql(), scope, run, true).await?;
    Ok((run.state.working() && run.pass == Some(pass)).then_some(run))
}

async fn record(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    run: i64,
    pass: Pass,
    results: Vec<Recorded>,
    next: i32,
) -> Result<()> {
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::with_capacity(results.len());
            for result in results {
                let id: i64 = sql.query_one("INSERT INTO base_bulk_data_rows(tenant_id,run_id,pass,row_number,outcome,record_id,source_key,message) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id", &[&tenant, &run, &pass.as_str(), &result.row, &result.outcome.as_str(), &result.record, &result.key, &result.message]).await?.get(0);
                changes.push(Change { kind: "base/bulk_data.row".into(), id, old: Value::Null, new: json!({"run": run, "pass": pass.as_str(), "row": result.row, "outcome": result.outcome.as_str(), "record_id": result.record}) });
            }
            sql.execute("UPDATE base_bulk_data_runs SET next_row=$3,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &run, &next]).await?;
            Ok(((), changes))
        })
    })
    .await
}

impl BulkData {
    pub(crate) async fn work(&self, execution: &Execution, id: i64, pass: Pass) -> Result<()> {
        let scope = execution.scope().ok_or(Error::Forbidden)?.clone();
        if scope.principal().is_some() {
            return Err(Error::Forbidden);
        }
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        let run = store::load(&*client, &scope, id, false).await?;
        if !run.state.working() || run.pass != Some(pass) {
            return Ok(());
        }
        let registered = self
            .types
            .get(run.record_type.as_str())
            .ok_or(Error::NotFound)?;
        let mapping = run.mapping.clone().ok_or(Error::Internal)?;
        let company = OrganisationId::new(run.company).map_err(|_| Error::Internal)?;
        let source = run.source.clone().ok_or(Error::NotFound)?;
        let document = self
            .artifacts
            .for_owner(&scope, company, &FILES)
            .read(client, &source)
            .await
            .map_err(crate::artifact)?;
        let parsed = csv::parse(&document.bytes, usize::MAX).map_err(|_| Error::Internal)?;
        let (offered, _) = self.offered(&*client, &scope, registered).await?;
        let mut work = Work {
            bulk: self,
            execution,
            scope: scope.clone(),
            registered,
            mapping,
            header: parsed.header.clone(),
            offered,
            run: id,
            pass,
            company: run.company,
            found: HashMap::new(),
        };
        let started = Instant::now();
        let mut next = usize::try_from(run.next_row).map_err(|_| Error::Internal)?;
        let mut batch = Vec::new();
        while next < parsed.rows.len() && started.elapsed() < SLICE {
            let cells = &parsed.rows[next];
            let row = next as i32 + 1;
            let planned = work.plan(&*client, cells).await?;
            let key = work.source_key(cells);
            match pass {
                Pass::Preview => {
                    batch.push(work.check(&*client, planned, row, key).await?);
                    next += 1;
                    if batch.len() == BATCH
                        && !self
                            .flush(
                                execution,
                                client,
                                &scope,
                                &work,
                                std::mem::take(&mut batch),
                                next,
                            )
                            .await?
                    {
                        return Ok(());
                    }
                }
                Pass::Run => {
                    if !work.write(client, planned, row, key).await? {
                        return Ok(());
                    }
                    next += 1;
                }
            }
        }
        if !batch.is_empty()
            && !self
                .flush(execution, client, &scope, &work, batch, next)
                .await?
        {
            return Ok(());
        }
        if next < parsed.rows.len() {
            self.later(execution, client, &scope, id, pass).await
        } else {
            self.finish(execution, client, &scope, id, pass).await
        }
    }

    /// Records a batch of dry-run results; false when the pass was cancelled.
    async fn flush(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        work: &Work<'_>,
        results: Vec<Recorded>,
        next: usize,
    ) -> Result<bool> {
        let mut tx = execution
            .begin(client, "base.bulk_data.import.check")
            .await?;
        let Some(run) = current(&tx, scope, work.run, work.pass).await? else {
            return Ok(false);
        };
        let first = results.first().map_or(0, |result| result.row - 1);
        if run.next_row != first {
            // Another attempt recorded these rows already: go on past them.
            return Ok(run.next_row > first);
        }
        record(
            &mut tx,
            scope.tenant(),
            work.run,
            work.pass,
            results,
            next as i32,
        )
        .await?;
        tx.commit().await?;
        self.wake(scope.tenant(), scope.user());
        Ok(true)
    }

    /// Queues the rest of the pass as its own job, from the next row.
    async fn later(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        id: i64,
        pass: Pass,
    ) -> Result<()> {
        let mut tx = execution
            .begin(client, "base.bulk_data.import.continue")
            .await?;
        if current(&tx, scope, id, pass).await?.is_none() {
            return Ok(());
        }
        let job = self
            .queue
            .enqueue_for(
                &mut tx,
                scope,
                WORKER,
                json!({"run": id, "pass": pass.as_str()}),
            )
            .await?;
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute("UPDATE base_bulk_data_runs SET job_id=$3,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &id, &job.id()]).await?;
                Ok(((), vec![Change { kind: "base/bulk_data.run".into(), id, old: Value::Null, new: json!({"continued": true, "pass": pass.as_str()}) }]))
            })
        })
        .await?;
        tx.commit().await
    }

    /// Ends the pass and, for the real run, tells the person.
    async fn finish(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        id: i64,
        pass: Pass,
    ) -> Result<()> {
        let mut tx = execution
            .begin(client, "base.bulk_data.import.finish")
            .await?;
        let Some(old) = current(&tx, scope, id, pass).await? else {
            return Ok(());
        };
        let state = match pass {
            Pass::Preview => "previewed",
            Pass::Run => "completed",
        };
        let tenant = scope.tenant();
        let run = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_runs SET state=$3,finished_at=clock_timestamp(),version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {}", store::COLUMNS), &[&tenant, &id, &state]).await?;
                    let run = Run::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.run".into(), id, old: crate::evidence(&old), new: crate::evidence(&run) }]))
                })
            })
            .await?;
        if pass == Pass::Run {
            let mut counted = vec![run];
            store::count(tx.sql(), tenant, &mut counted).await?;
            let run = &counted[0];
            let counts = run.run;
            let notice = Notice::new(format!("Import of {} finished", run.file_name))
                .kind("bulk_data.import")
                .body(format!(
                    "{} created, {} updated, {} skipped, {} not imported.",
                    counts.create, counts.update, counts.skip, counts.error
                ))
                .url(format!("/imports/{id}"));
            self.notifications
                .notify(&mut tx, scope, &[scope.user()], &notice)
                .await?;
        }
        tx.commit().await?;
        self.wake(tenant, scope.user());
        Ok(())
    }
}

impl Work<'_> {
    /// A dry-run row: the owner's preview of the write, nothing written.
    async fn check(
        &self,
        client: &tokio_postgres::Client,
        planned: Planned,
        row: i32,
        key: Option<String>,
    ) -> Result<Recorded> {
        let (outcome, record, message) = match planned {
            Planned::Refused(message) => (Outcome::Error, None, Some(message)),
            Planned::Skip(id) => (
                Outcome::Skip,
                Some(id),
                Some("Matches an existing record, which is left alone.".into()),
            ),
            Planned::Write {
                contract,
                input,
                outcome,
            } => {
                match self
                    .bulk
                    .jobs
                    .preview(client, self.execution, contract, &input)
                    .await
                {
                    Ok(_) => (outcome, None, None),
                    Err(error) => match refusal(&error, &self.offered) {
                        Some(message) => (Outcome::Error, None, Some(message)),
                        None => return Err(error_of(error)),
                    },
                }
            }
        };
        Ok(Recorded {
            row,
            outcome,
            record,
            key,
            message: message.map(bounded),
        })
    }

    /// A real row: the owner's write and its result in one transaction.
    /// False when the run was cancelled or ended meanwhile.
    async fn write(
        &self,
        client: &mut tokio_postgres::Client,
        planned: Planned,
        row: i32,
        key: Option<String>,
    ) -> Result<bool> {
        let tenant = self.scope.tenant();
        let action = match &planned {
            Planned::Write { contract, .. } => *contract,
            _ => "base.bulk_data.import.row",
        };
        let mut tx = self.execution.begin(client, action).await?;
        let Some(run) = current(&tx, &self.scope, self.run, self.pass).await? else {
            return Ok(false);
        };
        if run.next_row >= row {
            return Ok(true);
        }
        let permitted = match bilibi_authz::authorize_key(
            tx.sql(),
            &self.scope,
            self.registered.definition.import.capability,
        )
        .await
        {
            Ok(()) => true,
            Err(Error::Forbidden) => false,
            Err(error) => return Err(error),
        };
        let result = if !permitted {
            Recorded {
                row,
                outcome: Outcome::Error,
                record: None,
                key,
                message: Some(format!(
                    "You no longer have permission to import {}.",
                    self.registered.definition.label.to_lowercase()
                )),
            }
        } else {
            match planned {
                Planned::Refused(message) => Recorded {
                    row,
                    outcome: Outcome::Error,
                    record: None,
                    key,
                    message: Some(bounded(message)),
                },
                Planned::Skip(id) => Recorded {
                    row,
                    outcome: Outcome::Skip,
                    record: Some(id),
                    key,
                    message: Some("Matched an existing record, which was left alone.".into()),
                },
                Planned::Write {
                    contract,
                    input,
                    outcome,
                } => {
                    match self
                        .bulk
                        .jobs
                        .commit(&mut tx, self.execution, contract, &input)
                        .await
                    {
                        Ok(answer) => Recorded {
                            row,
                            outcome,
                            record: text_id(
                                &answer["result"][self.registered.definition.import.id],
                            ),
                            key,
                            message: None,
                        },
                        Err(error) => {
                            let Some(message) = refusal(&error, &self.offered) else {
                                return Err(error_of(error));
                            };
                            // The owner's refusal may have poisoned the
                            // transaction: record the row in a fresh one.
                            drop(tx);
                            tx = self
                                .execution
                                .begin(client, "base.bulk_data.import.row")
                                .await?;
                            let Some(run) = current(&tx, &self.scope, self.run, self.pass).await?
                            else {
                                return Ok(false);
                            };
                            if run.next_row >= row {
                                return Ok(true);
                            }
                            Recorded {
                                row,
                                outcome: Outcome::Error,
                                record: None,
                                key,
                                message: Some(bounded(message)),
                            }
                        }
                    }
                }
            }
        };
        record(&mut tx, tenant, self.run, self.pass, vec![result], row).await?;
        tx.commit().await?;
        self.bulk.wake(tenant, self.scope.user());
        Ok(true)
    }
}
