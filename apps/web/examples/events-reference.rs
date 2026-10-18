//! Running specimen of domain events (`base/events`), separate from product
//! code. The reference publisher (`fixtures/containers/event_publisher`) and
//! subscriber (`fixtures/containers/event_subscriber`) are Domain containers
//! that are never mounted in the Platform and never name each other. On a
//! fresh disposable DATABASE_URL, run the four compositions in this order:
//!
//! ```text
//! cargo run -p bilibi-web --example events-reference                 # 1. nothing mounted
//! cp -r fixtures/containers/event_publisher apps/domains/ && python3 scripts/discover-modules.py
//! cargo run -p bilibi-web --example events-reference                 # 2. publisher only
//! cp -r fixtures/containers/event_subscriber apps/domains/ && python3 scripts/discover-modules.py
//! cargo run -p bilibi-web --example events-reference                 # 3. both
//! rm -r apps/domains/event_publisher && python3 scripts/discover-modules.py
//! cargo run -p bilibi-web --example events-reference                 # 4. subscriber only
//! rm -r apps/domains/event_subscriber && python3 scripts/discover-modules.py && git checkout -- Cargo.lock
//! ```
//!
//! Every composition boots, and every one prints the boot refusals of a
//! subscriber in its publisher's own container and of a malformed event name.
//! Unmounted, the publisher's command is unknown. With the publisher alone,
//! a receipt records an event with no deliveries. With both, the subscriber
//! is new: the earlier receipt is not delivered to it; a receipt from a
//! supplier it has not onboarded is refused and retried while the return of
//! that receipt waits behind it; once the supplier exists both are delivered
//! in order; receiving a receipt again records nothing; a replay from the
//! first event delivers the earlier receipt and a second replay finds nothing
//! pending. With the subscriber alone, nothing is recorded or delivered.
//!
//! The example drives the publisher through its queue worker ID and reads
//! the subscriber's copy table to show its effects; it names neither crate.
use bilibi_authz::{grants::Grants, SystemScope};
use bilibi_base::events::{
    Event, Events, On, Subscribe, Subscriber, SubscriberFuture, DELIVER, PRINCIPAL, SERVICE,
    SUBSCRIBERS,
};
use bilibi_base::queue::{
    execution::{Config, Executor},
    Queue,
};
use bilibi_database::{pool, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{
    verify_composition, Catalog, Consumer, Contribution, Layer, Module, Provenance, Services,
};
use bilibi_tenancy::{Authentication, Scope};
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio_postgres::Transaction;
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const RECEIVED: &str = "event_publisher/receiving.goods_received";
const COMMAND: &str = "event_publisher/receiving-command";
const SUBSCRIBER: &str = "event_subscriber/payables.receipts";

/// Never registered: the payload of the two boot refusals below.
struct Nothing;
impl Subscribe for Nothing {
    fn handle<'a>(
        &'a self,
        _: &'a Transaction<'_>,
        _: &'a SystemScope,
        _: &'a Event,
    ) -> SubscriberFuture<'a> {
        Box::pin(async { Ok(Vec::new()) })
    }
}
static SAME_CONTAINER: Module = Module {
    id: "event_publisher/audit_trail",
    layer: Layer::Domains,
    dependencies: &["base/events"],
    migrations: &[],
    services: &[],
};
static SAME_CONTAINER_SUBSCRIBER: Contribution = Contribution::new(
    SUBSCRIBERS,
    "event_publisher/audit_trail.receipts",
    Provenance {
        module: "event_publisher/audit_trail",
        source: file!(),
        line: line!(),
    },
    &Subscriber {
        events: &[On {
            event: RECEIVED,
            versions: &[1],
        }],
        handler: &Nothing,
    },
);
static MALFORMED: Module = Module {
    id: "event_subscriber/typo",
    layer: Layer::Domains,
    dependencies: &["base/events"],
    migrations: &[],
    services: &[],
};
static MALFORMED_SUBSCRIBER: Contribution = Contribution::new(
    SUBSCRIBERS,
    "event_subscriber/typo.receipts",
    Provenance {
        module: "event_subscriber/typo",
        source: file!(),
        line: line!(),
    },
    &Subscriber {
        events: &[On {
            event: "goods_received",
            versions: &[1],
        }],
        handler: &Nothing,
    },
);

/// Boot builds the catalog from these contributions; show what it refuses.
fn boot_refusals(modules: &[&'static Module]) {
    for (module, contribution) in [
        (&SAME_CONTAINER, &SAME_CONTAINER_SUBSCRIBER),
        (&MALFORMED, &MALFORMED_SUBSCRIBER),
    ] {
        let mut with = modules.to_vec();
        with.push(module);
        let built = Catalog::build(
            &with,
            inventory::iter::<Consumer>.into_iter().collect(),
            inventory::iter::<Contribution>
                .into_iter()
                .chain([contribution])
                .collect(),
        );
        match built {
            Ok(_) => println!("boot accepted {} (unexpected)", contribution.id),
            Err(error) => println!("boot refuses: {error}"),
        }
    }
}

struct Proof {
    db: Db,
    boundary: Arc<MutationBoundary>,
    queue: Arc<Queue>,
    events: Arc<Events>,
    admin: Scope,
}
impl Proof {
    /// Enqueues one publisher command as the admin, as a page or import would.
    async fn command(&self, args: Value) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(&mut client, self.admin.mutation_context("events.reference"))
            .await?;
        self.queue
            .enqueue_for(&mut tx, &self.admin, COMMAND, args)
            .await?;
        tx.commit().await
    }
    async fn receive(&self, receipt: &str, supplier: &str, quantity: &str) -> Result<()> {
        println!("receive {receipt} from {supplier}");
        self.command(json!({"action": "receive", "receipt": receipt, "supplier": supplier, "quantity": quantity}))
            .await
    }
    async fn give_back(&self, receipt: &str) -> Result<()> {
        println!("return {receipt}");
        self.command(json!({"action": "return", "receipt": receipt}))
            .await
    }
    /// Waits for the publisher's commands to finish.
    async fn commands_done(&self) -> Result<()> {
        self.until("publisher commands", |db| async move {
            let open: i64 = db.get().await?.query_one(
                "SELECT count(*) FROM base_queue_jobs WHERE worker_id=$1 AND state NOT IN ('completed','cancelled','discarded')",
                &[&COMMAND],
            ).await?.get(0);
            Ok(open == 0)
        }).await
    }
    async fn until<F, Fut>(&self, what: &str, done: F) -> Result<()>
    where
        F: Fn(Db) -> Fut,
        Fut: std::future::Future<Output = Result<bool>>,
    {
        for _ in 0..600 {
            if done(self.db.clone()).await? {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        println!("timed out waiting for {what}");
        Err(Error::Internal)
    }
    async fn state(&self, heading: &str) -> Result<()> {
        let client = self.db.get().await?;
        let recorded: i64 = client
            .query_one("SELECT count(*) FROM base_events WHERE tenant_id=1", &[])
            .await?
            .get(0);
        println!("-- {heading}: {recorded} event(s) recorded in tenant 1");
        for d in self
            .events
            .deliveries(&**client, &self.admin, 1, false, 500)
            .await?
        {
            println!(
                "   event {} {} v{} {} -> {}{}: attempts {}, {}",
                d.event_id,
                d.name,
                d.version,
                d.subject,
                d.subscriber,
                if d.mounted { "" } else { " (unmounted)" },
                d.attempts,
                match (&d.delivered_at, &d.last_error) {
                    (Some(_), _) => "delivered".to_owned(),
                    (None, Some(error)) => format!("pending, last failure {error}"),
                    (None, None) => "pending".to_owned(),
                }
            );
        }
        Ok(())
    }
    async fn copy(&self) -> Result<()> {
        let client = self.db.get().await?;
        for row in client.query("SELECT receipt_number,supplier,quantity::text,returned_event IS NOT NULL FROM event_subscriber_receipts WHERE tenant_id=1 ORDER BY receipt_number", &[]).await? {
            println!(
                "   subscriber's copy: {} from {} quantity {} returned {}",
                row.get::<_, String>(0),
                row.get::<_, String>(1),
                row.get::<_, String>(2),
                row.get::<_, bool>(3)
            );
        }
        Ok(())
    }
    async fn pending(db: Db) -> Result<i64> {
        let client = db.get().await?;
        Ok(client
            .query_one(
                "SELECT count(*) FROM base_events_deliveries WHERE subscriber=$1 AND delivered_at IS NULL",
                &[&SUBSCRIBER],
            )
            .await?
            .get(0))
    }
    async fn onboard(&self, supplier: &str) -> Result<()> {
        println!("subscriber onboards supplier {supplier}");
        self.db.get().await?.execute(
            "INSERT INTO event_subscriber_suppliers(tenant_id,code) VALUES(1,$1) ON CONFLICT DO NOTHING",
            &[&supplier],
        ).await?;
        Ok(())
    }
    async fn replay(&self) -> Result<()> {
        let replay = self
            .events
            .replay(
                &self.admin,
                SUBSCRIBER,
                bilibi_base::events::ReplayFrom::Event(1),
                1000,
            )
            .await?;
        println!("replay {SUBSCRIBER} from event 1: {replay:?}");
        self.until("replayed deliveries", |db| async move {
            Ok(Self::pending(db).await? == 0)
        })
        .await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    let modules = bilibi_module_registry::discover()?;
    verify_composition(
        &modules,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?;
    bilibi_database::migrate(&db, &modules).await?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut services = Services::default();
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.install(&modules)?;
    let grants = services.get::<Grants>("base/authz.grants")?;
    grants.reconcile().await?;
    let events = services.get::<Events>(SERVICE)?;

    println!("mounted Domain modules: {:?}", bilibi_domains::MODULES);
    println!(
        "mounted event types: {:?}",
        events.event_types().collect::<Vec<_>>()
    );
    println!(
        "mounted subscribers: {:?}",
        events.subscribers().collect::<Vec<_>>()
    );
    boot_refusals(&modules);

    let client = db.get().await?;
    let admin = services
        .get::<Authentication>("base/tenancy.authentication")?
        .authenticate(&**client, 1, 4, "events".into())
        .await?;
    drop(client);
    // Declaration grants nothing: the tenant admin grants delivery once.
    grants
        .set_system(&admin, PRINCIPAL, DELIVER, Some(true))
        .await?;
    let proof = Proof {
        db: db.clone(),
        boundary: services.get("base/database.mutations")?,
        queue: services.get("base/queue.store")?,
        events: events.clone(),
        admin,
    };

    let executor = Executor::from_services(
        &services,
        Config {
            concurrency: 4,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(60),
        },
    )?;
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let worker = tokio::spawn(executor.run(shutdown));

    let publisher = events.event_types().any(|name| name == RECEIVED);
    let subscriber = events.subscribers().any(|key| key == SUBSCRIBER);
    let result = match (publisher, subscriber) {
        (false, _) => {
            println!(
                "publisher command while unmounted: {:?}",
                proof.receive("GRN-0001", "ACME", "12.5").await
            );
            proof
                .state("no publisher mounted: nothing new is recorded or delivered")
                .await
        }
        (true, false) => {
            proof.receive("GRN-0001", "ACME", "12.5").await?;
            proof.commands_done().await?;
            proof.state("recorded with no subscriber mounted").await
        }
        (true, true) => both(&proof).await,
    };
    let _ = stop.send(true);
    let _ = worker.await;
    result
}

async fn both(proof: &Proof) -> Result<()> {
    proof
        .state("the subscriber is new: earlier events are not its deliveries")
        .await?;
    proof.onboard("ACME").await?;
    proof.receive("GRN-0002", "GLOBEX", "3").await?;
    proof.give_back("GRN-0002").await?;
    proof.receive("GRN-0003", "ACME", "7").await?;
    proof.receive("GRN-0003", "ACME", "7").await?;
    proof.commands_done().await?;
    let recorded: i64 = proof
        .db
        .get()
        .await?
        .query_one(
            "SELECT count(*) FROM base_events WHERE tenant_id=1 AND event_key='received:GRN-0003'",
            &[],
        )
        .await?
        .get(0);
    println!("GRN-0003 received twice: {recorded} event recorded under its key");
    // GRN-0003 is delivered; GRN-0002's receipt is refused at least once.
    proof
        .until("the first refusal", |db| async move {
            let client = db.get().await?;
            let failed: i64 = client.query_one(
                "SELECT count(*) FROM base_events_deliveries WHERE subscriber=$1 AND last_error IS NOT NULL AND delivered_at IS NULL",
                &[&SUBSCRIBER],
            ).await?.get(0);
            Ok(failed > 0 && Proof::pending(db).await? == 2)
        })
        .await?;
    proof
        .state("GLOBEX is unknown: its receipt retries and its return waits behind it")
        .await?;
    proof.copy().await?;
    proof.onboard("GLOBEX").await?;
    proof
        .until("the retry", |db| async move {
            Ok(Proof::pending(db).await? == 0)
        })
        .await?;
    proof
        .state("after the retry, receipt then return, in order")
        .await?;
    proof.copy().await?;
    proof.replay().await?;
    proof
        .state("the replay delivered GRN-0001 to the new subscriber")
        .await?;
    proof.copy().await?;
    proof.replay().await?;
    proof.copy().await
}
