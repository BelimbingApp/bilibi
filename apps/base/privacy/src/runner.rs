//! The request jobs (ADR 05). Each runs as the approver who started it, through
//! `Execution::begin`, which re-proves that person and `privacy.request.approve`
//! on every attempt. One store job per mounted store answers that store with a
//! sealed grant for this request; once every store has answered or deferred,
//! the finish job bundles the export or writes the certificate and completes
//! the request. Every step is idempotent: a retried job finds its answer
//! recorded and moves on. Erasure holds the tenant's hold guard from before
//! its transaction until after it commits, so a hold placed meanwhile waits.
use crate::{
    documents,
    requests::{self, RequestKind, RequestView, EXPORT_KIND, STORE_KIND, SUBJECT},
    Privacy, AUDIT_MINIMUM,
};
use bilibi_audit::Minimums;
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_personal_data::{
    Cause, Deferral, Erased, Erasure, HandlerFuture, Holds, Kept, KeptReason, KindAction, Progress,
};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture as JobFuture, Outcome};
use bilibi_tenancy::Scope;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::Arc;

pub const STORE_WORKER: &str = "base/privacy-request-store";
pub const FINISH_WORKER: &str = "base/privacy-request-finish";
/// The record kind a store's kept audit evidence is listed under.
const EVIDENCE_KIND: &str = "base/audit.evidence";
/// Cleanup a completed erasure still owes because a hold kept records.
pub(crate) const RESUMPTION_KIND: &str = "base/privacy.resumption";
/// One finish job per request at a time (with the request ID).
const FINISH_GUARD: i32 = 841_331;

pub(crate) fn store_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(o)
            if o.len() == 2
                && o.get("request_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0)
                && o.get("store")
                    .and_then(Value::as_str)
                    .is_some_and(|s| !s.is_empty() && s.len() <= 255) =>
        {
            Ok(args)
        }
        _ => Err(Error::Invalid),
    }
}
pub(crate) fn finish_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(o)
            if o.len() == 1
                && o.get("request_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0) =>
        {
            Ok(args)
        }
        _ => Err(Error::Invalid),
    }
}

/// A short, stable code for the store or request row; never an error
/// message, which may quote personal data.
fn code(error: &Error) -> &'static str {
    match error {
        Error::Forbidden | Error::Unauthorized => "forbidden",
        Error::NotFound => "not_found",
        Error::Invalid => "invalid",
        Error::Conflict => "conflict",
        Error::StepUpRequired { .. } => "step_up_required",
        _ => "store_failed",
    }
}

/// A store's answer as the request keeps it: counts, kinds, IDs and reasons,
/// never a personal value. `records` counts the records whose audit evidence
/// was cleared.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub(crate) struct Answer {
    #[serde(default)]
    pub(crate) actions: Vec<KindAction>,
    #[serde(default)]
    pub(crate) kept: Vec<Kept>,
    #[serde(default)]
    pub(crate) deferral: Option<Deferral>,
    #[serde(default)]
    pub(crate) records: u64,
}
impl Answer {
    fn add(&mut self, step: &Erased) {
        for action in &step.actions {
            match self
                .actions
                .iter_mut()
                .find(|a| a.kind == action.kind && a.action == action.action)
            {
                Some(known) => known.count += action.count,
                None => self.actions.push(action.clone()),
            }
        }
        // A retried step reports what it kept again; list each record once.
        for kept in &step.kept {
            if !self
                .kept
                .iter()
                .any(|k| k.kind == kept.kind && k.id == kept.id && k.reason == kept.reason)
            {
                self.kept.push(kept.clone());
            }
        }
        if step.deferral.is_some() {
            self.deferral = step.deferral.clone();
        }
        self.records += step.records.len() as u64;
    }
}

pub(crate) struct StoreJob {
    pub(crate) privacy: Arc<Privacy>,
}
impl Handler for StoreJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> JobFuture<'a> {
        Box::pin(async move {
            let (Some(id), Some(store)) = (
                args.get("request_id").and_then(Value::as_i64),
                args.get("store").and_then(Value::as_str),
            ) else {
                return Outcome::Cancel("invalid_request_reference");
            };
            let Some(scope) = execution.scope() else {
                return Outcome::Cancel("needs_approver");
            };
            // Boxed: a store's erase and an export nest deep futures.
            match Box::pin(answer(&self.privacy, execution, scope, id, store)).await {
                Ok(()) => Outcome::Completed,
                Err(Error::NotFound) if !request_exists(&self.privacy, scope, id).await => {
                    Outcome::Cancel("request_unavailable")
                }
                Err(error) => {
                    // Shown on the request as late; the queue retries.
                    let _ = failed(&self.privacy, execution, scope, id, store, code(&error)).await;
                    Outcome::Retry("store_failed")
                }
            }
        })
    }
}

/// Whether the request a job names is still there. Only its absence cancels
/// a job: a `NotFound` from a store or a file is a failure, recorded and
/// retried, so the request shows as late. A check that cannot run counts as
/// present.
async fn request_exists(privacy: &Privacy, scope: &Scope, id: i64) -> bool {
    let Ok(client) = privacy.db.get().await else {
        return true;
    };
    !matches!(
        client
            .query_opt(
                "SELECT 1 FROM base_privacy_requests WHERE tenant_id=$1 AND id=$2",
                &[&scope.tenant(), &id],
            )
            .await,
        Ok(None)
    )
}

/// Records one failed attempt on the store row, in its own transaction.
async fn failed(
    privacy: &Privacy,
    execution: &Execution,
    scope: &Scope,
    id: i64,
    store: &str,
    code: &'static str,
) -> Result<()> {
    let mut client = privacy.db.get().await?;
    let mut tx = execution
        .begin(&mut client, "privacy.request.store_failed")
        .await?;
    let tenant = scope.tenant();
    let store = store.to_owned();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row: i64 = sql
                .query_opt(
                    "UPDATE base_privacy_request_stores SET attempts=attempts+1,last_error=$4,last_attempt_at=clock_timestamp() \
                     WHERE tenant_id=$1 AND request_id=$2 AND store=$3 AND (state='pending' OR NOT finished) RETURNING id",
                    &[&tenant, &id, &store, &code],
                )
                .await?
                .ok_or(Error::NotFound)?
                .get(0);
            let change = Change {
                kind: STORE_KIND.into(),
                id: row,
                old: json!({}),
                new: json!({"request": id, "store": store, "failed": code}),
            };
            Ok(((), vec![change]))
        })
    })
    .await?;
    tx.commit().await
}

impl Privacy {
    /// Every company's legal minimum for audit evidence in the tenant
    /// (`privacy.audit.legal_minimum_days`): evidence of a company stays while
    /// newer than its own minimum, whatever company filed the request, and
    /// evidence of no company follows the strictest.
    pub(crate) async fn evidence_minimums(
        &self,
        client: &tokio_postgres::Client,
        scope: &Scope,
    ) -> Result<Minimums> {
        let mut entries = Vec::new();
        for (company, value) in self
            .settings
            .company_values(client, scope, AUDIT_MINIMUM)
            .await?
        {
            if value.is_null() {
                continue;
            }
            let days = value
                .as_i64()
                .and_then(|days| i32::try_from(days).ok())
                .ok_or(Error::Invalid)?;
            entries.push((company, days));
        }
        Minimums::new(entries)
    }
}

/// Clears the audit evidence of every record a step erased, in the step's
/// transaction, and lists evidence kept for its legal minimum.
pub(crate) async fn clear_evidence(
    tx: &MutationTransaction<'_>,
    grant: &Erasure,
    step: &mut Erased,
    minimums: &Minimums,
) -> Result<()> {
    for record in &step.records {
        let cleared =
            bilibi_audit::erase_record(tx, grant, &record.kind, record.id, record.person, minimums)
                .await?;
        if cleared.kept > 0 {
            step.kept.push(Kept {
                kind: EVIDENCE_KIND.into(),
                id: format!("{} {}", record.kind, record.id),
                reason: KeptReason::LegalMinimum,
                until: cleared.kept_until,
                hold: None,
            });
        }
    }
    Ok(())
}

async fn answer(
    privacy: &Privacy,
    execution: &Execution,
    scope: &Scope,
    id: i64,
    store: &str,
) -> Result<()> {
    let mut client = privacy.db.get().await?;
    let mut request = requests::load(&**client, scope.tenant(), id, false).await?;
    if request.status != "running" {
        return Ok(());
    }
    request.confirmed = requests::confirmed(&**client, scope.tenant(), id).await?;
    let (key, data) = privacy.stores.get(store).ok_or(Error::Conflict)?;
    let subject = requests::subject_for(&request, key);
    let state = client
        .query_opt(
            "SELECT state,finished FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2 AND store=$3",
            &[&scope.tenant(), &id, &key],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let (state, finished): (String, bool) = (state.get(0), state.get(1));
    match request.kind {
        RequestKind::Access | RequestKind::Portability => {
            if state != "pending" {
                return Ok(());
            }
            // Never read while an erasure of the person runs, and stage only
            // what was read in the generation still current (P1: no erased
            // value may come back through an export).
            let (generation, erasing) =
                requests::erasure_generation(&**client, scope.tenant(), request.person).await?;
            if erasing {
                return Err(Error::Conflict);
            }
            let grant = privacy
                .issuer
                .disclosure(scope.tenant(), id, request.person);
            let exported = data
                .handler
                .export(&client, scope, &subject, &grant)
                .await?;
            let mut tx = execution
                .begin(&mut client, "privacy.request.export")
                .await?;
            requests::person_guard(tx.sql(), scope.tenant(), request.person, false).await?;
            if requests::erasure_generation(tx.sql(), scope.tenant(), request.person).await?
                != (generation, false)
            {
                tx.rollback().await?;
                return Err(Error::Conflict);
            }
            let records = exported.records;
            let files = serde_json::to_value(&exported.files).map_err(|_| Error::Internal)?;
            let summary = json!({"files": exported.files.len()});
            let tenant = scope.tenant();
            let answered = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let Some(row) = sql
                            .query_opt(
                                "UPDATE base_privacy_request_stores SET state='answered',result=$4,answered_at=clock_timestamp(),finished=true \
                                 WHERE tenant_id=$1 AND request_id=$2 AND store=$3 AND state='pending' RETURNING id",
                                &[&tenant, &id, &key, &summary],
                            )
                            .await?
                        else {
                            return Err(Error::Conflict);
                        };
                        let staged: i64 = sql
                            .query_one(
                                "INSERT INTO base_privacy_request_exports(request_id,tenant_id,store,records,files,generation) VALUES($1,$2,$3,$4,$5,$6) RETURNING id",
                                &[&id, &tenant, &key, &records, &files, &generation],
                            )
                            .await?
                            .get(0);
                        // The staged change names the store, never the records.
                        let changes = vec![
                            Change { kind: STORE_KIND.into(), id: row.get(0), old: json!({"state": "pending"}), new: json!({"request": id, "store": key, "state": "answered", "result": summary}) },
                            Change { kind: EXPORT_KIND.into(), id: staged, old: json!({}), new: json!({"request": id, "store": key, "staged": true}) },
                        ];
                        Ok(((), changes))
                    })
                })
                .await;
            match answered {
                Ok(()) => {}
                // Answered by an earlier attempt that committed late.
                Err(Error::Conflict) => return Ok(()),
                Err(error) => return Err(error),
            }
            finish_when_done(privacy, &mut tx, scope, id).await?;
            tx.commit().await
        }
        RequestKind::Erasure => {
            let grant =
                privacy
                    .issuer
                    .erasure(scope.tenant(), Cause::Request(id), Some(request.person));
            let minimums = privacy.evidence_minimums(&client, scope).await?;
            if state == "pending" {
                let holds = Holds::acquire(scope.tenant(), Some(request.person)).await?;
                let mut tx = execution
                    .begin(&mut client, "privacy.request.erase")
                    .await?;
                let locked = tx
                    .sql()
                    .query_opt(
                        "SELECT state FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2 AND store=$3 FOR UPDATE",
                        &[&scope.tenant(), &id, &key],
                    )
                    .await?
                    .ok_or(Error::NotFound)?
                    .get::<_, String>(0);
                if locked == "pending" {
                    let mut erased = data
                        .handler
                        .erase(&mut tx, scope, &subject, &grant, &holds)
                        .await?;
                    clear_evidence(&tx, &grant, &mut erased, &minimums).await?;
                    let mut answer = Answer::default();
                    answer.add(&erased);
                    record_answer(&mut tx, scope, id, key, &answer, false).await?;
                    tx.commit().await?;
                } else {
                    tx.rollback().await?;
                }
                // Released only now: the erasure is committed.
                holds.release().await?;
            } else if finished {
                return Ok(());
            }
            let holds = Holds::acquire(scope.tenant(), Some(request.person)).await?;
            let progress = Steps {
                db: privacy.db.clone(),
                execution,
                scope,
                id,
                key,
                grant: &grant,
                minimums: &minimums,
            };
            data.handler
                .after_erase(&mut client, scope, &subject, &grant, &holds, &progress)
                .await?;
            holds.release().await?;
            let mut tx = execution
                .begin(&mut client, "privacy.request.erase_finished")
                .await?;
            let answer = stored_answer(&tx, scope, id, key).await?;
            record_answer(&mut tx, scope, id, key, &answer, true).await?;
            finish_when_done(privacy, &mut tx, scope, id).await?;
            tx.commit().await
        }
    }
}

async fn stored_answer(
    tx: &MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    key: &str,
) -> Result<Answer> {
    let current: Option<Value> = tx
        .sql()
        .query_one(
            "SELECT result FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2 AND store=$3 FOR UPDATE",
            &[&scope.tenant(), &id, &key],
        )
        .await?
        .get(0);
    match current {
        Some(value) => serde_json::from_value(value).map_err(|_| Error::Internal),
        None => Ok(Answer::default()),
    }
}

/// `after_erase` steps, each saved in its own transaction as soon as the
/// handler reports it, so a retry still counts what earlier attempts did.
struct Steps<'a> {
    db: Db,
    execution: &'a Execution,
    scope: &'a Scope,
    id: i64,
    key: &'static str,
    grant: &'a Erasure,
    minimums: &'a Minimums,
}
impl Progress for Steps<'_> {
    fn record<'a>(&'a self, mut step: Erased) -> HandlerFuture<'a, ()> {
        Box::pin(async move {
            let mut client = self.db.get().await?;
            let mut tx = self
                .execution
                .begin(&mut client, "privacy.request.erase_step")
                .await?;
            clear_evidence(&tx, self.grant, &mut step, self.minimums).await?;
            let mut answer = stored_answer(&tx, self.scope, self.id, self.key).await?;
            answer.add(&step);
            record_answer(&mut tx, self.scope, self.id, self.key, &answer, false).await?;
            tx.commit().await
        })
    }
}

/// Saves a store's erasure answer.
async fn record_answer(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    key: &'static str,
    answer: &Answer,
    finished: bool,
) -> Result<()> {
    let state = if answer.deferral.is_some() {
        "deferred"
    } else {
        "answered"
    };
    let result = serde_json::to_value(answer).map_err(|_| Error::Internal)?;
    let tenant = scope.tenant();
    let summary = json!({"request": id, "store": key, "state": state, "finished": finished});
    tx.mutate(move |sql| {
        Box::pin(async move {
            let row: i64 = sql
                .query_one(
                    "UPDATE base_privacy_request_stores SET state=$4,result=$5,finished=$6,answered_at=coalesce(answered_at,clock_timestamp()) \
                     WHERE tenant_id=$1 AND request_id=$2 AND store=$3 RETURNING id",
                    &[&tenant, &id, &key, &state, &result, &finished],
                )
                .await?
                .get(0);
            Ok(((), vec![Change { kind: STORE_KIND.into(), id: row, old: json!({}), new: summary }]))
        })
    })
    .await
}

/// Once every store has answered (and erasures have finished), queue the finish job.
async fn finish_when_done(
    privacy: &Privacy,
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
) -> Result<()> {
    // Store jobs run concurrently. Locking the request makes their answers
    // take turns, so the last one counts after every other has committed;
    // without it, stores answering together each see the others open and no
    // one queues the finish. NO KEY UPDATE, because staged exports already
    // hold a key-share lock on the request through their foreign key.
    tx.sql()
        .query_one(
            "SELECT 1 FROM base_privacy_requests WHERE tenant_id=$1 AND id=$2 FOR NO KEY UPDATE",
            &[&scope.tenant(), &id],
        )
        .await?;
    let open: i64 = tx
        .sql()
        .query_one(
            "SELECT count(*) FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2 AND (state='pending' OR NOT finished)",
            &[&scope.tenant(), &id],
        )
        .await?
        .get(0);
    if open == 0 {
        privacy
            .queue
            .enqueue_for(tx, scope, FINISH_WORKER, json!({"request_id": id}))
            .await?;
    }
    Ok(())
}

pub(crate) struct FinishJob {
    pub(crate) privacy: Arc<Privacy>,
}
impl Handler for FinishJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> JobFuture<'a> {
        Box::pin(async move {
            let Some(id) = args.get("request_id").and_then(Value::as_i64) else {
                return Outcome::Cancel("invalid_request_reference");
            };
            let Some(scope) = execution.scope() else {
                return Outcome::Cancel("needs_approver");
            };
            // Boxed: the certificate's A4 render nests deep futures.
            match Box::pin(finish(&self.privacy, execution, scope, id)).await {
                Ok(Finish::Done) => Outcome::Completed,
                // Another finish holds the request, or its stores read again:
                // not a failure, so nothing is recorded and it is not late.
                Ok(Finish::Busy) => Outcome::Retry("finish_busy"),
                Err(Error::NotFound) if !request_exists(&self.privacy, scope, id).await => {
                    Outcome::Cancel("request_unavailable")
                }
                Err(error) => {
                    // Shown on the request; the queue retries.
                    let _ = finish_failed(&self.privacy, execution, scope, id, &error).await;
                    Outcome::Retry("finish_failed")
                }
            }
        })
    }
}

/// Records one failed finish attempt on the request, in its own transaction.
async fn finish_failed(
    privacy: &Privacy,
    execution: &Execution,
    scope: &Scope,
    id: i64,
    error: &Error,
) -> Result<()> {
    let code = match error {
        Error::Invalid => "export_too_large",
        Error::Internal | Error::Database(_) | Error::Pool(_) => "finish_failed",
        error => code(error),
    };
    let mut client = privacy.db.get().await?;
    let mut tx = execution
        .begin(&mut client, "privacy.request.finish_failed")
        .await?;
    let tenant = scope.tenant();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let attempts: i32 = sql
                .query_opt(
                    "UPDATE base_privacy_requests SET finish_attempts=finish_attempts+1,finish_error=$3 \
                     WHERE tenant_id=$1 AND id=$2 AND status='running' RETURNING finish_attempts",
                    &[&tenant, &id, &code],
                )
                .await?
                .ok_or(Error::NotFound)?
                .get(0);
            let change = Change {
                kind: requests::REQUEST_KIND.into(),
                id,
                old: json!({}),
                new: json!({"finish_failed": code, "finish_attempts": attempts}),
            };
            Ok(((), vec![change]))
        })
    })
    .await?;
    tx.commit().await
}

/// Holds the request's finish guard on its own connection; dropping it
/// without `release` closes the connection, which ends the guard.
struct FinishGuard(Option<deadpool_postgres::Object>, i64);
impl FinishGuard {
    async fn take(db: &Db, id: i64) -> Result<Self> {
        let client = db.get().await?;
        let taken: bool = client
            .query_one(
                "SELECT pg_try_advisory_lock($1,$2::bigint::int)",
                &[&FINISH_GUARD, &(id % i64::from(i32::MAX))],
            )
            .await?
            .get(0);
        if !taken {
            return Err(Error::Conflict);
        }
        Ok(Self(Some(client), id))
    }
    async fn release(mut self) -> Result<()> {
        if let Some(client) = self.0.take() {
            client
                .execute(
                    "SELECT pg_advisory_unlock($1,$2::bigint::int)",
                    &[&FINISH_GUARD, &(self.1 % i64::from(i32::MAX))],
                )
                .await?;
        }
        Ok(())
    }
}
impl Drop for FinishGuard {
    fn drop(&mut self) {
        if let Some(client) = self.0.take() {
            drop(deadpool_postgres::Object::take(client));
        }
    }
}

/// How a finish attempt ended without an error.
enum Finish {
    Done,
    /// Another finish holds the request, or a store is answering again.
    Busy,
}

async fn finish(
    privacy: &Privacy,
    execution: &Execution,
    scope: &Scope,
    id: i64,
) -> Result<Finish> {
    let guard = match FinishGuard::take(&privacy.db, id).await {
        Ok(guard) => guard,
        Err(Error::Conflict) => return Ok(Finish::Busy),
        Err(error) => return Err(error),
    };
    let mut client = privacy.db.get().await?;
    let request = privacy.request(&**client, scope, id).await?;
    if request.status != "running" {
        guard.release().await?;
        return Ok(Finish::Done);
    }
    if request.stores.iter().any(|s| s.state == "pending") {
        guard.release().await?;
        return Ok(Finish::Busy);
    }
    requests::eligible(privacy, &**client, scope, request.company).await?;
    let access = request.kind != RequestKind::Erasure;
    if access && !current_export(&**client, scope, &request).await? {
        // An erasure of the person was approved since the stores read: read again.
        restart_export(privacy, execution, scope, &request).await?;
        guard.release().await?;
        return Ok(Finish::Done);
    }
    if request.kind == RequestKind::Erasure {
        // The person's earlier exports go before their requests forget them,
        // and before the certificate is filed, so a failing deletion never
        // files another retained certificate on each retry; deleting an
        // already deleted one is a no-op, so a retry is safe.
        let grant =
            privacy
                .issuer
                .erasure(scope.tenant(), Cause::Request(id), Some(request.person));
        documents::delete_exports(privacy, &mut client, scope, &grant, request.person).await?;
    }
    let artifact = match request.kind {
        RequestKind::Access | RequestKind::Portability => match &request.export_artifact {
            Some(id) => id.clone(),
            None => Box::pin(documents::export(privacy, &mut client, scope, &request)).await?,
        },
        RequestKind::Erasure => match &request.certificate_artifact {
            Some(id) => id.clone(),
            None => {
                Box::pin(documents::certificate(
                    privacy,
                    &mut client,
                    scope,
                    &request,
                ))
                .await?
            }
        },
    };
    let mut tx = execution
        .begin(&mut client, "privacy.request.complete")
        .await?;
    if access {
        // Publication takes turns with erasure approval: check once more
        // under the guard, and never publish what an erasure has overtaken.
        requests::person_guard(tx.sql(), scope.tenant(), request.person, false).await?;
        if !current_export(tx.sql(), scope, &request).await? {
            tx.rollback().await?;
            documents::discard_export(privacy, &mut client, scope, &request, &artifact).await?;
            restart_export(privacy, execution, scope, &request).await?;
            guard.release().await?;
            return Ok(Finish::Done);
        }
    }
    complete(&mut tx, scope, &request, artifact).await?;
    privacy
        .engine
        .transition_in(
            &mut tx,
            scope,
            SUBJECT,
            id,
            "completed",
            bilibi_workflow::Request::default(),
        )
        .await?;
    tx.commit().await?;
    guard.release().await?;
    Ok(Finish::Done)
}

/// Whether every store of the request has its staged export, each read in
/// the person's current erasure generation, with no erasure of the person
/// running. A staged export an erasure dropped counts as stale too: an
/// export is never published with a store missing.
async fn current_export(
    sql: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    request: &RequestView,
) -> Result<bool> {
    let (generation, erasing) =
        requests::erasure_generation(sql, scope.tenant(), request.person).await?;
    let row = sql
        .query_one(
            "SELECT (SELECT count(*) FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2), \
             (SELECT count(*) FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2 AND generation=$3), \
             (SELECT count(*) FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2)",
            &[&scope.tenant(), &request.id, &generation],
        )
        .await?;
    let (stores, current, staged): (i64, i64, i64) = (row.get(0), row.get(1), row.get(2));
    Ok(!erasing && stores > 0 && current == stores && staged == stores)
}

/// Drops the staged exports and asks every store again, in one transaction.
async fn restart_export(
    privacy: &Privacy,
    execution: &Execution,
    scope: &Scope,
    request: &RequestView,
) -> Result<()> {
    let mut client = privacy.db.get().await?;
    let mut tx = execution
        .begin(&mut client, "privacy.request.export_restarted")
        .await?;
    let (tenant, id) = (scope.tenant(), request.id);
    tx.mutate(move |sql| {
        Box::pin(async move {
            let mut changes = Vec::new();
            for row in sql
                .query(
                    "DELETE FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2 RETURNING id,store",
                    &[&tenant, &id],
                )
                .await?
            {
                changes.push(Change {
                    kind: EXPORT_KIND.into(),
                    id: row.get(0),
                    old: json!({"request": id, "store": row.get::<_, String>(1), "staged": true}),
                    new: json!({}),
                });
            }
            // A finish error belongs to the export being restarted.
            if let Some(row) = sql
                .query_opt(
                    "UPDATE base_privacy_requests r SET finish_error=NULL FROM (SELECT finish_error FROM base_privacy_requests \
                     WHERE tenant_id=$1 AND id=$2 AND finish_error IS NOT NULL FOR UPDATE) o WHERE r.tenant_id=$1 AND r.id=$2 RETURNING o.finish_error",
                    &[&tenant, &id],
                )
                .await?
            {
                changes.push(Change {
                    kind: requests::REQUEST_KIND.into(),
                    id,
                    old: json!({"finish_error": row.get::<_, String>(0)}),
                    new: json!({"finish_error": null}),
                });
            }
            for row in sql
                .query(
                    "UPDATE base_privacy_request_stores SET state='pending',result=NULL,answered_at=NULL,finished=false \
                     WHERE tenant_id=$1 AND request_id=$2 RETURNING id,store",
                    &[&tenant, &id],
                )
                .await?
            {
                changes.push(Change {
                    kind: STORE_KIND.into(),
                    id: row.get(0),
                    old: json!({"state": "answered"}),
                    new: json!({"request": id, "store": row.get::<_, String>(1), "state": "pending", "restarted": "an erasure of the person ran since the store read"}),
                });
            }
            Ok(((), changes))
        })
    })
    .await?;
    for (key, _, _) in privacy.stores.iter() {
        privacy
            .queue
            .enqueue_for(
                &mut tx,
                scope,
                STORE_WORKER,
                json!({"request_id": id, "store": key}),
            )
            .await?;
    }
    tx.commit().await
}

/// Files the artifact on the request and drops its staged exports; for an
/// erasure, every request about the person forgets its personal text.
async fn complete(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    request: &RequestView,
    artifact: String,
) -> Result<()> {
    let (tenant, id, person) = (scope.tenant(), request.id, request.person);
    let before = requests::evidence(request);
    let erasure = request.kind == RequestKind::Erasure;
    tx.mutate(move |sql| {
        Box::pin(async move {
            if erasure {
                sql.execute(
                    "UPDATE base_privacy_requests SET certificate_artifact=$3,finish_error=NULL WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id, &artifact],
                )
                .await?;
            } else {
                sql.execute(
                    "UPDATE base_privacy_requests SET export_artifact=$3,finish_error=NULL WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id, &artifact],
                )
                .await?;
            }
            let staged = sql
                .query(
                    "DELETE FROM base_privacy_request_exports WHERE tenant_id=$1 AND request_id=$2 RETURNING id,store",
                    &[&tenant, &id],
                )
                .await?;
            let after = requests::load(sql, tenant, id, false).await?;
            let mut changes = vec![Change {
                kind: requests::REQUEST_KIND.into(),
                id,
                old: before,
                new: requests::evidence(&after),
            }];
            for row in staged {
                changes.push(Change {
                    kind: EXPORT_KIND.into(),
                    id: row.get(0),
                    old: json!({"request": id, "store": row.get::<_, String>(1), "staged": true}),
                    new: json!({}),
                });
            }
            if erasure {
                changes.extend(requests::forget_person(sql, tenant, person).await?);
                sql.execute(
                    "UPDATE base_privacy_requests SET export_artifact=NULL WHERE tenant_id=$1 AND person_id=$2 AND export_artifact IS NOT NULL",
                    &[&tenant, &person.get()],
                )
                .await?;
                // A hold or a retention date kept records, or a store deferred:
                // the cleanup they owe is persisted and retried by the daily
                // retention run, from the earliest date one ends (at once
                // while a hold keeps one), until nothing is kept for those
                // reasons.
                if let Some(row) = sql
                    .query_opt(
                        &format!(
                            "INSERT INTO base_privacy_resumptions(tenant_id,request_id,person_id,not_before) \
                             SELECT $1,$2,$3,CASE WHEN bool_or(o.after IS NULL) THEN NULL ELSE min(o.after) END \
                             FROM (SELECT base_privacy_owed_after(s.result) AS after FROM base_privacy_request_stores s \
                               WHERE s.tenant_id=$1 AND s.request_id=$2 AND {OWING}) o HAVING count(*) > 0 \
                             ON CONFLICT (request_id) DO NOTHING RETURNING id"
                        ),
                        &[&tenant, &id, &person.get()],
                    )
                    .await?
                {
                    changes.push(Change {
                        kind: RESUMPTION_KIND.into(),
                        id: row.get(0),
                        old: json!({}),
                        new: json!({"request": id, "owed": true}),
                    });
                }
            }
            Ok(((), changes))
        })
    })
    .await
}

/// A request store (`s`) whose erasure answer owes cleanup, in SQL; the same
/// rule as [`owes`].
const OWING: &str = "(s.state='deferred' OR EXISTS (SELECT 1 FROM jsonb_array_elements(coalesce(s.result->'kept','[]'::jsonb)) k \
                     WHERE k->>'reason' IN ('legal_hold','retained_until')))";

/// Whether an erasure's answer still owes cleanup: a record kept for a legal
/// hold or a retention date, or a store that deferred.
fn owes(answer: &Answer) -> bool {
    answer.deferral.is_some()
        || answer
            .kept
            .iter()
            .any(|k| matches!(k.reason, KeptReason::LegalHold | KeptReason::RetainedUntil))
}

/// Due owed cleanups per retention run, oldest first.
const RESUME_BATCH: i64 = 200;

impl Privacy {
    /// Cleanup that completed erasures still owe because a legal hold or a
    /// retention date kept records, or a store deferred, once due: each store
    /// that kept or deferred erases the person again as the scheduler, under
    /// the original request's grant and the hold guard. Records still kept
    /// for those reasons stay owed, due again when the first of them ends (at
    /// the next run while a hold keeps one); once none are the cleanup is
    /// done. Runs whatever the class settings say. One failing cleanup is
    /// recorded on its row and the others still run; the run then reports
    /// failure.
    pub(crate) async fn resume_erasures(
        &self,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        minimums: &Minimums,
    ) -> Result<()> {
        let open: Vec<(i64, i64)> = client
            .query(
                "SELECT id,request_id FROM base_privacy_resumptions WHERE tenant_id=$1 AND done_at IS NULL \
                 AND (not_before IS NULL OR not_before <= clock_timestamp()) ORDER BY id LIMIT $2",
                &[&scope.tenant(), &RESUME_BATCH],
            )
            .await?
            .iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect();
        let mut failed = false;
        for (resumption, request) in open {
            let outcome = Box::pin(self.resume(client, scope, request, minimums)).await;
            let (result, owed) = match &outcome {
                Ok(answer) => (
                    serde_json::to_value(answer).map_err(|_| Error::Internal)?,
                    owes(answer),
                ),
                Err(error) => {
                    failed = true;
                    (json!({"error": code(error)}), true)
                }
            };
            let mut tx = self
                .boundary
                .begin(client, scope.mutation_context("privacy.request.resume"))
                .await?;
            let tenant = scope.tenant();
            tx.mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE base_privacy_resumptions SET runs=runs+1,last_run_at=clock_timestamp(),last_result=$3, \
                         not_before=base_privacy_owed_after($3), \
                         done_at=CASE WHEN $4 THEN NULL ELSE clock_timestamp() END WHERE tenant_id=$1 AND id=$2 AND done_at IS NULL",
                        &[&tenant, &resumption, &result, &owed],
                    )
                    .await?;
                    Ok(((), vec![Change {
                        kind: RESUMPTION_KIND.into(),
                        id: resumption,
                        old: json!({"owed": true}),
                        new: json!({"request": request, "owed": owed, "result": result}),
                    }]))
                })
            })
            .await?;
            tx.commit().await?;
        }
        if failed {
            Err(Error::Internal)
        } else {
            Ok(())
        }
    }

    /// One owed cleanup: every store that kept or deferred at the request, as
    /// then, keeping what is still held. The others answered in full and are
    /// not asked again. Returns what it did and kept.
    async fn resume(
        &self,
        client: &mut tokio_postgres::Client,
        scope: &Scope,
        id: i64,
        minimums: &Minimums,
    ) -> Result<Answer> {
        let tenant = scope.tenant();
        let mut request = requests::load(&*client, tenant, id, false).await?;
        if request.kind != RequestKind::Erasure || request.status != "completed" {
            return Err(Error::Conflict);
        }
        request.confirmed = requests::confirmed(&*client, tenant, id).await?;
        let grant = self
            .issuer
            .erasure(tenant, Cause::Request(id), Some(request.person));
        let owing: Vec<String> = client
            .query(
                &format!(
                    "SELECT s.store FROM base_privacy_request_stores s WHERE s.tenant_id=$1 AND s.request_id=$2 AND {OWING}"
                ),
                &[&tenant, &id],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        let answer = std::sync::Mutex::new(Answer::default());
        for (key, _, data) in self.stores.iter() {
            if !owing.iter().any(|store| store.as_str() == key) {
                continue;
            }
            let subject = requests::subject_for(&request, key);
            let holds = Holds::acquire(tenant, Some(request.person)).await?;
            let mut tx = self
                .boundary
                .begin(client, scope.mutation_context("privacy.request.resume"))
                .await?;
            requests::person_guard(tx.sql(), tenant, request.person, true).await?;
            let mut erased = data
                .handler
                .erase(&mut tx, scope, &subject, &grant, &holds)
                .await?;
            clear_evidence(&tx, &grant, &mut erased, minimums).await?;
            tx.commit().await?;
            holds.release().await?;
            answer.lock().map_err(|_| Error::Internal)?.add(&erased);
            let holds = Holds::acquire(tenant, Some(request.person)).await?;
            let steps = Resumed {
                privacy: self,
                scope,
                grant: &grant,
                minimums,
                answer: &answer,
            };
            data.handler
                .after_erase(client, scope, &subject, &grant, &holds, &steps)
                .await?;
            holds.release().await?;
        }
        answer.into_inner().map_err(|_| Error::Internal)
    }
}

/// `after_erase` steps of an owed cleanup, each committed as reported.
struct Resumed<'a> {
    privacy: &'a Privacy,
    scope: &'a Scope,
    grant: &'a Erasure,
    minimums: &'a Minimums,
    answer: &'a std::sync::Mutex<Answer>,
}
impl Progress for Resumed<'_> {
    fn record<'a>(&'a self, mut step: Erased) -> HandlerFuture<'a, ()> {
        Box::pin(async move {
            let mut client = self.privacy.db.get().await?;
            let tx = self
                .privacy
                .boundary
                .begin(
                    &mut client,
                    self.scope.mutation_context("privacy.request.resume"),
                )
                .await?;
            clear_evidence(&tx, self.grant, &mut step, self.minimums).await?;
            tx.commit().await?;
            self.answer.lock().map_err(|_| Error::Internal)?.add(&step);
            Ok(())
        })
    }
}
