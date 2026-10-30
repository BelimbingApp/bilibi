//! Agent operations over the operator facade: the tenant's subscribers and
//! their gaps, its deliveries, and a reviewed replay. The same `Events`
//! methods the Events page calls, reached through Agent API's plans.
//! No Bilimbi counterpart.
//!
//! The replay is a Base Step-up operation (`base.events.replay`), off unless
//! the tenant turns it on: a replay only hands over facts already recorded,
//! and never twice. A commit may answer `step_up_required`; the agent asks its
//! person for a code and commits the same plan again.
use crate::{Events, ReplayFrom, SubscriberState, REPLAY, STEP_UP_REPLAY, VIEW};
use bilibi_agent_api::{
    json::{Data, Output},
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, ReadHandler, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_tenancy::Scope;
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static EVENTS: OnceLock<Arc<Events>> = OnceLock::new();
/// Bound once at boot by the `base/events.agent` service.
pub(crate) fn bind(events: Arc<Events>) -> bool {
    EVENTS.set(events).is_ok()
}
fn events() -> bilibi_agent_api::Result<&'static Arc<Events>> {
    EVENTS.get().ok_or(Error::Internal.into())
}

fn field_error(field: &str, message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([(field.into(), vec![message.into()])]))
}

fn time(at: Timestamp) -> String {
    at.to_string()
}
fn subscriber_json(state: &SubscriberState) -> Value {
    json!({
        "subscriber": state.subscriber,
        "mounted": state.mounted,
        "events": state.events,
        "last_delivered": state.last_delivered.map(|last| json!({
            "event_id": last.event_id,
            "recorded_at": time(last.recorded_at),
            "delivered_at": time(last.delivered_at),
        })),
        "pending": state.pending,
        "failing": state.failing,
        "first_pending": state.first_pending,
        "missed": state.missed,
        "first_missed": state.first_missed,
        "gap_from": state.gap_from(),
    })
}

enum Read {
    Subscribers,
    Deliveries,
}
impl ReadHandler for Read {
    fn owner(&self) -> &'static str {
        "base/events"
    }
    fn call<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let events = events()?;
            let client = events.db.get().await.map_err(Error::from)?;
            let data = match self {
                Self::Subscribers => {
                    let states = events.subscriber_states(&**client, scope).await?;
                    json!({"subscribers": states.iter().map(subscriber_json).collect::<Vec<_>>()})
                }
                Self::Deliveries => {
                    let rows = events
                        .deliveries(
                            &**client,
                            scope,
                            input["from_event"].as_i64().unwrap_or(1),
                            input["pending_only"].as_bool().unwrap_or(false),
                            input["limit"].as_i64().unwrap_or(100),
                        )
                        .await?;
                    json!({"deliveries": rows.iter().map(|row| json!({
                        "event_id": row.event_id, "name": row.name, "version": row.version,
                        "subject": row.subject, "key": row.key, "recorded_at": row.recorded_at,
                        "subscriber": row.subscriber, "mounted": row.mounted, "attempts": row.attempts,
                        "last_error": row.last_error, "failed_at": row.failed_at, "delivered_at": row.delivered_at,
                    })).collect::<Vec<_>>()})
                }
            };
            Ok(Output::Data(Data::Json(data)))
        })
    }
}

/// The replay's input: a subscriber, exactly one of `from_event` or
/// `from_time`, and an optional batch limit.
fn replay_input(input: &Value) -> bilibi_agent_api::Result<(&str, ReplayFrom, i64)> {
    let subscriber = input["subscriber"].as_str().unwrap_or_default();
    let from = match (input.get("from_event"), input.get("from_time")) {
        (Some(id), None) => ReplayFrom::Event(id.as_i64().unwrap_or_default()),
        (None, Some(at)) => ReplayFrom::Time(
            at.as_str()
                .unwrap_or_default()
                .parse()
                .map_err(|_| field_error("from_time", "must be a date-time with an offset"))?,
        ),
        _ => {
            return Err(field_error(
                "from_event",
                "give exactly one of from_event or from_time",
            ))
        }
    };
    Ok((subscriber, from, input["limit"].as_i64().unwrap_or(1000)))
}
fn refusal(error: Error) -> AgentError {
    match error {
        Error::NotFound => field_error("subscriber", "is not a mounted subscriber"),
        error => error.into(),
    }
}

struct Replay;
impl WriteHandler for Replay {
    fn owner(&self) -> &'static str {
        "base/events"
    }
    fn preview<'a>(
        &'a self,
        _client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (subscriber, from, limit) = replay_input(input)?;
            let events = events()?;
            let client = events.db.get().await.map_err(Error::from)?;
            let review = events
                .replay_review(&**client, scope, subscriber, from, limit)
                .await
                .map_err(refusal)?;
            let mut facts = review.to_json();
            facts["subscriber"] = json!(subscriber);
            let erased = if review.erased > 0 {
                format!(
                    "; {} events a personal-data erasure changed are left out",
                    review.erased
                )
            } else {
                String::new()
            };
            facts["summary"] = json!(if review.pending == 0 {
                format!("Nothing to replay to {subscriber}: every event in the batch is delivered{erased}")
            } else {
                format!(
                    "Hand {} events to {subscriber} ({} new deliveries); delivered events are skipped{erased}",
                    review.pending, review.created
                )
            });
            facts["step_up"] = json!([STEP_UP_REPLAY]);
            Ok(facts.into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let (subscriber, from, limit) = replay_input(input)?;
            let replay = events()?
                .replay_in(tx, scope, subscriber, from, limit)
                .await
                .map_err(refusal)?;
            let mut result = replay.to_json();
            result["subscriber"] = json!(subscriber);
            Ok(result.into())
        })
    }
}

fn subscribers_schema() -> Value {
    json!({"type":"object","properties":{}})
}
fn deliveries_schema() -> Value {
    json!({"type":"object","properties":{
        "from_event":{"type":"integer","minimum":1,"description":"The first event ID listed; default 1"},
        "pending_only":{"type":"boolean","description":"Leave out delivered rows"},
        "limit":{"type":"integer","minimum":1,"maximum":500,"description":"Default 100"}
    }})
}
fn replay_schema() -> Value {
    json!({"type":"object","required":["subscriber"],"properties":{
        "subscriber":{"type":"string","maxLength":200,"description":"A mounted subscriber key, from base.events.subscriber.list"},
        "from_event":{"type":"integer","minimum":1,"description":"Replay from this event ID on (give this or from_time)"},
        "from_time":{"type":"string","format":"date-time","description":"Replay from the first event recorded at or after this instant (give this or from_event)"},
        "limit":{"type":"integer","minimum":1,"maximum":1000,"description":"Events looked at in one batch; default 1000. `next` names where the following batch starts"}
    }})
}
fn subscribers_output() -> Value {
    json!({"type":"object","description":"Each subscriber: key, mounted, event names it reads, last delivered event, pending and failing deliveries, missed events (no delivery at all) and gap_from, the event a replay closing the gap starts at."})
}
fn deliveries_output() -> Value {
    json!({"type":"object","description":"Deliveries in event then subscriber order: event, version, subject, key, subscriber, mounted, attempts, last failure code and receipt time."})
}
fn replay_output() -> Value {
    json!({"type":"object","description":"The batch looked at (from, through, next), deliveries created and events now pending for the subscriber; pending 0 means nothing was written. erased counts events a personal-data erasure changed, which get no delivery."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "base/events", Provenance { module: "base/events", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key:"base.events.subscriber.list", title:"List event subscribers", summary:"Every subscriber of this tenant's events: whether it is mounted, the last event delivered to it, pending deliveries and the events it missed while unmounted.", keywords:&["event","subscriber","delivery","gap","replay","remount"], capability:VIEW, input:subscribers_schema, output:subscribers_output, handler:Handler::Read(&Read::Subscribers),channels:Channels::Every,step_up:&[] },
        Operation { key:"base.events.delivery.list", title:"List event deliveries", summary:"This tenant's event deliveries from an event ID on, with attempts and the last failure code.", keywords:&["event","delivery","pending","failure"], capability:VIEW, input:deliveries_schema, output:deliveries_output, handler:Handler::Read(&Read::Deliveries),channels:Channels::Every,step_up:&[] },
        Operation { key:"base.events.replay", title:"Replay events to a subscriber", summary:"Hand a mounted subscriber the events it has no delivery for, and requeue its pending ones, from an event ID or a time on. Delivered events are skipped, so a repeat does nothing. Step-up: base.events.replay.", keywords:&["event","replay","subscriber","remount","gap","delivery"], capability:REPLAY, input:replay_schema, output:replay_output, handler:Handler::SafeWrite(&Replay),channels:Channels::Every,step_up:&[STEP_UP_REPLAY] },
    ],
    guides: &[Guide { key:"base.events", title:"Event replay", summary:"Find a subscriber's gap after a remount and close it.", keywords:&["event","replay","remount","gap","subscriber"], capability:VIEW,
        body:"A module in one Domain or Extension reacts to another's facts through events. Deliveries are created for the subscribers mounted when an event is recorded, so a subscriber unmounted at that time never receives it. List subscribers with base.events.subscriber.list: missed counts events it has no delivery for and gap_from is where a replay closing the gap starts. Replay with base.events.replay from gap_from (or an earlier event or a time): plan, show the review to the person, then commit. Replaying is safe to repeat; delivered events are skipped. If next is not null, plan again from next. Delivery runs in the background; read the subscriber list again to see it finish. A commit may answer step_up_required; ask the person for the current code from their authenticator app and commit the same plan again with step_up_code." }],
}) }
