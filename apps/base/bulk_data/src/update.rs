//! Bulk updates (issue 133): one field set on many records of one record
//! type, the records ticked on a list or every record its filters match, each
//! through the type's own update contract as the person who started it. No
//! Bilimbi or Belimbing counterpart.
//!
//! A page (or an agent) starts an update with the field, its value and the
//! selection. A queued job, as that person (ADR 10), first resolves the
//! records once, with the version each has then; the filter's values are
//! dropped once resolved. It then writes each record in its own short
//! transaction (`Execution::begin`) with the record's result beside it: it
//! reads the record through the owner's read, keeps the fields it does not
//! set, and calls the owner's update with the resolved version, so a record
//! changed since is refused by the owner's version fence and reported, never
//! overwritten. The type's bulk-update capability is checked again for every
//! record. A pass works in bounded slices and queues its own continuation
//! from the next unrecorded record, so a retried or resumed attempt never
//! writes a record twice. Cancelling keeps what was written.
use crate::export::{maybe_stamp, stamp, Failed};
use crate::import::{bounded, error_of, refusal, text_id};
use crate::store::instant;
use crate::{
    mapping::convert, record::Registered, refused, BulkData, Counts, Field, Outcome, WriteError,
    UPDATE_MAX_RECORDS, VIEW,
};
use bilibi_agent_api::Error as ContractError;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_notifications::Notice;
use bilibi_queue::{Execution, Handler, HandlerFuture, Outcome as JobOutcome};
use bilibi_tenancy::Scope;
use jiff::Timestamp;
use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_postgres::{GenericClient, Row};

pub(crate) const SERVICE: &str = "base/bulk_data.update-job";
pub(crate) const WORKER: &str = "base/bulk_data-update";
/// How long one attempt works before it queues its continuation.
const SLICE: Duration = Duration::from_secs(120);
/// Records written between wakes of the person's open pages.
const WAKE_EVERY: usize = 25;
const COLUMNS: &str = "id,record_type,company_id,field,value,chosen,filter,ids,except_ids,jsonb_array_length(targets) AS records,state,next_row,error,job_id,trace_id,version,floor(extract(epoch FROM created_at))::bigint AS created,floor(extract(epoch FROM finished_at))::bigint AS finished";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    /// The job is finding the records, once.
    Resolving,
    /// The job is updating the records.
    Running,
    Completed,
    /// Stopped by its person; records already updated stay.
    Cancelled,
    /// It ended before updating anything; `error` says why.
    Failed,
}
impl UpdateState {
    fn parse(text: &str) -> Result<Self> {
        Ok(match text {
            "resolving" => Self::Resolving,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "failed" => Self::Failed,
            _ => return Err(Error::Internal),
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Resolving => "resolving",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }
    /// Its job still has work to do.
    pub fn working(self) -> bool {
        matches!(self, Self::Resolving | Self::Running)
    }
}

/// One bulk update, as its person reads it.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateRun {
    pub id: i64,
    pub record_type: String,
    #[serde(skip)]
    pub company: i64,
    /// The field set, and the value it is set to.
    pub field: String,
    pub value: Value,
    /// How the records were chosen: `selected` ticked records, or every
    /// record the list's filters matched.
    pub selection: &'static str,
    /// The records found when the job started; `None` until then.
    pub records: Option<i32>,
    #[serde(skip)]
    filter: Option<Value>,
    #[serde(skip)]
    ids: Option<Vec<String>>,
    /// With the filters, records left out until the update has resolved.
    #[serde(skip)]
    except: Vec<String>,
    pub state: UpdateState,
    /// Records done so far.
    pub progress: i32,
    /// Results so far: updated, left as they were (already set), or not
    /// updated (`create` is always 0).
    pub counts: Counts,
    pub error: Option<String>,
    #[serde(skip)]
    pub job_id: Option<i64>,
    /// Its job ended without finishing (its person lost access, or every
    /// attempt failed): cancel it and start again.
    pub stopped: bool,
    pub trace_id: Option<String>,
    pub version: i64,
    #[serde(serialize_with = "stamp")]
    pub created_at: Timestamp,
    #[serde(serialize_with = "maybe_stamp")]
    pub finished_at: Option<Timestamp>,
}
impl UpdateRun {
    fn from_row(row: &Row) -> Result<Self> {
        let ids: Option<Value> = row.get("ids");
        let filter: Option<Value> = row.get("filter");
        Ok(Self {
            id: row.get("id"),
            record_type: row.get("record_type"),
            company: row.get("company_id"),
            field: row.get("field"),
            value: row.get("value"),
            selection: if row.get::<_, &str>("chosen") == "selected" {
                "selected"
            } else {
                "filter"
            },
            records: row.get("records"),
            filter,
            ids: ids
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| Error::Internal)?,
            except: row
                .get::<_, Option<Value>>("except_ids")
                .map(serde_json::from_value)
                .transpose()
                .map_err(|_| Error::Internal)?
                .unwrap_or_default(),
            state: UpdateState::parse(row.get("state"))?,
            progress: row.get("next_row"),
            counts: Counts::default(),
            error: row.get("error"),
            job_id: row.get("job_id"),
            stopped: false,
            trace_id: row.get("trace_id"),
            version: row.get("version"),
            created_at: instant(row.get("created")),
            finished_at: row.get::<_, Option<i64>>("finished").map(instant),
        })
    }
    /// Content-free audit evidence: never the filter's values.
    fn evidence(&self) -> Value {
        json!({"record_type": self.record_type, "field": self.field, "records": self.records, "done": self.progress, "state": self.state.as_str(), "version": self.version})
    }
}

/// What a person may do in bulk with a record type's list
/// ([`BulkData::offers`]).
#[derive(Clone, Debug, Default)]
pub struct Offers {
    /// They may export it.
    pub export: bool,
    /// The fields they may set in bulk; empty when they may set none.
    pub fields: Vec<Field>,
}

/// One record's result.
#[derive(Clone, Debug, Serialize)]
pub struct UpdateRow {
    pub row: i32,
    pub record_id: String,
    pub outcome: Outcome,
    pub message: Option<String>,
}

/// The person's own update.
pub(crate) async fn load(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
    lock: bool,
) -> Result<UpdateRun> {
    let sql = format!(
        "SELECT {COLUMNS} FROM base_bulk_data_updates WHERE tenant_id=$1 AND id=$2 AND started_by=$3{}",
        if lock { " FOR UPDATE" } else { "" }
    );
    client
        .query_opt(&sql, &[&scope.tenant(), &id, &scope.user()])
        .await?
        .map(|row| UpdateRun::from_row(&row))
        .transpose()?
        .ok_or(Error::NotFound)
}

/// The records an update resolved, each with its version then; read once
/// per attempt, not with every load.
async fn targets(
    client: &impl GenericClient,
    scope: &Scope,
    id: i64,
) -> Result<Vec<(String, Option<i64>)>> {
    let row = client
        .query_one(
            "SELECT targets FROM base_bulk_data_updates WHERE tenant_id=$1 AND id=$2 AND started_by=$3",
            &[&scope.tenant(), &id, &scope.user()],
        )
        .await?;
    match row.get::<_, Option<Value>>(0) {
        Some(targets) => serde_json::from_value(targets).map_err(|_| Error::Internal),
        None => Ok(Vec::new()),
    }
}

/// Each update's results so far, counted in one query for them all.
async fn count(client: &impl GenericClient, tenant: i64, runs: &mut [UpdateRun]) -> Result<()> {
    let ids: Vec<i64> = runs.iter().map(|run| run.id).collect();
    for row in client
        .query(
            "SELECT update_id, outcome, count(*) FROM base_bulk_data_update_rows WHERE tenant_id=$1 AND update_id=ANY($2) GROUP BY update_id, outcome",
            &[&tenant, &ids],
        )
        .await?
    {
        let (id, n): (i64, i64) = (row.get(0), row.get(2));
        let Some(run) = runs.iter_mut().find(|run| run.id == id) else {
            continue;
        };
        match Outcome::parse(row.get(1)) {
            Some(Outcome::Update) => run.counts.update = n,
            Some(Outcome::Skip) => run.counts.skip = n,
            Some(Outcome::Error) => run.counts.error = n,
            _ => {}
        }
    }
    Ok(())
}

/// Marks updates whose job Queue ended while they still had work as
/// `stopped`, with one look at Queue for them all.
async fn mark_stopped(
    client: &impl GenericClient,
    tenant: i64,
    runs: &mut [UpdateRun],
) -> Result<()> {
    let jobs: Vec<i64> = runs
        .iter()
        .filter(|run| run.state.working())
        .filter_map(|run| run.job_id)
        .collect();
    if jobs.is_empty() {
        return Ok(());
    }
    let ended: std::collections::HashSet<i64> = bilibi_queue::states(client, tenant, &jobs)
        .await?
        .into_iter()
        .filter(|status| status.state.terminal())
        .map(|status| status.id)
        .collect();
    for run in runs.iter_mut() {
        run.stopped = run.state.working() && run.job_id.is_some_and(|job| ended.contains(&job));
    }
    Ok(())
}

/// Locks the update and answers how many records are done, while it is
/// still running.
async fn running(tx: &MutationTransaction<'_>, scope: &Scope, id: i64) -> Result<Option<i32>> {
    let row = tx
        .sql()
        .query_opt(
            "SELECT state, next_row FROM base_bulk_data_updates WHERE tenant_id=$1 AND id=$2 AND started_by=$3 FOR UPDATE",
            &[&scope.tenant(), &id, &scope.user()],
        )
        .await?
        .ok_or(Error::NotFound)?;
    Ok((row.get::<_, &str>(0) == "running").then(|| row.get(1)))
}

/// What the bulk update was asked to take.
pub(crate) enum Chosen<'a> {
    /// Ticked records, by id.
    Ids(&'a [String]),
    /// The list contract's filters, without `page` or `page_size`, less the
    /// records unticked after "Select all N matching".
    Filter {
        filter: &'a Value,
        except: &'a [String],
    },
}

impl BulkData {
    fn updatable(
        &self,
        key: &str,
    ) -> std::result::Result<(&Registered, &'static crate::BulkUpdate), WriteError> {
        let registered = self.registered(key)?;
        let bulk = registered
            .definition
            .bulk_update
            .as_ref()
            .ok_or_else(|| refused("record_type", "cannot be updated in bulk"))?;
        Ok((registered, bulk))
    }

    /// Whether the person may update this record type in bulk: its
    /// bulk-update capability and the Imports pages, where updates are
    /// followed. Pages ask before offering it.
    pub async fn may_update(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        key: &str,
    ) -> Result<bool> {
        let held = bilibi_authz::effective_capabilities(client, scope).await?;
        Ok(self
            .updatable(key)
            .is_ok_and(|(_, bulk)| held.contains(VIEW) && held.contains(bulk.capability)))
    }

    /// The fields this person may set in bulk on a record type: those the
    /// owner offers, less any restricted for them. Empty when they may not.
    pub async fn update_fields(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        key: &str,
    ) -> Result<Vec<Field>> {
        let Ok((registered, _)) = self.updatable(key) else {
            return Ok(Vec::new());
        };
        let restricted =
            bilibi_authz::restrictions_for(client, scope, registered.definition.table).await?;
        Ok(self.offers(client, scope, key, &restricted).await?.fields)
    }

    /// What a person may do in bulk with a record type's list: export it,
    /// and set which fields, from one look at their capabilities. A list
    /// page passes the table's restricted fields it already read.
    pub async fn offers(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        key: &str,
        restricted: &std::collections::HashSet<String>,
    ) -> Result<Offers> {
        let held = bilibi_authz::effective_capabilities(client, scope).await?;
        let Ok(registered) = self.registered(key) else {
            return Ok(Offers::default());
        };
        let definition = registered.definition;
        let pages = held.contains(VIEW);
        let export = pages
            && definition
                .export
                .as_ref()
                .is_some_and(|export| held.contains(export.capability));
        let fields = match &definition.bulk_update {
            Some(bulk) if pages && held.contains(bulk.capability) => registered
                .bulk_fields
                .iter()
                .filter(|field| !restricted.contains(&field.name))
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        Ok(Offers { export, fields })
    }

    /// Checks an update request before it starts, and answers the value as
    /// the update contract takes it.
    pub(crate) async fn ready_update(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        record_type: &str,
        field: &str,
        value: &str,
        chosen: &Chosen<'_>,
    ) -> std::result::Result<Value, WriteError> {
        let (registered, bulk) = self.updatable(record_type)?;
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        bilibi_authz::authorize_key(client, scope, bulk.capability).await?;
        crate::work_company(scope, "update records in bulk")?;
        let restricted =
            bilibi_authz::restrictions_for(client, scope, registered.definition.table).await?;
        let Some(field) = registered
            .bulk_fields
            .iter()
            .find(|offered| offered.name == field && !restricted.contains(&offered.name))
        else {
            return Err(refused(
                "field",
                "is not a field you may set on these records",
            ));
        };
        let value = match convert(field, value) {
            Ok(Some(value)) => value,
            Ok(None) => return Err(refused("value", "give the value to set")),
            Err(message) => return Err(refused("value", &message)),
        };
        let max = self
            .settings
            .get_global(client, UPDATE_MAX_RECORDS)
            .await?
            .as_i64()
            .ok_or(Error::Internal)?;
        match chosen {
            Chosen::Ids(ids) => {
                if ids.is_empty() {
                    return Err(refused("ids", "tick at least one record"));
                }
                crate::distinct_ids("ids", ids, max)?;
            }
            Chosen::Filter { filter, except } => {
                // Leaving records out adds no work, so it is bounded only by
                // what a list's selection may hold, not by the record limit.
                crate::distinct_ids("except_ids", except, bilibi_ui::Selection::MAX_KEYS as i64)?;
                self.ready_filter(bulk.list, filter)?;
            }
        }
        Ok(value)
    }

    /// Opens an update and queues its job, on the caller's transaction.
    pub(crate) async fn start_update(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        record_type: &str,
        field: &str,
        value: &str,
        chosen: &Chosen<'_>,
    ) -> std::result::Result<UpdateRun, WriteError> {
        let value = self
            .ready_update(tx.sql(), scope, record_type, field, value, chosen)
            .await?;
        let (registered, _) = self.updatable(record_type)?;
        let company = crate::work_company(scope, "update records in bulk")?.get();
        let (tenant, user, kind, field) = (
            scope.tenant(),
            scope.user(),
            registered.key,
            field.to_owned(),
        );
        let (chosen, filter, ids, except) = match chosen {
            Chosen::Ids(ids) => ("selected", None, Some(json!(ids)), None),
            Chosen::Filter { filter, except } => (
                "filter",
                Some((*filter).clone()),
                None,
                (!except.is_empty()).then(|| json!(except)),
            ),
        };
        let trace = tx.context().trace_id.clone();
        let opened = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("INSERT INTO base_bulk_data_updates(tenant_id,company_id,record_type,started_by,field,value,chosen,filter,ids,except_ids,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11) RETURNING {COLUMNS}"), &[&tenant, &company, &kind, &user, &field, &value, &chosen, &filter, &ids, &except, &trace]).await?;
                    let run = UpdateRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.update".into(), id: run.id, old: Value::Null, new: run.evidence() }]))
                })
            })
            .await?;
        let job = self
            .queue
            .enqueue_for(tx, scope, WORKER, json!({"update": opened.id}))
            .await?;
        let id = opened.id;
        let old = opened.evidence();
        let run = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_updates SET job_id=$3,version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id, &job.id()]).await?;
                    let run = UpdateRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.update".into(), id, old, new: run.evidence() }]))
                })
            })
            .await?;
        Ok(run)
    }

    /// Checks that the person's update may be stopped.
    pub(crate) async fn cancellable_update(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
        version: i64,
        lock: bool,
    ) -> std::result::Result<UpdateRun, WriteError> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        let run = load(client, scope, id, lock).await?;
        let (_, bulk) = self.updatable(&run.record_type)?;
        bilibi_authz::authorize_key(client, scope, bulk.capability).await?;
        if run.version != version {
            return Err(WriteError::Stale);
        }
        if !run.state.working() {
            return Err(refused("update_id", "has already ended"));
        }
        Ok(run)
    }

    /// Stops the person's update; records already updated stay.
    pub(crate) async fn cancel_update(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> std::result::Result<UpdateRun, WriteError> {
        let old = self
            .cancellable_update(tx.sql(), scope, id, version, true)
            .await?;
        let tenant = scope.tenant();
        let run = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_updates SET state='cancelled',filter=NULL,ids=NULL,except_ids=NULL,finished_at=clock_timestamp(),version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id]).await?;
                    let run = UpdateRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.update".into(), id, old: old.evidence(), new: run.evidence() }]))
                })
            })
            .await?;
        Ok(run)
    }

    /// The person's own update, with its counts.
    pub async fn update(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
    ) -> Result<UpdateRun> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        let mut runs = [load(client, scope, id, false).await?];
        count(client, scope.tenant(), &mut runs).await?;
        mark_stopped(client, scope.tenant(), &mut runs).await?;
        let [run] = runs;
        Ok(run)
    }

    /// The person's newest updates, with their counts, marked as their
    /// pages show them.
    pub async fn updates(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        limit: i64,
    ) -> Result<Vec<UpdateRun>> {
        bilibi_authz::authorize_key(client, scope, VIEW).await?;
        let mut runs = client
            .query(
                &format!("SELECT {COLUMNS} FROM base_bulk_data_updates WHERE tenant_id=$1 AND started_by=$2 ORDER BY id DESC LIMIT $3"),
                &[&scope.tenant(), &scope.user(), &limit.clamp(1, 100)],
            )
            .await?
            .iter()
            .map(UpdateRun::from_row)
            .collect::<Result<Vec<_>>>()?;
        count(client, scope.tenant(), &mut runs).await?;
        mark_stopped(client, scope.tenant(), &mut runs).await?;
        Ok(runs)
    }

    /// One page of an update's results in record order, and how many there
    /// are, optionally only one outcome.
    pub async fn update_rows(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
        outcome: Option<Outcome>,
        offset: i64,
        limit: i64,
    ) -> Result<(Vec<UpdateRow>, i64)> {
        let run = self.update(client, scope, id).await?;
        let outcome = outcome.map(Outcome::as_str);
        let total: i64 = client
            .query_one(
                "SELECT count(*) FROM base_bulk_data_update_rows WHERE tenant_id=$1 AND update_id=$2 AND ($3::text IS NULL OR outcome=$3)",
                &[&scope.tenant(), &run.id, &outcome],
            )
            .await?
            .get(0);
        let rows = client
            .query(
                "SELECT row_number,record_id,outcome,message FROM base_bulk_data_update_rows WHERE tenant_id=$1 AND update_id=$2 AND ($3::text IS NULL OR outcome=$3) ORDER BY row_number OFFSET $4 LIMIT $5",
                &[&scope.tenant(), &run.id, &outcome, &offset.max(0), &limit.clamp(1, 300)],
            )
            .await?
            .iter()
            .map(|row| {
                Ok(UpdateRow {
                    row: row.get(0),
                    record_id: row.get(1),
                    outcome: Outcome::parse(row.get(2)).ok_or(Error::Internal)?,
                    message: row.get(3),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        Ok((rows, total))
    }

    /// The job's work: resolve the records once, then update them in
    /// bounded slices.
    pub(crate) async fn work_update(&self, execution: &Execution, id: i64) -> Result<()> {
        let scope = execution.scope().ok_or(Error::Forbidden)?.clone();
        if scope.principal().is_some() {
            return Err(Error::Forbidden);
        }
        let mut object = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut object;
        let mut run = load(&*client, &scope, id, false).await?;
        let Ok((registered, bulk)) = self.updatable(&run.record_type) else {
            return self
                .end_update(
                    execution,
                    client,
                    &scope,
                    id,
                    Err(Failed(
                        "This kind of record can no longer be updated in bulk.".into(),
                    )),
                )
                .await;
        };
        if run.state == UpdateState::Resolving {
            match self
                .resolve(execution, client, &scope, registered, bulk, &run)
                .await?
            {
                Ok(targets) => {
                    let tenant = scope.tenant();
                    let mut tx = execution
                        .begin(client, "base.bulk_data.update.resolve")
                        .await?;
                    let old = load(tx.sql(), &scope, id, true).await?;
                    if old.state != UpdateState::Resolving {
                        return Ok(());
                    }
                    let targets = json!(targets);
                    let old_evidence = old.evidence();
                    tx.mutate(move |sql| {
                        Box::pin(async move {
                            let row = sql.query_one(&format!("UPDATE base_bulk_data_updates SET targets=$3,filter=NULL,ids=NULL,except_ids=NULL,state='running',version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id, &targets]).await?;
                            let run = UpdateRun::from_row(&row)?;
                            Ok(((), vec![Change { kind: "base/bulk_data.update".into(), id, old: old_evidence, new: run.evidence() }]))
                        })
                    })
                    .await?;
                    tx.commit().await?;
                    self.wake(tenant, scope.user());
                    run = load(&*client, &scope, id, false).await?;
                }
                Err(failed) => {
                    return self
                        .end_update(execution, client, &scope, id, Err(failed))
                        .await
                }
            }
        }
        if run.state != UpdateState::Running {
            return Ok(());
        }
        let fields: Vec<Field> = registered.bulk_fields.clone();
        let targets = targets(&*client, &scope, id).await?;
        let started = Instant::now();
        let mut next = usize::try_from(run.progress).map_err(|_| Error::Internal)?;
        while next < targets.len() && started.elapsed() < SLICE {
            let (record, version) = targets[next].clone();
            let row = next as i32 + 1;
            if !self
                .write_one(
                    execution, client, &scope, registered, bulk, &fields, &run, row, &record,
                    version,
                )
                .await?
            {
                return Ok(());
            }
            next += 1;
            // Open pages follow along every so many records, not every one.
            if next % WAKE_EVERY == 0 {
                self.wake(scope.tenant(), scope.user());
            }
        }
        self.wake(scope.tenant(), scope.user());
        if next < targets.len() {
            self.continue_update(execution, client, &scope, id).await
        } else {
            self.end_update(execution, client, &scope, id, Ok(())).await
        }
    }

    /// The records the update takes, each with its version now, or why it
    /// cannot start, in words.
    async fn resolve(
        &self,
        execution: &Execution,
        client: &tokio_postgres::Client,
        scope: &Scope,
        registered: &Registered,
        bulk: &crate::BulkUpdate,
        run: &UpdateRun,
    ) -> Result<std::result::Result<Vec<(String, Option<i64>)>, Failed>> {
        for capability in [VIEW, bulk.capability] {
            match bilibi_authz::authorize_key(client, scope, capability).await {
                Ok(()) => {}
                Err(Error::Forbidden | Error::Unauthorized) => {
                    return Ok(Err(Failed(
                        "You no longer have permission to update these records.".into(),
                    )))
                }
                Err(error) => return Err(error),
            }
        }
        let max = self
            .settings
            .get_global(client, UPDATE_MAX_RECORDS)
            .await?
            .as_i64()
            .ok_or(Error::Internal)?;
        let mut targets = Vec::new();
        if let Some(ids) = &run.ids {
            for id in ids {
                let mut input = Map::new();
                input.insert(bulk.get_id.into(), crate::typed(id));
                let version = match self
                    .jobs
                    .read(client, execution, bulk.get, &Value::Object(input))
                    .await
                {
                    Ok(answer) => answer["result"]["version"].as_i64(),
                    Err(error) => {
                        if refusal(&error, &[]).is_none() {
                            return Err(error_of(error));
                        }
                        // Not found, or not theirs: reported on its row.
                        None
                    }
                };
                targets.push((id.clone(), version));
            }
        } else {
            // Read until two full reads agree, so a record the list moved
            // while it was paged is never left out of the update.
            let filter = run.filter.clone().ok_or(Error::Internal)?;
            let listed = match self
                .read_list(
                    execution,
                    client,
                    registered,
                    bulk.list,
                    &filter,
                    &run.except,
                    max,
                    Instant::now(),
                )
                .await?
            {
                Ok(listed) => listed,
                Err(failed) => return Ok(Err(failed)),
            };
            for (id, version) in listed {
                targets.push((text_id(&id).ok_or(Error::Internal)?, version));
            }
        }
        if targets.is_empty() {
            return Ok(Err(Failed(
                "No records matched, so nothing was changed.".into(),
            )));
        }
        Ok(Ok(targets))
    }

    /// One record: its current values read through the owner, then the
    /// owner's update with the resolved version and the result beside it,
    /// in one transaction. False when the update was cancelled or ended.
    #[allow(clippy::too_many_arguments)]
    async fn write_one(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        registered: &Registered,
        bulk: &crate::BulkUpdate,
        fields: &[Field],
        run: &UpdateRun,
        row: i32,
        record: &str,
        version: Option<i64>,
    ) -> Result<bool> {
        // What the row will do, read before the write's transaction.
        let planned: std::result::Result<Option<Value>, String> = match version {
            None => Err("Not found, or not a record you may change.".into()),
            Some(version) => {
                let mut input = Map::new();
                input.insert(bulk.get_id.into(), crate::typed(record));
                match self
                    .jobs
                    .read(&*client, execution, bulk.get, &Value::Object(input))
                    .await
                {
                    Ok(answer) => {
                        let current = &answer["result"];
                        if current.get(&run.field) == Some(&run.value) {
                            Ok(None)
                        } else {
                            let mut values = Map::new();
                            for name in &registered.bulk_update_fields {
                                if let Some(value) = current.get(name).filter(|value| {
                                    !value.is_null() && value.get("restricted").is_none()
                                }) {
                                    values.insert(name.clone(), value.clone());
                                }
                            }
                            values.insert(run.field.clone(), run.value.clone());
                            let mut input = Map::new();
                            input.insert(bulk.update_id.into(), crate::typed(record));
                            input.insert(bulk.update_version.into(), json!(version));
                            if bulk.update_fields.is_empty() {
                                input.extend(values);
                            } else {
                                input.insert(bulk.update_fields.into(), Value::Object(values));
                            }
                            Ok(Some(Value::Object(input)))
                        }
                    }
                    Err(error) => match refusal(&error, fields) {
                        Some(message) => Err(message),
                        None => return Err(error_of(error)),
                    },
                }
            }
        };
        let tenant = scope.tenant();
        let mut tx = execution.begin(client, bulk.update).await?;
        let Some(progress) = running(&tx, scope, run.id).await? else {
            return Ok(false);
        };
        if progress >= row {
            // Another attempt recorded this record already.
            return Ok(true);
        }
        let permitted = match bilibi_authz::authorize_key(tx.sql(), scope, bulk.capability).await {
            Ok(()) => true,
            Err(Error::Forbidden) => false,
            Err(error) => return Err(error),
        };
        let (outcome, message) = if !permitted {
            (
                Outcome::Error,
                Some(format!(
                    "You no longer have permission to update {} in bulk.",
                    registered.definition.label.to_lowercase()
                )),
            )
        } else {
            match planned {
                Err(message) => (Outcome::Error, Some(message)),
                Ok(None) => (Outcome::Skip, Some("Already set to this value.".into())),
                Ok(Some(input)) => match self
                    .jobs
                    .commit(&mut tx, execution, bulk.update, &input)
                    .await
                {
                    Ok(_) => (Outcome::Update, None),
                    Err(error) => {
                        let message = match &error {
                            ContractError::Backend(Error::Conflict) => {
                                "The record changed after the update started, so it was not overwritten.".to_owned()
                            }
                            _ => match refusal(&error, fields) {
                                Some(message) => message,
                                None => return Err(error_of(error)),
                            },
                        };
                        // The owner's refusal may have poisoned the
                        // transaction: record the result in a fresh one.
                        drop(tx);
                        tx = execution.begin(client, "base.bulk_data.update.row").await?;
                        let Some(progress) = running(&tx, scope, run.id).await? else {
                            return Ok(false);
                        };
                        if progress >= row {
                            return Ok(true);
                        }
                        (Outcome::Error, Some(message))
                    }
                },
            }
        };
        let (update, record) = (run.id, record.to_owned());
        let message = message.map(bounded);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = sql.query_one("INSERT INTO base_bulk_data_update_rows(tenant_id,update_id,row_number,record_id,outcome,message) VALUES($1,$2,$3,$4,$5,$6) RETURNING id", &[&tenant, &update, &row, &record, &outcome.as_str(), &message]).await?.get(0);
                sql.execute("UPDATE base_bulk_data_updates SET next_row=$3,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2", &[&tenant, &update, &row]).await?;
                Ok(((), vec![Change { kind: "base/bulk_data.update_row".into(), id, old: Value::Null, new: json!({"update": update, "row": row, "record_id": record, "outcome": outcome.as_str()}) }]))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(true)
    }

    /// Queues the rest as its own job, from the next record.
    async fn continue_update(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        id: i64,
    ) -> Result<()> {
        let mut tx = execution
            .begin(client, "base.bulk_data.update.continue")
            .await?;
        let old = load(tx.sql(), scope, id, true).await?;
        if old.state != UpdateState::Running {
            return Ok(());
        }
        let job = self
            .queue
            .enqueue_for(&mut tx, scope, WORKER, json!({"update": id}))
            .await?;
        let tenant = scope.tenant();
        let old = old.evidence();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(&format!("UPDATE base_bulk_data_updates SET job_id=$3,version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id, &job.id()]).await?;
                let run = UpdateRun::from_row(&row)?;
                Ok(((), vec![Change { kind: "base/bulk_data.update".into(), id, old, new: run.evidence() }]))
            })
        })
        .await?;
        tx.commit().await
    }

    /// Records how the update ended and tells its person.
    async fn end_update(
        &self,
        execution: &Execution,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        id: i64,
        result: std::result::Result<(), Failed>,
    ) -> Result<()> {
        let mut tx = execution
            .begin(client, "base.bulk_data.update.finish")
            .await?;
        let old = load(tx.sql(), scope, id, true).await?;
        if !old.state.working() {
            return Ok(());
        }
        let tenant = scope.tenant();
        let label = self.label(&old.record_type).to_lowercase();
        let started = old.state == UpdateState::Running;
        let (state, error) = match result {
            Ok(()) => ("completed", None),
            Err(Failed(why)) => ("failed", Some(why)),
        };
        let old_evidence = old.evidence();
        let mut run = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql.query_one(&format!("UPDATE base_bulk_data_updates SET state=$3,error=$4,filter=NULL,ids=NULL,except_ids=NULL,finished_at=clock_timestamp(),version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2 RETURNING {COLUMNS}"), &[&tenant, &id, &state, &error]).await?;
                    let run = UpdateRun::from_row(&row)?;
                    Ok((run.clone(), vec![Change { kind: "base/bulk_data.update".into(), id, old: old_evidence, new: run.evidence() }]))
                })
            })
            .await?;
        count(tx.sql(), tenant, std::slice::from_mut(&mut run)).await?;
        let notice = match run.state {
            UpdateState::Completed => {
                Notice::new(format!("Bulk update of {label} finished")).body(format!(
                    "{} updated, {} already set, {} not updated.",
                    run.counts.update, run.counts.skip, run.counts.error
                ))
            }
            _ if started => Notice::new(format!("Bulk update of {label} stopped"))
                .body(run.error.clone().unwrap_or_default()),
            _ => Notice::new(format!("Bulk update of {label} did not start"))
                .body(run.error.clone().unwrap_or_default()),
        }
        .kind("bulk_data.update")
        .url(format!("/updates/{id}"));
        self.notifications
            .notify(&mut tx, scope, &[scope.user()], &notice)
            .await?;
        tx.commit().await?;
        self.wake(tenant, scope.user());
        Ok(())
    }
}

pub(crate) struct UpdateJob(pub Arc<BulkData>);

impl Handler for UpdateJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args["update"].as_i64() else {
                return JobOutcome::Cancel("worker_args_refused");
            };
            match self.0.work_update(execution, id).await {
                Ok(()) => JobOutcome::Completed,
                Err(Error::Unauthorized | Error::Forbidden) => JobOutcome::Cancel("actor_refused"),
                Err(Error::NotFound) => JobOutcome::Cancel("update_missing"),
                Err(Error::Conflict) => JobOutcome::Retry("lease_lost"),
                Err(_) => JobOutcome::Retry("update_unavailable"),
            }
        })
    }
}

/// Queue's argument check: an update id, nothing else.
pub(crate) fn arguments(args: Value) -> Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    if object.len() == 1
        && object
            .get("update")
            .and_then(Value::as_i64)
            .is_some_and(|id| id > 0)
    {
        Ok(args)
    } else {
        Err(Error::Invalid)
    }
}
