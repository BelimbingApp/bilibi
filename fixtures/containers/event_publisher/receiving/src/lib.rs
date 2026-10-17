//! Reference event publisher, the shape Supply Chain Receiving takes when
//! Finance Payables must learn about received goods. It owns one table, one
//! capability and a queue worker that receives or returns goods; each command
//! records `event_publisher/receiving.goods_received` or `.goods_returned`
//! in the same transaction. It names no subscriber. Mount it only to prove
//! `base/events` (`apps/web/examples/events-reference.rs`).
use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_events::{EventType, Events, NewEvent, Recorded, EVENTS};
use bilibi_module_registry::{Contribution, Layer, Module, Provenance, Service};
use bilibi_queue::{
    execution::{
        Execution, Handler, HandlerBinding, HandlerFuture, HandlerService, Outcome, HANDLERS,
    },
    Worker, WORKERS,
};
use serde_json::{json, Value};
use std::sync::Arc;

const MODULE: &str = "event_publisher/receiving";
pub const RECEIVE: &str = "event-publisher.receiving.receive";
pub const RECEIVED: &str = "event_publisher/receiving.goods_received";
pub const RETURNED: &str = "event_publisher/receiving.goods_returned";
const WORKER: &str = "event_publisher/receiving-command";

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! {
    Module {
        id: MODULE, layer: Layer::Domains,
        dependencies: &["base/authz", "base/database", "base/events", "base/queue"],
        migrations: &[("event-publisher-receiving-001-receipts", include_str!("../migrations/001-receipts.sql"))],
        services: &[Service { id: "event_publisher/receiving.commands", factory: |services| {
            Ok(Arc::new(HandlerService(Arc::new(Commands {
                events: services.get(bilibi_events::SERVICE)?,
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            }))))
        } }],
    }
}
inventory::submit! { Contribution::new(VOCABULARY, "event_publisher/receiving.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["event-publisher"], verbs: &["receive"],
    capabilities: &[CapabilityDefinition::tenant(RECEIVE)], roles: &[], system_principals: &[],
}) }
inventory::submit! { Contribution::new(EVENTS, RECEIVED, provenance!(), &EventType { version: 1 }) }
inventory::submit! { Contribution::new(EVENTS, RETURNED, provenance!(), &EventType { version: 1 }) }
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 3, validate: command }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "event_publisher/receiving.commands", capability: RECEIVE }) }

/// `{"action": "receive", "receipt": "GRN-1", "supplier": "ACME", "quantity": "12.5"}`
/// or `{"action": "return", "receipt": "GRN-1"}`.
fn command(args: Value) -> Result<Value> {
    let o = args.as_object().ok_or(Error::Invalid)?;
    let text = |key: &str| {
        o.get(key)
            .and_then(Value::as_str)
            .is_some_and(|v| !v.is_empty() && v.len() <= 64)
    };
    let ok = match o.get("action").and_then(Value::as_str) {
        Some("receive") => o.len() == 4 && text("receipt") && text("supplier") && text("quantity"),
        Some("return") => o.len() == 2 && text("receipt"),
        _ => false,
    };
    if ok {
        Ok(args)
    } else {
        Err(Error::Invalid)
    }
}

/// The facade command: receive goods and record the event in the same
/// transaction. Receiving the same receipt again changes nothing and answers
/// the event already recorded.
pub async fn receive(
    tx: &mut MutationTransaction<'_>,
    events: &Events,
    receipt: &str,
    supplier: &str,
    quantity: &str,
) -> Result<Recorded> {
    let tenant = tx.context().tenant;
    let existing = tx
        .sql()
        .query_opt(
            "SELECT id FROM event_publisher_receipts WHERE tenant_id=$1 AND receipt_number=$2",
            &[&tenant, &receipt],
        )
        .await?;
    let id: i64 = match existing {
        Some(row) => row.get(0),
        None => {
            let (receipt, supplier, quantity) =
                (receipt.to_owned(), supplier.to_owned(), quantity.to_owned());
            tx.mutate(move |sql| Box::pin(async move {
                let id: i64 = sql.query_one(
                    "INSERT INTO event_publisher_receipts(tenant_id,receipt_number,supplier,quantity) VALUES($1,$2,$3,$4::text::numeric) RETURNING id",
                    &[&tenant, &receipt, &supplier, &quantity],
                ).await.map_err(|_| Error::Invalid)?.get(0);
                Ok((id, vec![Change { kind: "event_publisher/receiving.receipt".into(), id, old: Value::Null, new: json!({"receipt": receipt, "supplier": supplier, "quantity": quantity}) }]))
            })).await?
        }
    };
    let quantity: String = tx
        .sql()
        .query_one(
            "SELECT quantity::text FROM event_publisher_receipts WHERE id=$1",
            &[&id],
        )
        .await?
        .get(0);
    let supplier: String = tx
        .sql()
        .query_one(
            "SELECT supplier FROM event_publisher_receipts WHERE id=$1",
            &[&id],
        )
        .await?
        .get(0);
    events
        .record(
            tx,
            NewEvent {
                name: RECEIVED,
                subject: &format!("goods_receipt:{id}"),
                key: &format!("received:{receipt}"),
                payload: json!({"receipt": receipt, "supplier": supplier, "quantity": quantity}),
            },
        )
        .await
}

/// Returns received goods; the event's subject is the receipt, so a
/// subscriber always sees the return after the receipt.
pub async fn return_goods(
    tx: &mut MutationTransaction<'_>,
    events: &Events,
    receipt: &str,
) -> Result<Recorded> {
    let tenant = tx.context().tenant;
    let row = tx.sql().query_opt("SELECT id,returned FROM event_publisher_receipts WHERE tenant_id=$1 AND receipt_number=$2 FOR UPDATE", &[&tenant, &receipt]).await?.ok_or(Error::NotFound)?;
    let (id, returned): (i64, bool) = (row.get(0), row.get(1));
    if !returned {
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE event_publisher_receipts SET returned=true WHERE id=$1",
                    &[&id],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: "event_publisher/receiving.receipt".into(),
                        id,
                        old: json!({"returned": false}),
                        new: json!({"returned": true}),
                    }],
                ))
            })
        })
        .await?;
    }
    events
        .record(
            tx,
            NewEvent {
                name: RETURNED,
                subject: &format!("goods_receipt:{id}"),
                key: &format!("returned:{receipt}"),
                payload: json!({"receipt": receipt}),
            },
        )
        .await
}

struct Commands {
    events: Arc<Events>,
    db: Db,
}
impl Handler for Commands {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let result = async {
                let mut client = self.db.get().await?;
                let mut tx = execution
                    .begin(&mut client, "event_publisher.receiving.command")
                    .await?;
                let field = |key: &str| args.get(key).and_then(Value::as_str).unwrap_or_default();
                let recorded = match field("action") {
                    "receive" => {
                        receive(
                            &mut tx,
                            &self.events,
                            field("receipt"),
                            field("supplier"),
                            field("quantity"),
                        )
                        .await?
                    }
                    _ => return_goods(&mut tx, &self.events, field("receipt")).await?,
                };
                tx.commit().await?;
                Ok::<_, Error>(recorded)
            }
            .await;
            match result {
                Ok(_) => Outcome::Completed,
                Err(Error::Invalid | Error::NotFound | Error::Conflict) => {
                    Outcome::Cancel("receiving_command_refused")
                }
                Err(_) => Outcome::Retry("receiving_unavailable"),
            }
        })
    }
}
