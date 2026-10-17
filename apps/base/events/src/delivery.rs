//! The `base/events-delivery` job: one per recorded event (and per replayed
//! subject), running as `domain-events`. For each mounted subscriber still
//! pending on the event it walks that subscriber's pending events of the same
//! subject in recording order, up to this event, one transaction each: lock
//! the delivery row, run the subscriber, set the receipt, audit, commit. A
//! refusal stops that subscriber's walk (later events of the subject wait) and
//! the job retries with the queue's backoff; other subscribers carry on.
use crate::{Event, Events, Subscriber};
use bilibi_database::{Change, Error, Result};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Identity, Outcome};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::task::JoinSet;

pub(crate) struct Delivery {
    pub events: Arc<Events>,
}
impl Handler for Delivery {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args.get("event_id").and_then(Value::as_i64) else {
                return Outcome::Cancel("invalid_event_reference");
            };
            match deliver(&self.events, execution, id).await {
                Ok(true) => Outcome::Completed,
                Ok(false) => Outcome::Retry("event_delivery_pending"),
                Err(Error::NotFound) => Outcome::Cancel("event_unavailable"),
                Err(_) => Outcome::Retry("event_store_unavailable"),
            }
        })
    }
}

/// Whether every mounted subscriber now holds a receipt for event `id`.
/// Deliveries to an unmounted subscriber stay pending, for a replay once it
/// is mounted again; they do not hold the job.
async fn deliver(events: &Arc<Events>, execution: &Execution, id: i64) -> Result<bool> {
    let client = events.db.get().await?;
    // Tenant comes from the job's proven identity, never from the event row.
    let row = client
        .query_opt(
            "SELECT publisher,subject FROM base_events WHERE id=$1 AND tenant_id=$2",
            &[&id, &execution.tenant()],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let (publisher, subject): (String, String) = (row.get(0), row.get(1));
    let pending = pending(&client, id).await?;
    drop(client);
    for subscriber in pending
        .iter()
        .filter_map(|key| events.subscribers.get_key_value(key.as_str()))
    {
        walk(events, execution, subscriber, &publisher, &subject, id).await?;
    }
    let client = events.db.get().await?;
    Ok(pending_mounted(events, &client, id).await? == 0)
}
async fn pending(client: &tokio_postgres::Client, id: i64) -> Result<Vec<String>> {
    Ok(client
        .query(
            "SELECT subscriber FROM base_events_deliveries WHERE event_id=$1 AND delivered_at IS NULL ORDER BY subscriber",
            &[&id],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect())
}
async fn pending_mounted(
    events: &Events,
    client: &tokio_postgres::Client,
    id: i64,
) -> Result<usize> {
    Ok(pending(client, id)
        .await?
        .iter()
        .filter(|key| events.subscribers.contains_key(key.as_str()))
        .count())
}

enum Step {
    Delivered,
    Done,
    Refused(i64, &'static str),
}
const PENDING: &str = "SELECT e.id,e.name,e.version,e.event_key,e.payload,e.recorded_at::text,d.attempts FROM base_events_deliveries d JOIN base_events e ON e.id=d.event_id WHERE d.subscriber=$1 AND d.delivered_at IS NULL AND e.tenant_id=$2 AND e.publisher=$3 AND e.subject=$4 ORDER BY e.id LIMIT 1";

async fn walk(
    events: &Arc<Events>,
    execution: &Execution,
    (key, subscriber): (&&'static str, &&'static Subscriber),
    publisher: &str,
    subject: &str,
    target: i64,
) -> Result<()> {
    loop {
        // A task boundary catches a panicking subscriber; dropping this future
        // (lease timeout, shutdown) aborts the child and rolls its SQL back.
        let mut task = JoinSet::new();
        let (events_, execution_) = (events.clone(), execution.clone());
        let (key, subscriber) = (*key, *subscriber);
        let (publisher_, subject_) = (publisher.to_owned(), subject.to_owned());
        task.spawn(async move {
            next(
                &events_,
                &execution_,
                key,
                subscriber,
                &publisher_,
                &subject_,
                target,
            )
            .await
        });
        let step = match task.join_next().await {
            Some(Ok(step)) => step?,
            _ => {
                let client = events.db.get().await?;
                match client
                    .query_opt(PENDING, &[&key, &execution.tenant(), &publisher, &subject])
                    .await?
                {
                    Some(row) => Step::Refused(row.get(0), "subscriber_panicked"),
                    None => Step::Done,
                }
            }
        };
        match step {
            Step::Delivered => continue,
            Step::Done => return Ok(()),
            Step::Refused(event, code) => {
                let client = events.db.get().await?;
                client.execute(
                    "UPDATE base_events_deliveries SET attempts=attempts+1,last_error=$3,failed_at=clock_timestamp() WHERE event_id=$1 AND subscriber=$2 AND delivered_at IS NULL",
                    &[&event, &key, &code],
                ).await?;
                return Ok(());
            }
        }
    }
}

/// Delivers the subject's lowest pending event to one subscriber, if it is
/// not past `target`.
async fn next(
    events: &Events,
    execution: &Execution,
    key: &'static str,
    subscriber: &'static Subscriber,
    publisher: &str,
    subject: &str,
    target: i64,
) -> Result<Step> {
    let Identity::System(scope) = execution.identity() else {
        return Err(Error::Forbidden);
    };
    let scope = scope.clone();
    let mut client = events.db.get().await?;
    // `begin` fences the job's lease and re-proves the principal's grant.
    let mut tx = execution
        .begin(&mut client, "events.subscriber.delivered")
        .await?;
    // The row lock serializes a concurrent attempt at the same delivery.
    let locked = format!("{PENDING} FOR UPDATE OF d");
    let Some(row) = tx
        .sql()
        .query_opt(&locked, &[&key, &execution.tenant(), &publisher, &subject])
        .await?
    else {
        tx.rollback().await?;
        return Ok(Step::Done);
    };
    let id: i64 = row.get(0);
    if id > target {
        tx.rollback().await?;
        return Ok(Step::Done);
    }
    let event = Event {
        id,
        tenant: execution.tenant(),
        name: row.get(1),
        version: u16::try_from(row.get::<_, i32>(2)).map_err(|_| Error::Invalid)?,
        key: row.get(3),
        payload: row.get(4),
        recorded_at: row.get(5),
        subject: subject.to_owned(),
        attempt: row.get::<_, i32>(6) + 1,
    };
    let reads = subscriber
        .events
        .iter()
        .any(|on| on.event == event.name && on.versions.contains(&event.version));
    if !reads {
        tx.rollback().await?;
        return Ok(Step::Refused(id, "unsupported_event_version"));
    }
    let handler = subscriber.handler;
    let result = tx
        .mutate(move |sql| {
            Box::pin(async move {
                let mut changes = handler.handle(sql, &scope, &event).await?;
                sql.execute(
                    "UPDATE base_events_deliveries SET attempts=attempts+1,last_error=NULL,delivered_at=clock_timestamp() WHERE event_id=$1 AND subscriber=$2",
                    &[&id, &key],
                )
                .await?;
                changes.push(Change {
                    kind: "base/events.delivery".into(),
                    id,
                    old: Value::Null,
                    new: json!({"subscriber": key, "event_key": event.key}),
                });
                Ok(((), changes))
            })
        })
        .await;
    match result {
        Ok(()) => {
            tx.commit().await?;
            Ok(Step::Delivered)
        }
        Err(error) => {
            tx.rollback().await?;
            Ok(Step::Refused(id, code(&error)))
        }
    }
}
/// Bounded failure codes; a subscriber's messages and SQL are never stored.
fn code(error: &Error) -> &'static str {
    match error {
        Error::Unauthorized | Error::Forbidden => "subscriber_forbidden",
        Error::NotFound => "subscriber_not_found",
        Error::Invalid => "subscriber_refused",
        Error::Conflict => "subscriber_conflict",
        _ => "subscriber_unavailable",
    }
}
