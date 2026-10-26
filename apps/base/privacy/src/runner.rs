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
                Err(Error::NotFound) => Outcome::Cancel("request_unavailable"),
                Err(error) => {
                    // Shown on the request as late; the queue retries.
                    let _ = failed(&self.privacy, execution, scope, id, store, code(&error)).await;
                    Outcome::Retry("store_failed")
                }
            }
        })
    }
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
    /// A company's legal minimum for audit evidence, in days: evidence newer
    /// than this stays until it has passed (`privacy.audit.legal_minimum_days`).
    pub(crate) async fn evidence_minimum(
        &self,
        client: &tokio_postgres::Client,
        scope: &Scope,
        company: bilibi_party_ids::OrganisationId,
    ) -> Result<Option<i32>> {
        Ok(self
            .settings
            .get(
                client,
                scope,
                bilibi_settings::Target::Company(company),
                AUDIT_MINIMUM,
            )
            .await?
            .value
            .as_i64()
            .and_then(|days| i32::try_from(days).ok())
            .filter(|days| (1..=36_500).contains(days)))
    }
}

/// Clears the audit evidence of every record a step erased, in the step's
/// transaction, and lists evidence kept for its legal minimum.
pub(crate) async fn clear_evidence(
    tx: &MutationTransaction<'_>,
    grant: &Erasure,
    step: &mut Erased,
    keep_days: Option<i32>,
) -> Result<()> {
    for record in &step.records {
        let cleared =
            bilibi_audit::erase_record(tx, grant, &record.kind, record.id, keep_days).await?;
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
                                "INSERT INTO base_privacy_request_exports(request_id,tenant_id,store,records,files) VALUES($1,$2,$3,$4,$5) RETURNING id",
                                &[&id, &tenant, &key, &records, &files],
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
            let keep_days = privacy
                .evidence_minimum(&client, scope, request.company)
                .await?;
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
                    clear_evidence(&tx, &grant, &mut erased, keep_days).await?;
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
                keep_days,
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
    keep_days: Option<i32>,
}
impl Progress for Steps<'_> {
    fn record<'a>(&'a self, mut step: Erased) -> HandlerFuture<'a, ()> {
        Box::pin(async move {
            let mut client = self.db.get().await?;
            let mut tx = self
                .execution
                .begin(&mut client, "privacy.request.erase_step")
                .await?;
            clear_evidence(&tx, self.grant, &mut step, self.keep_days).await?;
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
                Ok(()) => Outcome::Completed,
                Err(Error::NotFound) => Outcome::Cancel("request_unavailable"),
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

async fn finish(privacy: &Privacy, execution: &Execution, scope: &Scope, id: i64) -> Result<()> {
    let guard = FinishGuard::take(&privacy.db, id).await?;
    let mut client = privacy.db.get().await?;
    let request = privacy.request(&**client, scope, id).await?;
    if request.status != "running" {
        return guard.release().await;
    }
    if request.stores.iter().any(|s| s.state == "pending") {
        return Err(Error::Conflict);
    }
    requests::eligible(privacy, &**client, scope, request.company).await?;
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
    if request.kind == RequestKind::Erasure {
        // The person's earlier exports go before their requests forget them;
        // deleting an already deleted one is a no-op, so a retry is safe.
        let grant =
            privacy
                .issuer
                .erasure(scope.tenant(), Cause::Request(id), Some(request.person));
        documents::delete_exports(privacy, &mut client, scope, &grant, request.person).await?;
    }
    let mut tx = execution
        .begin(&mut client, "privacy.request.complete")
        .await?;
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
    guard.release().await
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
            }
            Ok(((), changes))
        })
    })
    .await
}
