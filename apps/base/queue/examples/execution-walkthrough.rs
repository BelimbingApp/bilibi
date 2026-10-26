//! Private running worker/owner specimen, not a unit-test harness.
//! Modes let an operator exercise actual process death, retries and revocation,
//! and (SP1) a job that runs as a named system principal with a sealed scope.
use bilibi_authz::{
    grants::Grants, CapabilityDefinition, SystemPrincipalDefinition, SystemScope,
    VocabularyContribution, VOCABULARY,
};
use bilibi_base::{
    party_ids::OrganisationId,
    settings::{Definition, Editor, ScopeKind, Setting, Settings, Target, ValueType, DEFINITIONS},
    step_up::StepUp,
};
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

/// The specimen's named system principal, declared for the binding's
/// capability and for the company secret's editor capability. Declaration
/// grants nothing: `grant` gives it the durable tenant grants.
const PRINCIPAL: &str = "queue-specimen";
const SECRET_VIEW: &str = "companies.secret.view";
/// An encrypted company setting only the granted principal may read.
const SECRET_KEY: &str = "queue.specimen.secret";
/// The fixture company the specimen reads its secret for.
const COMPANY: i64 = 1;
inventory::submit! { Contribution::new(VOCABULARY, "queue.execution-walkthrough", Provenance { module: "base/queue", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &[], verbs: &[],
    capabilities: &[CapabilityDefinition::tenant(SECRET_VIEW)],
    roles: &[],
    system_principals: &[SystemPrincipalDefinition { name: PRINCIPAL, capabilities: &["companies.update", SECRET_VIEW] }],
}) }
inventory::submit! { Contribution::new(DEFINITIONS, SECRET_KEY, Provenance { module: "base/queue", source: file!(), line: line!() }, &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Company], default: || Value::Null, nullable: true, encrypted: true,
    minimum: None, maximum: None, validator: None,
    editor: Some(Editor { group: "Queue specimen", label: "Specimen company secret", help: "A private walkthrough value, stored encrypted.", capability: Some(SECRET_VIEW), reveal_duration_ms: 5_000 }),
})) }

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
                    | "secret"
            )
        })
    {
        return Err(Error::Invalid);
    }
    Ok(args)
}
// The fixture's accounts sign in through Core User's verifier, which the
// linked Platform binds; this executable binds none of its own.
struct Specimen {
    db: Db,
    settings: Arc<Settings>,
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
                "secret" => {
                    if let Some(outcome) = self.read_secret(execution).await {
                        return outcome;
                    }
                }
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
    /// SP1: the company secret through the attempt's sealed scope. Settings
    /// hands a principal the plaintext only with the definition's capability
    /// granted; the value itself is never printed. `None` lets the effect run.
    async fn read_secret(&self, execution: &Execution) -> Option<Outcome> {
        let Some(scope) = execution.scope() else {
            return Some(Outcome::Cancel("scope_missing"));
        };
        println!(
            "running as principal={:?} user={} name={:?} actor_kind={}",
            scope.principal(),
            scope.user(),
            scope.user_name,
            scope.mutation_context("queue.specimen.probe").actor.kind
        );
        let Ok(client) = self.db.get().await else {
            return Some(Outcome::Retry("owner_unavailable"));
        };
        let Ok(company) = OrganisationId::new(COMPANY) else {
            return Some(Outcome::Cancel("company_invalid"));
        };
        // A granted principal reaches every live company of its tenant
        // (`authorize_target` asks it for no tenant-wide key); the archived
        // check is the same as for an account.
        let target =
            bilibi_core::company::authorize_target(&**client, scope, COMPANY, "companies.update")
                .await;
        let writable =
            bilibi_core::company::require_writable(&**client, scope, COMPANY, false).await;
        println!("company target: {target:?} writable: {writable:?}");
        match self
            .settings
            .get(&**client, scope, Target::Company(company), SECRET_KEY)
            .await
        {
            Ok(resolved) => {
                let value = resolved.value.as_str();
                println!(
                    "secret read: decrypted={} chars={}",
                    resolved.encrypted && value.is_some(),
                    value.map_or(0, |s| s.chars().count())
                );
                None
            }
            Err(Error::Forbidden) => {
                println!("secret read refused: Forbidden");
                Some(Outcome::Cancel("secret_refused"))
            }
            Err(error) => {
                println!("secret read failed: {error:?}");
                Some(Outcome::Retry("owner_unavailable"))
            }
        }
    }
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
    let mut services = Services::default();
    let modules = bilibi_module_registry::discover()?;
    let prepare = std::env::args().nth(1).as_deref() == Some("prepare");
    if prepare {
        bilibi_database::migrate(&db, &modules).await?;
    }
    bilibi_walkthrough_fixture::install(&db).await?;
    if prepare {
        // The fixture's manager (user 1) enqueues as the user; its grant-all
        // administrator (user 4, admin@example.test) grants the principal and
        // stores the company secret.
        db.get().await?.batch_execute("INSERT INTO principal_capabilities(tenant_id,user_id,capability,is_allowed) VALUES(1,1,'companies.update',true) ON CONFLICT DO NOTHING").await?;
        println!("private prerequisite-only host prepared");
        return Ok(());
    }
    services.provide("bootstrap.database", Arc::new(db.clone()))?;
    services.provide(
        "bootstrap.composition",
        std::sync::Arc::new(bilibi_web::composition()),
    )?;
    services.install(&modules)?;
    // This fixture binary enrols no authenticator (README of base/step_up).
    services
        .get::<StepUp>(bilibi_base::step_up::SERVICE)?
        .set_enforced(false);
    let settings = services.get::<Settings>("base/settings.store")?;
    services.provide(
        "base/queue.specimen",
        Arc::new(HandlerService(Arc::new(Specimen {
            db: db.clone(),
            settings: settings.clone(),
        }))),
    )?;
    let command = std::env::args().nth(1).unwrap_or_else(|| "inspect".into());
    let queue = services.get::<Queue>("base/queue.store")?;
    let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
    let authentication = services.get::<Authentication>("base/tenancy.authentication")?;
    let grants = services.get::<Grants>("base/authz.grants")?;
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
        let rows = client.query("SELECT j.id,j.tenant_id,j.state,j.attempt,j.failure_code,j.actor_kind,j.system_principal,count(e.job_id)::bigint AS effects FROM base_queue_jobs j LEFT JOIN queue_execution_effects e ON e.job_id=j.id WHERE j.worker_id='queue/execution-walkthrough' GROUP BY j.id ORDER BY j.id",&[]).await?;
        for row in rows {
            let state = store::lookup(&**client, row.get("tenant_id"), row.get("id")).await?;
            println!(
                "job={} state={:?} terminal={} attempt={} code={:?} actor={} principal={:?} effects={}",
                state.id,
                state.state,
                state.state.terminal(),
                state.attempt,
                state.failure_code,
                row.get::<_, String>("actor_kind"),
                row.get::<_, Option<String>>("system_principal"),
                row.get::<_, i64>("effects")
            );
        }
        // The effects' audit rows: a system job's name its principal (SP1).
        let audits = client.query("SELECT auditable_id,actor_type,actor_id,system_principal,event FROM base_audit_mutations WHERE auditable_type='queue.specimen.effect' ORDER BY id",&[]).await?;
        for row in audits {
            println!(
                "audit effect job={} actor_type={} actor_id={} system_principal={:?} event={}",
                row.get::<_, String>("auditable_id"),
                row.get::<_, String>("actor_type"),
                row.get::<_, i64>("actor_id"),
                row.get::<_, Option<String>>("system_principal"),
                row.get::<_, String>("event")
            );
        }
        return Ok(());
    }
    if command == "enqueue-system" {
        // SP1: the specimen job as the declared principal, the way a scheduler
        // or a connector's poll enqueues unattended work.
        let mode = std::env::args().nth(2).unwrap_or_else(|| "secret".into());
        let system = SystemScope::rehydrate(&**client, 1, Some(PRINCIPAL)).await?;
        let mut tx = boundary
            .begin(
                &mut client,
                system.mutation_context("queue.specimen.enqueue-system"),
            )
            .await?;
        let job = queue
            .enqueue_system(
                &mut tx,
                &system,
                "queue/execution-walkthrough",
                json!({"mode":mode}),
            )
            .await?;
        tx.commit().await?;
        println!(
            "enqueued job={} mode={mode} principal={PRINCIPAL}",
            job.id()
        );
        return Ok(());
    }
    if matches!(
        command.as_str(),
        "grant" | "revoke" | "revoke-secret" | "store-secret"
    ) {
        // The fixture administrator acts; `set_system` is the same audited
        // write the Authz system-principal screen makes.
        let admin = authentication
            .authenticate(&**client, 1, 4, String::new())
            .await?;
        match command.as_str() {
            "grant" => {
                for key in ["companies.update", SECRET_VIEW] {
                    grants
                        .set_system(&admin, PRINCIPAL, key, Some(true))
                        .await?;
                }
                println!("granted {PRINCIPAL}: companies.update, {SECRET_VIEW}");
            }
            "revoke-secret" => {
                grants
                    .set_system(&admin, PRINCIPAL, SECRET_VIEW, None)
                    .await?;
                println!("revoked {PRINCIPAL}: {SECRET_VIEW}");
            }
            "revoke" => {
                for key in ["companies.update", SECRET_VIEW] {
                    grants.set_system(&admin, PRINCIPAL, key, None).await?;
                }
                println!("revoked {PRINCIPAL}: companies.update, {SECRET_VIEW}");
            }
            _ => {
                // Stored encrypted; the worker later proves it can read it
                // without this process or the worker printing the value.
                let company = OrganisationId::new(COMPANY).map_err(|_| Error::Invalid)?;
                let mut tx = boundary
                    .begin(
                        &mut client,
                        admin.mutation_context("queue.specimen.store-secret"),
                    )
                    .await?;
                settings
                    .put(
                        &mut tx,
                        &admin,
                        Target::Company(company),
                        SECRET_KEY,
                        json!("specimen-secret-not-for-printing"),
                    )
                    .await?;
                tx.commit().await?;
                println!("stored {SECRET_KEY} for company {COMPANY} (encrypted)");
            }
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
