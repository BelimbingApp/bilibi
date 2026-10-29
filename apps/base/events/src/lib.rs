//! Events: how a module reacts to a fact another Domain or Extension's module
//! recorded, with neither naming the other's crate. Supply Chain records
//! `supply_chain/receiving.goods_received` in its own transaction; Finance
//! Payables subscribes by that name and keeps its own copy of what it needs.
//!
//! The publisher declares each event type it records (`EVENTS`) and records
//! it inside its `MutationTransaction`: the event row, one pending delivery
//! per mounted subscriber, the audit row and a delivery job commit or roll
//! back with the owner's writes. Delivery runs on `base/queue` as the
//! `domain-events` system principal, at least once, in recording order per
//! subject, and each subscriber's effects commit with its receipt and audit
//! in one transaction. Nothing is broadcast in process: the outbox rows are
//! the source of truth.
mod agent;
mod catalog;
mod delivery;
mod operator;
mod privacy;
pub use privacy::{PersonalEvent, PERSONAL_EVENTS};
mod web;
pub use operator::{Delivered, DeliveryState, Replay, ReplayFrom, SubscriberState};

use bilibi_authz::{
    CapabilityDefinition, SystemPrincipalDefinition, SystemScope, VocabularyContribution,
    VOCABULARY,
};
use bilibi_database::{Change, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Catalog, Contribution, Layer, Module, Provenance, RegistryError, Result as RegistryResult,
    Service,
};
use bilibi_queue::{
    execution::{HandlerBinding, HandlerService, HANDLERS},
    Queue, Worker, WORKERS,
};
use serde_json::json;
pub use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};
use tokio_postgres::Transaction;

/// The catalog publishers declare event types in:
/// `Contribution::new(EVENTS, "<module>.<snake_case name>", provenance, &EventType { version: 1 })`.
pub const EVENTS: &str = "base/events.events";
/// The catalog subscribers contribute to:
/// `Contribution::new(SUBSCRIBERS, "<module>.<snake_case name>", provenance, &Subscriber { .. })`.
pub const SUBSCRIBERS: &str = "base/events.subscribers";
/// The `Events` service publishers and operators resolve, after declaring `base/events`.
pub const SERVICE: &str = "base/events.outbox";
/// The system principal deliveries run as. Grant it [`DELIVER`] in each tenant
/// (`Grants::set_system`) before events can be delivered there.
pub const PRINCIPAL: &str = "domain-events";
pub const DELIVER: &str = "events.delivery.deliver";
/// Read delivery state in the operator's tenant.
pub const VIEW: &str = "admin.events.view";
/// Replay events to a subscriber in the operator's tenant.
pub const REPLAY: &str = "admin.events.replay";
/// The Base Step-up operation a replay asks for; off unless the tenant turns
/// it on, since a replay hands over only recorded facts and never twice.
pub const STEP_UP_REPLAY: &str = "base.events.replay";
const WORKER: &str = "base/events-delivery";
const DELIVERY: &str = "base/events.delivery";

/// A publisher's declaration of one event type it records. The contribution ID
/// is the event name, in the publisher's own namespace. Raise `version` when
/// the payload shape changes incompatibly; subscribers list the versions they
/// read, and boot refuses one that does not read the mounted publisher's.
pub struct EventType {
    pub version: u16,
}

/// One event name a subscriber reads, and the versions of it it understands.
pub struct On {
    pub event: &'static str,
    pub versions: &'static [u16],
}
/// Contribution payload. The contribution ID is the subscriber's key, in its
/// own module's namespace; receipts are kept under it, so never rename it.
pub struct Subscriber {
    pub events: &'static [On],
    pub handler: &'static dyn Subscribe,
}
pub type SubscriberFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<Change>>> + Send + 'a>>;
/// Implement on the subscriber's own type. Write only in the supplied
/// transaction and return every mutation; the receipt commits with them.
/// Never open or commit a second transaction and never do network or file IO
/// here: record that work in the subscriber's own outbox, keyed by
/// [`Event::key`]. An error rolls the effects back and the delivery is
/// retried; later events of the same subject wait for it.
pub trait Subscribe: Send + Sync {
    fn handle<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a SystemScope,
        event: &'a Event,
    ) -> SubscriberFuture<'a>;
}

/// A recorded event as a subscriber receives it. The publisher's actor is not
/// carried: the event is a fact, not authority. `scope` in [`Subscribe`] is the
/// `domain-events` principal in `tenant`.
#[derive(Clone, Debug)]
pub struct Event {
    pub id: i64,
    pub tenant: i64,
    pub name: String,
    pub version: u16,
    pub subject: String,
    /// The publisher's idempotency key; use it as the subscriber's own key.
    pub key: String,
    pub payload: Value,
    pub recorded_at: String,
    /// This subscriber's attempt at this event, from 1.
    pub attempt: i32,
}
/// What a publisher records. `subject` is the record the event is about
/// (`goods_receipt:42`); delivery keeps one subject's events in recording
/// order, so record them under the owner's lock on that record. `key` is
/// unique per publisher and tenant: recording it again answers the event
/// already recorded, and a different event under the same key is `Conflict`.
pub struct NewEvent<'a> {
    pub name: &'a str,
    pub subject: &'a str,
    pub key: &'a str,
    pub payload: Value,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Recorded {
    /// Recorded in this transaction; provisional until it commits.
    New(i64),
    /// Recorded earlier under the same key; nothing was written.
    Existing(i64),
}
impl Recorded {
    pub fn id(self) -> i64 {
        match self {
            Self::New(id) | Self::Existing(id) => id,
        }
    }
}

/// The mounted event types and subscribers, built once from the catalog.
pub struct Events {
    db: Db,
    boundary: Arc<MutationBoundary>,
    queue: Arc<Queue>,
    step_up: Arc<bilibi_step_up::StepUp>,
    /// Event name → (publisher module, current version).
    types: BTreeMap<&'static str, (&'static str, u16)>,
    subscribers: BTreeMap<&'static str, &'static Subscriber>,
}
impl Events {
    /// Records `event` in the owner's transaction. The name must be an event
    /// type a mounted module declares; the audit row carries no payload.
    pub async fn record(
        &self,
        tx: &mut MutationTransaction<'_>,
        event: NewEvent<'_>,
    ) -> Result<Recorded> {
        let (&name, &(publisher, version)) =
            self.types.get_key_value(event.name).ok_or(Error::Invalid)?;
        if !text(event.subject) || !text(event.key) {
            return Err(Error::Invalid);
        }
        bilibi_queue::arguments::validate(&event.payload)?;
        let tenant = tx.context().tenant;
        if let Some(row) = tx.sql().query_opt(
            "SELECT id,name,subject,payload FROM base_events WHERE tenant_id=$1 AND publisher=$2 AND event_key=$3",
            &[&tenant, &publisher, &event.key],
        ).await? {
            let same = row.get::<_, &str>(1) == event.name
                && row.get::<_, &str>(2) == event.subject
                && row.get::<_, Value>(3) == event.payload;
            return if same { Ok(Recorded::Existing(row.get(0))) } else { Err(Error::Conflict) };
        }
        let subscribers: Vec<&'static str> = self.subscribers_of(name).collect();
        let (subject, key, payload) = (
            event.subject.to_owned(),
            event.key.to_owned(),
            event.payload,
        );
        let trace = tx.context().trace_id.clone();
        let listed = subscribers.clone();
        let id = tx.mutate(move |sql| Box::pin(async move {
            let id: i64 = sql.query_one(
                "INSERT INTO base_events(tenant_id,publisher,name,version,subject,event_key,payload,trace_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8) RETURNING id",
                &[&tenant, &publisher, &name, &i32::from(version), &subject, &key, &payload, &trace],
            ).await?.get(0);
            sql.execute(
                "INSERT INTO base_events_deliveries(event_id,subscriber) SELECT $1,unnest($2::text[])",
                &[&id, &listed],
            ).await?;
            let change = Change { kind: "base/events.event".into(), id, old: Value::Null,
                new: json!({"name": name, "version": version, "subject": subject, "key": key, "subscribers": listed}) };
            Ok((id, vec![change]))
        })).await?;
        if !subscribers.is_empty() {
            let enqueued = async {
                let scope = SystemScope::rehydrate(tx.sql(), tenant, Some(PRINCIPAL)).await?;
                self.queue
                    .enqueue_system_within(tx, &scope, WORKER, json!({"event_id": id}))
                    .await
            }
            .await;
            if let Err(error) = enqueued {
                // A refusal before the queue insert must still poison the
                // owner's transaction, so the event never commits undelivered.
                return tx
                    .mutate(move |_| Box::pin(async move { Err(error) }))
                    .await;
            }
        }
        Ok(Recorded::New(id))
    }
    /// Every mounted event type name, sorted.
    pub fn event_types(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.types.keys().copied()
    }
    /// Every mounted subscriber key, sorted.
    pub fn subscribers(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.subscribers.keys().copied()
    }
    fn subscribers_of<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'static str> + 'a {
        self.subscribers
            .iter()
            .filter(move |(_, s)| s.events.iter().any(|on| on.event == name))
            .map(|(key, _)| *key)
    }
}
/// A subject or key: 1–200 bytes without control characters.
fn text(value: &str) -> bool {
    !value.is_empty() && value.len() <= 200 && !value.chars().any(char::is_control)
}

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/events",
            source: file!(),
            line: line!(),
        }
    };
}
fn delivery_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(o)
            if o.len() == 1
                && o.get("event_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0) =>
        {
            Ok(args)
        }
        _ => Err(Error::Invalid),
    }
}
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 100, validate: delivery_arguments }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: DELIVERY, capability: DELIVER }) }
inventory::submit! { Contribution::new(VOCABULARY, "base/events.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["events"],
    verbs: &["deliver", "replay"],
    capabilities: &[CapabilityDefinition::tenant(DELIVER), CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(REPLAY)],
    roles: &[],
    system_principals: &[SystemPrincipalDefinition { name: PRINCIPAL, capabilities: &[DELIVER] }],
}) }

fn build(services: &bilibi_module_registry::Services) -> RegistryResult<Events> {
    let modules = bilibi_module_registry::discover()?;
    let catalog = Catalog::discover(&modules)?;
    let (types, subscribers) = catalog::routes(&catalog, &modules)?;
    Ok(Events {
        db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
        boundary: services.get("base/database.mutations")?,
        queue: services.get("base/queue.store")?,
        step_up: services.get(bilibi_step_up::SERVICE)?,
        types,
        subscribers,
    })
}
bilibi_step_up::declare!(
    "base/events",
    bilibi_step_up::Operation::new(STEP_UP_REPLAY, "Replay events to a subscriber")
        .off_by_default()
);
inventory::submit! { Contribution::new(ROUTES, "base/events.index", provenance!(), &Route {
    method: Method::Get, path: web::PATH, service: PAGES, operation: "index",
    access: Access::Capability(VIEW), framed: true,
}) }
bilibi_menu::contribute!(
    "base/events",
    bilibi_menu::MenuItem::page("admin.system.events", "Events", web::PATH)
        .icon("arrow-path")
        .under("admin.system")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(20)
);
pub(crate) const PAGES: &str = "base/events.pages";
// The page's replay is the `base.events.replay` contract (ADR 01).
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, PAGES, provenance!(), &bilibi_module_registry::contributions::PageWrites {
    service: PAGES,
    contracts: &[("replay", "base.events.replay")],
    pending: bilibi_module_registry::contributions::Pending::None,
}) }

inventory::submit! {
    Module {
        id: "base/events", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/audit", "base/tenancy", "base/authz", "base/queue", "base/step_up", "base/agent_api", "base/ui", "base/menu", "base/datetime", "base/locale", "base/personal_data"],
        migrations: &[("base-events-001", include_str!("../migrations/001-events.sql"))],
        services: &[
            Service { id: SERVICE, factory: |services| Ok(Arc::new(build(services)?)) },
            Service { id: DELIVERY, factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(delivery::Delivery { events: services.get(SERVICE)? }))))
            } },
            Service { id: "base/events.agent", factory: |services| {
                if !agent::bind(services.get(SERVICE)?) {
                    return Err(RegistryError::Service("duplicate events agent binding".into()));
                }
                Ok(Arc::new(()))
            } },
            Service { id: PAGES, factory: |services| {
                Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, Error>(Arc::new(web::Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    events: services.get(SERVICE)?,
                    contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
        ],
    }
}
