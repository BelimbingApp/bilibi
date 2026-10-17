//! Operator reads and replay, in the operator's own tenant.
use crate::{Events, PRINCIPAL, REPLAY, VIEW, WORKER};
use bilibi_authz::{authorize_key, SystemScope};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Replay {
    /// Events now pending for the subscriber, newly or still.
    pub pending: usize,
    /// The first event not looked at, when `limit` cut the batch short.
    pub next: Option<i64>,
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

    /// Hands the scope tenant's events from `from_event` on (at most `limit`,
    /// 1–1,000) to mounted `subscriber`: a subscriber mounted after they were
    /// recorded, or one whose delivery job ran out of attempts. Delivered
    /// events are not delivered again. One job per subject walks it in order.
    /// Needs [`REPLAY`], and `domain-events` its grant in the tenant.
    pub async fn replay(
        &self,
        scope: &Scope,
        subscriber: &str,
        from_event: i64,
        limit: i64,
    ) -> Result<Replay> {
        if from_event < 1 || !(1..=1000).contains(&limit) {
            return Err(Error::Invalid);
        }
        let (&key, definition) = self
            .subscribers
            .get_key_value(subscriber)
            .ok_or(Error::NotFound)?;
        let names: Vec<&str> = definition.events.iter().map(|on| on.event).collect();
        let tenant = scope.tenant();
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, scope.mutation_context("events.replay"))
            .await?;
        authorize_key(tx.sql(), scope, REPLAY).await?;
        let mut ids: Vec<i64> = tx.sql().query(
            "SELECT id FROM base_events WHERE tenant_id=$1 AND name=ANY($2) AND id>=$3 ORDER BY id LIMIT $4",
            &[&tenant, &names, &from_event, &(limit + 1)],
        ).await?.iter().map(|row| row.get(0)).collect();
        let next = (ids.len() as i64 > limit).then(|| ids.pop()).flatten();
        let Some(&first) = ids.first() else {
            tx.rollback().await?;
            return Ok(Replay { pending: 0, next });
        };
        let through = *ids.last().unwrap_or(&first);
        let pending: Vec<(i64, String, String)> = tx.mutate(move |sql| Box::pin(async move {
            sql.execute(
                "INSERT INTO base_events_deliveries(event_id,subscriber) SELECT unnest($1::bigint[]),$2 ON CONFLICT DO NOTHING",
                &[&ids, &key],
            ).await?;
            let pending: Vec<(i64, String, String)> = sql.query(
                "SELECT e.id,e.publisher,e.subject FROM base_events e JOIN base_events_deliveries d ON d.event_id=e.id AND d.subscriber=$2 WHERE e.id=ANY($1) AND d.delivered_at IS NULL ORDER BY e.id",
                &[&ids, &key],
            ).await?.iter().map(|row| (row.get(0), row.get(1), row.get(2))).collect();
            let change = Change { kind: "base/events.replay".into(), id: first, old: Value::Null,
                new: json!({"subscriber": key, "from": first, "through": through, "pending": pending.len()}) };
            Ok((pending, vec![change]))
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
                    .enqueue_system_within(&mut tx, &system, WORKER, json!({"event_id": id}))
                    .await?;
            }
        }
        tx.commit().await?;
        Ok(Replay {
            pending: pending.len(),
            next,
        })
    }
}
