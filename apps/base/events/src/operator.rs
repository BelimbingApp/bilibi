//! Operator reads and replay, in the operator's own tenant.
use crate::{Events, PRINCIPAL, REPLAY, STEP_UP_REPLAY, VIEW, WORKER};
use bilibi_authz::{authorize_key, SystemScope};
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use jiff::Timestamp;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_postgres::GenericClient;

/// One event's delivery to one subscriber. `mounted` is false for a
/// subscriber this build does not compose; its pending rows wait for a replay
/// once it is mounted again. Failures are bounded codes, never messages.
#[derive(Clone, Debug)]
pub struct DeliveryState {
    pub event_id: i64,
    pub name: String,
    pub version: u16,
    pub subject: String,
    pub key: String,
    pub recorded_at: String,
    pub subscriber: String,
    pub mounted: bool,
    pub attempts: i32,
    pub last_error: Option<String>,
    pub failed_at: Option<String>,
    pub delivered_at: Option<String>,
}

/// One subscriber as the tenant's outbox sees it. A mounted subscriber is
/// listed even before its first delivery; an unmounted one only while the
/// tenant holds deliveries for it. The gap is computed when it is read, so it
/// shows right after a remount with nothing written at boot.
#[derive(Clone, Debug)]
pub struct SubscriberState {
    pub subscriber: String,
    pub mounted: bool,
    /// The event names it reads; empty when it is not mounted.
    pub events: Vec<&'static str>,
    /// The latest event delivered to it.
    pub last_delivered: Option<Delivered>,
    /// Deliveries not yet delivered, and how many of them last failed.
    pub pending: i64,
    pub failing: i64,
    pub first_pending: Option<i64>,
    /// Events of a name it reads that it has no delivery for: recorded while
    /// it was unmounted, or before it was first mounted. Only a replay hands
    /// them over.
    pub missed: i64,
    pub first_missed: Option<i64>,
}
#[derive(Clone, Copy, Debug)]
pub struct Delivered {
    pub event_id: i64,
    pub recorded_at: Timestamp,
    pub delivered_at: Timestamp,
}
impl SubscriberState {
    /// Where a replay that closes the whole gap starts: the first missed or
    /// pending event. `None` when there is nothing to hand over.
    pub fn gap_from(&self) -> Option<i64> {
        match (self.first_missed, self.first_pending) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// Where a replay starts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReplayFrom {
    /// This event ID on.
    Event(i64),
    /// The first event of a name the subscriber reads recorded at or after
    /// this instant on.
    Time(Timestamp),
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Replay {
    /// The first and last event looked at; `None` when none was found.
    pub from: Option<i64>,
    pub through: Option<i64>,
    /// Deliveries created by this replay (or that it would create): events
    /// the subscriber had no delivery for.
    pub created: usize,
    /// Events now pending for the subscriber, newly or still. Zero means the
    /// replay did nothing and wrote nothing.
    pub pending: usize,
    /// The first event not looked at, when `limit` cut the batch short.
    pub next: Option<i64>,
    /// Events the subscriber had no delivery for that a personal-data erasure
    /// changed (ADR 05): no delivery is created for them, since their payload
    /// no longer holds the facts the subscriber reads.
    pub erased: usize,
}
impl Replay {
    pub fn to_json(&self) -> Value {
        json!({"from": self.from, "through": self.through, "created": self.created, "pending": self.pending, "next": self.next, "erased": self.erased})
    }
}

const SUBSCRIBERS: &str = "SELECT d.subscriber,count(*) FILTER (WHERE d.delivered_at IS NULL),count(*) FILTER (WHERE d.delivered_at IS NULL AND d.last_error IS NOT NULL),min(e.id) FILTER (WHERE d.delivered_at IS NULL) FROM base_events_deliveries d JOIN base_events e ON e.id=d.event_id WHERE e.tenant_id=$1 GROUP BY d.subscriber";
const LAST_DELIVERED: &str = "SELECT DISTINCT ON (d.subscriber) d.subscriber,e.id,(extract(epoch FROM e.recorded_at)*1000000)::bigint,(extract(epoch FROM d.delivered_at)*1000000)::bigint FROM base_events_deliveries d JOIN base_events e ON e.id=d.event_id WHERE e.tenant_id=$1 AND d.delivered_at IS NOT NULL ORDER BY d.subscriber,e.id DESC";
const MISSED: &str = "SELECT count(*),min(e.id) FROM base_events e WHERE e.tenant_id=$1 AND e.name=ANY($2) AND e.erased_at IS NULL AND NOT EXISTS (SELECT 1 FROM base_events_deliveries d WHERE d.event_id=e.id AND d.subscriber=$3)";

fn instant(micros: i64) -> Result<Timestamp> {
    Timestamp::from_microsecond(micros).map_err(|_| Error::Internal)
}

impl Events {
    /// Deliveries in the scope's tenant from event `from_event` on, in event
    /// then subscriber order, at most `limit` (1–500). `pending_only` leaves
    /// out delivered rows. Needs [`VIEW`].
    pub async fn deliveries(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        from_event: i64,
        pending_only: bool,
        limit: i64,
    ) -> Result<Vec<DeliveryState>> {
        if from_event < 1 || !(1..=500).contains(&limit) {
            return Err(Error::Invalid);
        }
        authorize_key(client, scope, VIEW).await?;
        let rows = client.query(
            "SELECT e.id,e.name,e.version,e.subject,e.event_key,e.recorded_at::text,d.subscriber,d.attempts,d.last_error,d.failed_at::text,d.delivered_at::text FROM base_events e JOIN base_events_deliveries d ON d.event_id=e.id WHERE e.tenant_id=$1 AND e.id>=$2 AND (NOT $3 OR d.delivered_at IS NULL) ORDER BY e.id,d.subscriber LIMIT $4",
            &[&scope.tenant(), &from_event, &pending_only, &limit],
        ).await?;
        rows.iter()
            .map(|row| {
                let subscriber: String = row.get(6);
                Ok(DeliveryState {
                    event_id: row.get(0),
                    name: row.get(1),
                    version: u16::try_from(row.get::<_, i32>(2)).map_err(|_| Error::Invalid)?,
                    subject: row.get(3),
                    key: row.get(4),
                    recorded_at: row.get(5),
                    mounted: self.subscribers.contains_key(subscriber.as_str()),
                    subscriber,
                    attempts: row.get(7),
                    last_error: row.get(8),
                    failed_at: row.get(9),
                    delivered_at: row.get(10),
                })
            })
            .collect()
    }

    /// Every subscriber of the scope's tenant, sorted by key: whether it is
    /// mounted, the last event delivered to it, what is pending and what it
    /// missed. Needs [`VIEW`].
    pub async fn subscriber_states(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
    ) -> Result<Vec<SubscriberState>> {
        authorize_key(client, scope, VIEW).await?;
        let tenant = scope.tenant();
        let mut states: BTreeMap<String, SubscriberState> = self
            .subscribers
            .iter()
            .map(|(key, subscriber)| {
                let state = SubscriberState {
                    subscriber: (*key).to_owned(),
                    mounted: true,
                    events: subscriber.events.iter().map(|on| on.event).collect(),
                    last_delivered: None,
                    pending: 0,
                    failing: 0,
                    first_pending: None,
                    missed: 0,
                    first_missed: None,
                };
                ((*key).to_owned(), state)
            })
            .collect();
        for row in client.query(SUBSCRIBERS, &[&tenant]).await? {
            let key: String = row.get(0);
            let state = states
                .entry(key.clone())
                .or_insert_with(|| SubscriberState {
                    subscriber: key,
                    mounted: false,
                    events: Vec::new(),
                    last_delivered: None,
                    pending: 0,
                    failing: 0,
                    first_pending: None,
                    missed: 0,
                    first_missed: None,
                });
            state.pending = row.get(1);
            state.failing = row.get(2);
            state.first_pending = row.get(3);
        }
        for row in client.query(LAST_DELIVERED, &[&tenant]).await? {
            if let Some(state) = states.get_mut(row.get::<_, &str>(0)) {
                state.last_delivered = Some(Delivered {
                    event_id: row.get(1),
                    recorded_at: instant(row.get(2))?,
                    delivered_at: instant(row.get(3))?,
                });
            }
        }
        for state in states.values_mut().filter(|state| state.mounted) {
            let row = client
                .query_one(MISSED, &[&tenant, &state.events, &state.subscriber])
                .await?;
            state.missed = row.get(0);
            state.first_missed = row.get(1);
        }
        Ok(states.into_values().collect())
    }

    /// What [`Events::replay`] would do now, writing nothing: the same batch,
    /// how many deliveries it would create and how many would be pending.
    /// Needs [`REPLAY`].
    pub async fn replay_review(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        subscriber: &str,
        from: ReplayFrom,
        limit: i64,
    ) -> Result<Replay> {
        let names = self.replayable(subscriber, from, limit)?.1;
        authorize_key(client, scope, REPLAY).await?;
        Ok(
            batch(client, scope.tenant(), subscriber, &names, from, limit)
                .await?
                .0,
        )
    }

    /// Hands the scope tenant's events from `from` on (at most `limit`,
    /// 1–1,000) to mounted `subscriber`: events recorded while it was
    /// unmounted or before it was mounted, and deliveries whose job ran out
    /// of attempts. It creates a delivery only where the subscriber has none
    /// and never hands a delivered event over again, so a second replay finds
    /// nothing and writes nothing. One job per subject walks it in order.
    /// Needs [`REPLAY`], the tenant's step-up policy for
    /// [`STEP_UP_REPLAY`], and `domain-events` its grant in the tenant.
    pub async fn replay(
        &self,
        scope: &Scope,
        subscriber: &str,
        from: ReplayFrom,
        limit: i64,
    ) -> Result<Replay> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("events.replay"))
            .await?;
        let replay = self
            .replay_in(&mut tx, scope, subscriber, from, limit)
            .await?;
        if replay.pending == 0 {
            tx.rollback().await?;
        } else {
            tx.commit().await?;
        }
        Ok(replay)
    }

    /// [`Events::replay`] inside the caller's transaction (an agent plan's
    /// commit). The caller commits; when `pending` is zero nothing was written.
    pub async fn replay_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        subscriber: &str,
        from: ReplayFrom,
        limit: i64,
    ) -> Result<Replay> {
        let (key, names) = self.replayable(subscriber, from, limit)?;
        let tenant = scope.tenant();
        authorize_key(tx.sql(), scope, REPLAY).await?;
        let (review, ids) = batch(tx.sql(), tenant, key, &names, from, limit).await?;
        if review.pending == 0 {
            return Ok(review);
        }
        self.step_up
            .require(scope, STEP_UP_REPLAY, &format!("subscriber:{key}"))
            .await?;
        let (first, through) = (review.from, review.through);
        type Done = (usize, Vec<(i64, String, String)>, usize);
        let (created, pending, erased): Done = tx.mutate(move |sql| Box::pin(async move {
            // Share-locked so an erasure of these events waits, or has
            // committed and stamped them; an erased event gets no delivery.
            let live: Vec<i64> = sql.query(
                "SELECT id FROM base_events WHERE id=ANY($1) AND erased_at IS NULL ORDER BY id FOR SHARE",
                &[&ids],
            ).await?.iter().map(|row| row.get(0)).collect();
            let created = sql.execute(
                "INSERT INTO base_events_deliveries(event_id,subscriber) SELECT unnest($1::bigint[]),$2 ON CONFLICT DO NOTHING",
                &[&live, &key],
            ).await? as usize;
            let erased = sql.query_one(
                "SELECT count(*) FROM base_events e WHERE e.id=ANY($1) AND e.erased_at IS NOT NULL AND NOT EXISTS (SELECT 1 FROM base_events_deliveries d WHERE d.event_id=e.id AND d.subscriber=$2)",
                &[&ids, &key],
            ).await?.get::<_, i64>(0) as usize;
            let pending: Vec<(i64, String, String)> = sql.query(
                "SELECT e.id,e.publisher,e.subject FROM base_events e JOIN base_events_deliveries d ON d.event_id=e.id AND d.subscriber=$2 WHERE e.id=ANY($1) AND d.delivered_at IS NULL ORDER BY e.id",
                &[&ids, &key],
            ).await?.iter().map(|row| (row.get(0), row.get(1), row.get(2))).collect();
            let change = Change { kind: "base/events.replay".into(), id: first.unwrap_or(1), old: Value::Null,
                new: json!({"subscriber": key, "from": first, "through": through, "created": created, "pending": pending.len(), "erased": erased}) };
            Ok(((created, pending, erased), vec![change]))
        })).await?;
        // Delivering a subject's last pending event walks its earlier ones first.
        let mut last = BTreeMap::new();
        for (id, publisher, subject) in &pending {
            last.insert((publisher.as_str(), subject.as_str()), *id);
        }
        if !last.is_empty() {
            let system = SystemScope::rehydrate(tx.sql(), tenant, Some(PRINCIPAL)).await?;
            for id in last.values() {
                self.queue
                    .enqueue_system_within(tx, &system, WORKER, json!({"event_id": id}))
                    .await?;
            }
        }
        Ok(Replay {
            created,
            pending: pending.len(),
            erased,
            ..review
        })
    }

    /// The mounted subscriber's key and the names it reads, after the bounds.
    fn replayable(
        &self,
        subscriber: &str,
        from: ReplayFrom,
        limit: i64,
    ) -> Result<(&'static str, Vec<&'static str>)> {
        if matches!(from, ReplayFrom::Event(id) if id < 1) || !(1..=1000).contains(&limit) {
            return Err(Error::Invalid);
        }
        let (&key, definition) = self
            .subscribers
            .get_key_value(subscriber)
            .ok_or(Error::NotFound)?;
        Ok((key, definition.events.iter().map(|on| on.event).collect()))
    }
}

/// The batch a replay looks at, and what it holds for the subscriber, read
/// without writing.
async fn batch(
    client: &impl GenericClient,
    tenant: i64,
    key: &str,
    names: &[&str],
    from: ReplayFrom,
    limit: i64,
) -> Result<(Replay, Vec<i64>)> {
    let start = match from {
        ReplayFrom::Event(id) => Some(id),
        ReplayFrom::Time(at) => client
            .query_one(
                "SELECT min(id) FROM base_events WHERE tenant_id=$1 AND name=ANY($2) AND recorded_at>=$3::text::timestamptz",
                &[&tenant, &names, &at.to_string()],
            )
            .await?
            .get(0),
    };
    let Some(start) = start else {
        return Ok((Replay::default(), Vec::new()));
    };
    let mut ids: Vec<i64> = client
        .query(
            "SELECT id FROM base_events WHERE tenant_id=$1 AND name=ANY($2) AND id>=$3 ORDER BY id LIMIT $4",
            &[&tenant, &names, &start, &(limit + 1)],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    let next = (ids.len() as i64 > limit).then(|| ids.pop()).flatten();
    let row = client.query_one(
        "SELECT count(*) FILTER (WHERE d.event_id IS NULL AND e.erased_at IS NULL),count(*) FILTER (WHERE d.delivered_at IS NULL AND (d.event_id IS NOT NULL OR e.erased_at IS NULL)),count(*) FILTER (WHERE d.event_id IS NULL AND e.erased_at IS NOT NULL) \
         FROM unnest($1::bigint[]) AS i(id) JOIN base_events e ON e.id=i.id LEFT JOIN base_events_deliveries d ON d.event_id=i.id AND d.subscriber=$2",
        &[&ids, &key],
    ).await?;
    let replay = Replay {
        from: ids.first().copied(),
        through: ids.last().copied(),
        created: row.get::<_, i64>(0) as usize,
        pending: row.get::<_, i64>(1) as usize,
        next,
        erased: row.get::<_, i64>(2) as usize,
    };
    Ok((replay, ids))
}
