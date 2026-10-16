//! Private running worker/owner specimen, not a unit-test harness.
//! Modes let an operator exercise actual process death, retries and revocation.
use bilibi_database::{pool, Change, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Contribution, Provenance, Services};
use bilibi_queue::{
    execution::{
        Config, Execution, Executor, Handler, HandlerBinding, HandlerFuture, HandlerService,
        Outcome, HANDLERS,
    },
    store, Queue, Worker, WORKERS,
};
use bilibi_tenancy::Authentication;
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();

fn allowlist(args: Value) -> Result<Value> {
    let object = args.as_object().ok_or(Error::Invalid)?;
    if object.len() != 1
        || !object.get("mode").and_then(Value::as_str).is_some_and(|s| {
            matches!(
                s,
                "complete"
                    | "restart"
                    | "retry"
                    | "discard"
                    | "cancel"
                    | "panic"
                    | "invalid"
                    | "heartbeat"
                    | "timeout"
                    | "late-write"
                    | "fenced-write"
            )
        })
    {
        return Err(Error::Invalid);
    }
    Ok(args)
}
// This owner exists only in the private executable. It proves its own bounded
// specimen account through Tenancy's production authentication edge.
struct SpecimenVerifier;
impl bilibi_tenancy::ActorVerifier for SpecimenVerifier {
    fn verify<'a>(
        &'a self,
        client: &'a dyn bilibi_tenancy::ProofClient,
        tenant: i64,
        user: i64,
    ) -> bilibi_tenancy::ProofFuture<'a, bilibi_tenancy::AccountIdentity> {
        Box::pin(async move {
            if tenant != 1
                || client
                    .query_opt(
                        "SELECT user_id FROM queue_execution_accounts WHERE user_id=$1 AND active",
                        &[&user],
                    )
                    .await?
                    .is_none()
            {
                return Err(Error::Unauthorized);
            }
            Ok(bilibi_tenancy::AccountIdentity {
                name: "Private queue actor".into(),
                email: "queue@example.test".into(),
                person: None,
                organisation: None,
                affiliation: None,
                installation_owner: false,
            })
        })
    }
}
inventory::submit! { bilibi_module_registry::Module {
    id:"extensions/queue_specimen",layer:bilibi_module_registry::Layer::Extensions,
    dependencies:&["base/queue","base/tenancy"],
    migrations:&[("extensions-queue-specimen-001-accounts","CREATE TABLE queue_execution_accounts(user_id bigint PRIMARY KEY, active boolean NOT NULL)")],
    services:&[bilibi_module_registry::Service {id:"extensions/queue_specimen.actor",factory:|services| {
        services.get::<Authentication>("base/tenancy.authentication")?.bind(Arc::new(SpecimenVerifier))?;
        Ok(Arc::new(()))
    }}],
} }
struct Specimen {
    db: Db,
}
impl Handler for Specimen {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let mode = args["mode"].as_str().unwrap_or("invalid");
            println!(
                "handling job={} attempt={} mode={mode}",
                execution.job_id, execution.attempt
            );
            match mode {
                "retry" if execution.attempt == 1 => return Outcome::Retry("specimen_retry"),
                "discard" => return Outcome::Retry("specimen_failed"),
                "cancel" => return Outcome::Cancel("specimen_cancel"),
                "invalid" => return Outcome::Retry("not a safe code"),
                "panic" if execution.attempt == 1 => panic!("private specimen panic"),
                "restart" if execution.attempt == 1 => {
                    tokio::time::sleep(Duration::from_secs(60)).await
                }
                "heartbeat" => tokio::time::sleep(Duration::from_secs(8)).await,
                "timeout" => tokio::time::sleep(Duration::from_secs(60)).await,
                "late-write" => tokio::time::sleep(Duration::from_secs(5)).await,
                "fenced-write" => tokio::time::sleep(Duration::from_millis(700)).await,
                _ => {}
            }
            let result = self.effect(execution).await;
            match result {
                Ok(()) => Outcome::Completed,
                Err(Error::Unauthorized | Error::Forbidden) => {
                    Outcome::Cancel("owner_actor_refused")
                }
                Err(Error::Conflict) => {
                    println!("fenced owner write refused job={}", execution.job_id);
                    Outcome::Cancel("owner_lease_refused")
                }
                Err(_) => Outcome::Retry("owner_unavailable"),
            }
        })
    }
}
impl Specimen {
    async fn effect(&self, execution: &Execution) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = execution
            .begin(&mut client, "queue.specimen.effect")
            .await?;
        let id = execution.job_id;
        // The job ID is a durable owner idempotency key. Duplicate delivery
        // returns the existing effect rather than applying it again.
        tx.mutate(move |sql|Box::pin(async move {
            let inserted = sql.execute("INSERT INTO queue_execution_effects(job_id) VALUES($1) ON CONFLICT DO NOTHING", &[&id]).await?;
            Ok(((),vec![Change {kind:"queue.specimen.effect".into(),id,old:Value::Null,new:json!({"applied":inserted == 1})}]))
        })).await?;
        tx.commit().await
    }
}
inventory::submit! { Contribution::new(WORKERS,"queue/execution-walkthrough",Provenance {module:"base/queue",source:file!(),line:line!()},&Worker {max_attempts:3,validate:allowlist}) }
inventory::submit! { Contribution::new(HANDLERS,"queue/execution-walkthrough",Provenance {module:"base/queue",source:file!(),line:line!()},&HandlerBinding {service:"base/queue.specimen",capability:"companies.update"}) }

#[tokio::main]
async fn main() -> Result<()> {
    if std::env::var("BILIBI_DISPOSABLE_QUEUE_DEMO").as_deref() != Ok("yes") {
        return Err(Error::Invalid);
    }
    let db = pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let mut services = Services::default();
    let modules = bilibi_module_registry::discover()?;
    if std::env::args().nth(1).as_deref() == Some("prepare") {
        bilibi_database::migrate(&db, &modules).await?;
        db.get().await?.batch_execute("INSERT INTO tenants(id,name) VALUES(1,'Private queue specimen'),(2,'Other private tenant') ON CONFLICT DO NOTHING; INSERT INTO queue_execution_accounts(user_id,active) VALUES(1,true) ON CONFLICT DO NOTHING; INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,1,'companies.update',true) ON CONFLICT DO NOTHING").await?;
        println!("private prerequisite-only host prepared");
        return Ok(());
    }
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.install(&modules)?;
    services.provide(
        "base/queue.specimen",
        Arc::new(HandlerService(Arc::new(Specimen { db: db.clone() }))),
    )?;
    let command = std::env::args().nth(1).unwrap_or_else(|| "inspect".into());
    let queue = services.get::<Queue>("base/queue.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let mut client = db.get().await?;
    client.batch_execute("CREATE TABLE IF NOT EXISTS queue_execution_effects(job_id bigint PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now())").await?;
    if command == "worker" {
        drop(client);
        let config = Config {
            concurrency: 2,
            poll_interval: Duration::from_millis(100),
            lease_seconds: 3,
            max_runtime: Duration::from_secs(
                std::env::var("QUEUE_DEMO_RUNTIME")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(90),
            ),
        };
        let executor = Executor::from_services(&services, config)?;
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let running = tokio::spawn(executor.run(stopped));
        println!("private worker pid={}", std::process::id());
        let _ = tokio::signal::ctrl_c().await;
        let _ = stop.send(true);
        return running.await.map_err(|_| Error::Internal)?;
    }
    if command == "inspect" {
        let rows = client.query("SELECT j.id,j.tenant_id,j.state,j.attempt,j.failure_code,count(e.job_id)::bigint AS effects FROM base_queue_jobs j LEFT JOIN queue_execution_effects e ON e.job_id=j.id WHERE j.worker_id='queue/execution-walkthrough' GROUP BY j.id ORDER BY j.id",&[]).await?;
        for row in rows {
            let state = store::lookup(&**client, row.get("tenant_id"), row.get("id")).await?;
            println!(
                "job={} state={:?} terminal={} attempt={} code={:?} effects={}",
                state.id,
                state.state,
                state.state.terminal(),
                state.attempt,
                state.failure_code,
                row.get::<_, i64>("effects")
            );
        }
        return Ok(());
    }
    let user = std::env::var("QUEUE_DEMO_USER")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let actor = authentication
        .authenticate(&**client, 1, user, String::new())
        .await?;
    bilibi_authz::authorize_key(&**client, &actor, "companies.update").await?;
    let mut tx = boundary
        .begin(
            &mut client,
            actor.mutation_context(format!("queue.specimen.{command}")),
        )
        .await?;
    match command.as_str() {
        "enqueue" => {
            let mode = std::env::args().nth(2).unwrap_or_else(|| "complete".into());
            let job = queue
                .enqueue_for(
                    &mut tx,
                    &actor,
                    "queue/execution-walkthrough",
                    json!({"mode":mode}),
                )
                .await?;
            tx.commit().await?;
            println!("enqueued job={} mode={mode} user={user}", job.id());
        }
        "cancel" | "retry" => {
            let id = std::env::args()
                .nth(2)
                .and_then(|s| s.parse().ok())
                .ok_or(Error::Invalid)?;
            let state = if command == "cancel" {
                store::cancel_for(&mut tx, &actor, id).await?
            } else {
                store::retry_for(&mut tx, &actor, id).await?
            };
            tx.commit().await?;
            println!("{command} job={id} state={state:?}");
        }
        _ => return Err(Error::Invalid),
    }
    Ok(())
}
