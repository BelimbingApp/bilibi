//! Reference event subscriber, the shape Finance Payables takes when it keeps
//! its own copy of received goods for matching. It subscribes to the
//! reference publisher's events by name only: no Cargo dependency, no
//! descriptor edge, no SQL on the publisher's tables. It composes and boots
//! with the publisher unmounted, and then simply receives nothing.
//!
//! A receipt from a supplier Payables has not onboarded is refused (the
//! delivery retries until the supplier exists), and a return is applied to
//! the copy the receipt made. Mount it only to prove `base/events`
//! (`apps/web/examples/events-reference.rs`).
use bilibi_authz::SystemScope;
use bilibi_database::{Change, Error};
use bilibi_events::{Event, On, Subscribe, Subscriber, SubscriberFuture, SUBSCRIBERS};
use bilibi_module_registry::{Contribution, Layer, Module, Provenance};
use serde_json::{json, Value};
use tokio_postgres::Transaction;

const MODULE: &str = "event_subscriber/payables";
// The publisher's documented event names and payloads; its crate is never named.
const RECEIVED: &str = "event_publisher/receiving.goods_received";
const RETURNED: &str = "event_publisher/receiving.goods_returned";

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &["base/authz", "base/database", "base/events"],
        migrations: &[("event-subscriber-payables-001-receipts", include_str!("../migrations/001-receipts.sql"))],
        services: &[],
    }
}
inventory::submit! { Contribution::new(SUBSCRIBERS, "event_subscriber/payables.receipts", Provenance { module: MODULE, source: file!(), line: line!() }, &Subscriber {
    events: &[On { event: RECEIVED, versions: &[1] }, On { event: RETURNED, versions: &[1] }],
    handler: &Receipts,
}) }

struct Receipts;
impl Subscribe for Receipts {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a SystemScope,
        event: &'a Event,
    ) -> SubscriberFuture<'a> {
        Box::pin(async move {
            let field = |key: &str| {
                event
                    .payload
                    .get(key)
                    .and_then(Value::as_str)
                    .ok_or(Error::Invalid)
            };
            let tenant = scope.tenant();
            let receipt = field("receipt")?;
            if event.name == RECEIVED {
                let supplier = field("supplier")?;
                if tx
                    .query_opt(
                        "SELECT 1 FROM event_subscriber_suppliers WHERE tenant_id=$1 AND code=$2",
                        &[&tenant, &supplier],
                    )
                    .await?
                    .is_none()
                {
                    return Err(Error::Conflict);
                }
                // The event key makes a redelivered receipt a no-op.
                let copied = tx.execute(
                    "INSERT INTO event_subscriber_receipts(tenant_id,receipt_number,supplier,quantity,received_event) VALUES($1,$2,$3,$4::text::numeric,$5) ON CONFLICT DO NOTHING",
                    &[&tenant, &receipt, &supplier, &field("quantity")?, &event.key],
                ).await?;
                return Ok(change(
                    event,
                    copied,
                    json!({"receipt": receipt, "supplier": supplier}),
                ));
            }
            let updated = tx.execute(
                "UPDATE event_subscriber_receipts SET returned_event=$3 WHERE tenant_id=$1 AND receipt_number=$2 AND returned_event IS NULL",
                &[&tenant, &receipt, &event.key],
            ).await?;
            if updated == 0 && tx.query_opt("SELECT 1 FROM event_subscriber_receipts WHERE tenant_id=$1 AND receipt_number=$2", &[&tenant, &receipt]).await?.is_none() {
                return Err(Error::NotFound);
            }
            Ok(change(
                event,
                updated,
                json!({"receipt": receipt, "returned": true}),
            ))
        })
    }
}
fn change(event: &Event, rows: u64, new: Value) -> Vec<Change> {
    if rows == 0 {
        return Vec::new();
    }
    vec![Change {
        kind: "event_subscriber/payables.receipt".into(),
        id: event.id,
        old: Value::Null,
        new,
    }]
}
