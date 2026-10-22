//! The request jobs (ADR 05). Each runs as the approver who started it, through
//! `Execution::begin`, which re-proves that person and `privacy.request.approve`
//! on every attempt. One store job per mounted store answers that store; once
//! every store has answered or deferred, the finish job bundles the export or
//! writes the certificate and completes the request. Every step is idempotent:
//! a retried job finds its answer recorded and moves on.
use crate::{
    documents,
    holds::Holds,
    registry::Erased,
    requests::{self, RequestKind, RequestView, EXPORT_KIND, STORE_KIND, SUBJECT},
    Privacy,
};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::Arc;

pub const STORE_WORKER: &str = "base/privacy-request-store";
pub const FINISH_WORKER: &str = "base/privacy-request-finish";

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

/// A short, stable code for the store row; never an error message, which may
/// quote personal data.
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

pub(crate) struct StoreJob {
    pub(crate) privacy: Arc<Privacy>,
}
impl Handler for StoreJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
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
            match answer(&self.privacy, execution, scope, id, store).await {
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

async fn answer(
    privacy: &Privacy,
    execution: &Execution,
    scope: &Scope,
    id: i64,
    store: &str,
) -> Result<()> {
    let mut client = privacy.db.get().await?;
    let request = requests::load(&**client, scope.tenant(), id, false).await?;
    if request.status != "running" {
        return Ok(());
    }
    let (key, data) = privacy
        .stores
        .by_key
        .get_key_value(store)
        .map(|(key, (_, data))| (*key, *data))
        .ok_or(Error::Conflict)?;
    let subject = requests::subject_of(&request);
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
            let exported = data.handler.export(&client, scope, &subject).await?;
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
            if state == "pending" {
                let mut tx = execution
                    .begin(&mut client, "privacy.request.erase")
                    .await?;
                // Marks this transaction as the request's erasure: the audit and
                // media erasure paths refuse without it.
                tx.sql()
                    .execute(
                        "SELECT set_config($1, $2, true)",
                        &[&bilibi_audit::ERASURE_MARKER, &id.to_string()],
                    )
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
                    let holds = Holds::load(tx.sql(), scope.tenant(), Some(request.person)).await?;
                    let erased = data.handler.erase(&mut tx, scope, &subject, &holds).await?;
                    for record in &erased.records {
                        bilibi_audit::erase_values(
                            &tx,
                            id,
                            &record.kind,
                            record.id,
                            &record.fields,
                        )
                        .await?;
                    }
                    record_answer(&mut tx, scope, id, key, &erased, false).await?;
                    tx.commit().await?;
                } else {
                    tx.rollback().await?;
                }
            } else if finished {
                return Ok(());
            }
            let holds = Holds::load(&**client, scope.tenant(), Some(request.person)).await?;
            let more = data
                .handler
                .after_erase(&mut client, scope, &subject, &holds)
                .await?;
            let mut tx = execution
                .begin(&mut client, "privacy.request.erase_finished")
                .await?;
            let tenant = scope.tenant();
            let current: Value = tx
                .sql()
                .query_one(
                    "SELECT result FROM base_privacy_request_stores WHERE tenant_id=$1 AND request_id=$2 AND store=$3 FOR UPDATE",
                    &[&tenant, &id, &key],
                )
                .await?
                .get(0);
            let mut erased: Erased =
                serde_json::from_value(current).map_err(|_| Error::Internal)?;
            erased.merge(more);
            record_answer(&mut tx, scope, id, key, &erased, true).await?;
            finish_when_done(privacy, &mut tx, scope, id).await?;
            tx.commit().await
        }
    }
}

/// Saves a store's erasure answer. The stored result keeps counts, kinds, IDs
/// and reasons; the per-record field names are dropped once audit is cleared.
async fn record_answer(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    key: &'static str,
    erased: &Erased,
    finished: bool,
) -> Result<()> {
    let state = if erased.deferral.is_some() {
        "deferred"
    } else {
        "answered"
    };
    let mut result = serde_json::to_value(erased).map_err(|_| Error::Internal)?;
    if let Some(object) = result.as_object_mut() {
        let count = erased.records.len();
        object.insert("records".into(), json!(count));
    }
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
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args.get("request_id").and_then(Value::as_i64) else {
                return Outcome::Cancel("invalid_request_reference");
            };
            let Some(scope) = execution.scope() else {
                return Outcome::Cancel("needs_approver");
            };
            match finish(&self.privacy, execution, scope, id).await {
                Ok(()) => Outcome::Completed,
                Err(Error::NotFound) => Outcome::Cancel("request_unavailable"),
                Err(_) => Outcome::Retry("finish_failed"),
            }
        })
    }
}

async fn finish(privacy: &Privacy, execution: &Execution, scope: &Scope, id: i64) -> Result<()> {
    let mut client = privacy.db.get().await?;
    let request = privacy.request(&**client, scope, id).await?;
    if request.status != "running" {
        return Ok(());
    }
    if request.stores.iter().any(|s| s.state == "pending") {
        return Err(Error::Conflict);
    }
    let artifact = match request.kind {
        RequestKind::Access | RequestKind::Portability => match &request.export_artifact {
            Some(id) => id.clone(),
            None => documents::export(privacy, &mut client, scope, &request).await?,
        },
        RequestKind::Erasure => match &request.certificate_artifact {
            Some(id) => id.clone(),
            None => documents::certificate(privacy, &mut client, scope, &request).await?,
        },
    };
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
    tx.commit().await
}

/// Files the artifact on the request, drops staged exports, and for an erasure
/// forgets the identifiers the requester gave.
async fn complete(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    request: &RequestView,
    artifact: String,
) -> Result<()> {
    let (tenant, id) = (scope.tenant(), request.id);
    let before = requests::evidence(request);
    let erasure = request.kind == RequestKind::Erasure;
    tx.mutate(move |sql| {
        Box::pin(async move {
            if erasure {
                sql.execute(
                    "UPDATE base_privacy_requests SET certificate_artifact=$3,identifiers='[]' WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id, &artifact],
                )
                .await?;
            } else {
                sql.execute(
                    "UPDATE base_privacy_requests SET export_artifact=$3 WHERE tenant_id=$1 AND id=$2",
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
            Ok(((), changes))
        })
    })
    .await
}
