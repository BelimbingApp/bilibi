//! Tokio execution edge. DB leases provide at-least-once delivery, not exactly-once
//! effects. Handlers must use owner idempotency and reauthorize owner operations.
use crate::{arguments, store, Queue};
use bilibi_authz::{SystemAuthentication, SystemScope};
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError, Services};
use bilibi_tenancy::{Authentication, Scope};
use serde_json::Value;
use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio::{sync::watch, task::JoinSet};

pub const HANDLERS: &str = "base/queue.handlers";
pub const OBSERVERS: &str = "base/queue.observers";
pub type HandlerFuture<'a> = Pin<Box<dyn Future<Output = Outcome> + Send + 'a>>;
/// Owner services implement this bridge and contribute a matching HandlerBinding.
/// A retry/cancel code must be lowercase [a-z][a-z0-9_]{0,63}; error payloads,
/// panic messages, SQL and business args are never persisted in queue failures.
pub trait Handler: Send + Sync {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a>;
}
pub struct HandlerService(pub Arc<dyn Handler>);
#[derive(Clone, Copy)]
pub struct HandlerBinding {
    pub service: &'static str,
    /// Rechecked on each attempt and inside Execution::begin. Owners still guard
    /// target-specific effects; this gate alone does not prove a target.
    pub capability: &'static str,
}
#[derive(Clone, Debug)]
pub enum Outcome {
    Completed,
    Retry(&'static str),
    Cancel(&'static str),
}
fn valid_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}
fn validate_bindings(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        let binding = entry.get::<HandlerBinding>()?;
        if !binding
            .service
            .starts_with(&format!("{}.", entry.provenance.module))
            || binding.capability.is_empty()
            || binding.capability.len() > 200
        {
            return Err(RegistryError::Contribution(format!(
                "invalid handler binding {}",
                entry.id
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<HandlerBinding>(HANDLERS,"base/queue",validate_bindings) }

/// What an observer may know about one attempt: stable facts only, never
/// arguments, actor or tenant.
#[derive(Clone, Debug)]
pub struct JobFacts {
    pub worker_id: String,
    pub attempt: i32,
    pub max_attempts: i32,
}
/// Wraps one whole attempt (actor rehydration, authorization and the owner's
/// handler) to measure it. An observer must never delay, alter or fail the work:
/// it returns the same outcome it was handed. If the attempt is dropped (lease
/// timeout, shutdown) the wrapped future is dropped with it.
pub trait Observer: Send + Sync {
    fn observe<'a>(&'a self, job: JobFacts, work: HandlerFuture<'a>) -> HandlerFuture<'a>;
}
pub struct ObserverService(pub Arc<dyn Observer>);
/// Contributed to `OBSERVERS` by a module that measures jobs (Perf). Its `service`
/// must belong to that module and resolve to `ObserverService`.
#[derive(Clone, Copy)]
pub struct ObserverBinding {
    pub service: &'static str,
}
fn validate_observers(entries: &[Entry]) -> bilibi_module_registry::Result<()> {
    for entry in entries {
        let binding = entry.get::<ObserverBinding>()?;
        if !binding
            .service
            .starts_with(&format!("{}.", entry.provenance.module))
        {
            return Err(RegistryError::Contribution(format!(
                "invalid observer binding {}",
                entry.id
            )));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<ObserverBinding>(OBSERVERS,"base/queue",validate_observers) }

/// Who the job runs as, re-proven on every attempt and again in `begin`.
#[derive(Clone, Debug)]
pub enum Identity {
    /// The user who enqueued the job, with the same party facets as then.
    /// Boxed: a sealed scope is far larger than a system identity.
    User(Box<Scope>),
    /// A named system principal, declared in Authz with a durable tenant grant.
    System(SystemScope),
}
#[derive(Clone)]
pub struct Execution {
    pub job_id: i64,
    pub attempt: i32,
    pub max_attempts: i32,
    identity: Identity,
    /// The sealed scope this attempt runs under: the user's, or the system
    /// principal's (`Authentication::for_system`, SP1), built per attempt.
    scope: Box<Scope>,
    token: String,
    trace: String,
    capability: &'static str,
    authentication: Arc<Authentication>,
    boundary: Arc<MutationBoundary>,
}
impl Execution {
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
    /// The sealed scope to hand owner facades: the user behind a
    /// user-enqueued job, or the named system principal of a system job
    /// (SP1), re-sealed on every attempt. `Some` for both kinds; the
    /// `Option` is kept for the owners written when a system job had none.
    pub fn scope(&self) -> Option<&Scope> {
        Some(&self.scope)
    }
    pub fn tenant(&self) -> i64 {
        match &self.identity {
            Identity::User(scope) => scope.tenant(),
            Identity::System(scope) => scope.tenant(),
        }
    }
    /// Fences a business transaction against expired/replaced/cancelled claims.
    /// The queue row remains locked until owner commit/rollback, serializing it
    /// with cancellation and recovery. Reauthorize target-specific operations
    /// inside this transaction too. Keep it short; never hold it over external IO.
    /// An external effect still requires the owner's durable idempotency key.
    pub async fn begin<'a>(
        &self,
        client: &'a mut tokio_postgres::Client,
        action: impl Into<String>,
    ) -> Result<MutationTransaction<'a>> {
        let mut context = match &self.identity {
            Identity::User(scope) => scope.mutation_context(action),
            Identity::System(scope) => scope.mutation_context(action),
        };
        context.trace_id = self.trace.clone();
        let tx = self.boundary.begin(client, context).await?;
        let alive = tx.sql().query_opt("SELECT id FROM base_queue_jobs WHERE id=$1 AND tenant_id=$2 AND state='executing' AND lease_token=$3 AND lease_expires_at>clock_timestamp() FOR UPDATE", &[&self.job_id,&self.tenant(),&self.token]).await?.is_some();
        if !alive {
            return Err(Error::Conflict);
        }
        match &self.identity {
            Identity::User(captured) => {
                let scope = self
                    .authentication
                    .authenticate(tx.sql(), captured.tenant(), captured.user(), String::new())
                    .await?;
                if !same_actor(&scope, captured) {
                    return Err(Error::Unauthorized);
                }
                bilibi_authz::authorize_key(tx.sql(), &scope, self.capability).await?;
            }
            Identity::System(captured) => {
                let scope =
                    SystemScope::rehydrate(tx.sql(), captured.tenant(), captured.principal())
                        .await?;
                bilibi_authz::authorize_system(tx.sql(), &scope, self.capability).await?;
            }
        }
        Ok(tx)
    }
}
fn same_actor(a: &Scope, b: &Scope) -> bool {
    a.tenant() == b.tenant()
        && a.user() == b.user()
        && a.actor().person() == b.actor().person()
        && a.actor().organisation() == b.actor().organisation()
        && a.actor().affiliation() == b.actor().affiliation()
}

#[derive(Clone, Copy, Debug)]
pub struct Config {
    pub concurrency: usize,
    pub poll_interval: Duration,
    pub lease_seconds: i32,
    pub max_runtime: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            concurrency: 10,
            poll_interval: Duration::from_secs(1),
            lease_seconds: 30,
            max_runtime: Duration::from_secs(900),
        }
    }
}
impl Config {
    fn validate(self) -> Result<Self> {
        if !(1..=64).contains(&self.concurrency)
            || !(3..=3600).contains(&self.lease_seconds)
            || self.poll_interval < Duration::from_millis(10)
            || self.poll_interval > Duration::from_secs(60)
            || self.max_runtime.is_zero()
            || self.max_runtime > Duration::from_secs(86400)
        {
            return Err(Error::Invalid);
        }
        Ok(self)
    }
}
struct BoundHandler {
    service: Arc<HandlerService>,
    capability: &'static str,
}
pub struct Executor {
    db: Db,
    queue: Arc<Queue>,
    authentication: Arc<Authentication>,
    boundary: Arc<MutationBoundary>,
    handlers: BTreeMap<&'static str, BoundHandler>,
    observers: Vec<Arc<ObserverService>>,
    config: Config,
}
impl Executor {
    /// Call after Services::install: owners are upstream of Queue and bind their
    /// handler services during boot. Missing/foreign bindings fail before work.
    pub fn from_services(services: &Services, config: Config) -> Result<Arc<Self>> {
        let queue = services.get::<Queue>("base/queue.store")?;
        let catalog: &Catalog = services.catalog()?;
        let mut handlers = BTreeMap::new();
        for entry in catalog.entries::<HandlerBinding>(HANDLERS)? {
            let worker = queue.workers().get(entry.id)?;
            if worker.module != entry.provenance.module {
                return Err(Error::Invalid);
            }
            let binding = entry.get::<HandlerBinding>()?;
            if !bilibi_authz::vocabulary()?
                .capabilities
                .contains_key(binding.capability)
            {
                return Err(Error::Invalid);
            }
            handlers.insert(
                entry.id,
                BoundHandler {
                    service: services.get(binding.service)?,
                    capability: binding.capability,
                },
            );
        }
        if queue.workers().iter().any(|w| !handlers.contains_key(w.id)) {
            return Err(Error::Invalid);
        }
        let observers = catalog
            .entries::<ObserverBinding>(OBSERVERS)?
            .iter()
            .map(|entry| services.get(entry.get::<ObserverBinding>()?.service))
            .collect::<bilibi_module_registry::Result<_>>()?;
        Ok(Arc::new(Self {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            queue,
            authentication: services.get("base/tenancy.authentication")?,
            boundary: services.get("base/database.mutations")?,
            handlers,
            observers,
            config: config.validate()?,
        }))
    }
    /// One loop per slot, bounded by config; each app process runs independently.
    /// Stopping drops in-flight handlers (transactions roll back). Their leases
    /// remain durable and are recovered by a subsequent worker process.
    pub async fn run(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        let mut slots = JoinSet::new();
        for _ in 0..self.config.concurrency {
            let executor = self.clone();
            let stop = shutdown.clone();
            slots.spawn(async move { executor.slot(stop).await });
        }
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                change = shutdown.changed() => { if change.is_err() || *shutdown.borrow() { break; } }
                result = slots.join_next() => {
                    return match result { Some(Ok(result)) => result, _ => Err(Error::Internal) };
                }
            }
        }
        slots.abort_all();
        while slots.join_next().await.is_some() {}
        Ok(())
    }
    async fn slot(self: Arc<Self>, mut stop: watch::Receiver<bool>) -> Result<()> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            // Infrastructure errors do not expose SQL/args or terminate the whole
            // executor; abandoned claims are reconciled by DB lease recovery.
            match store::claim(&self.db, self.config.lease_seconds).await {
                Ok(Some(claim)) => {
                    if self.execute(claim).await.is_err() {
                        eprintln!("queue execution infrastructure unavailable");
                    }
                }
                Ok(None) => {}
                Err(_) => eprintln!("queue claim infrastructure unavailable"),
            }
            tokio::select! {
                _ = tokio::time::sleep(self.config.poll_interval) => {}
                _ = stop.changed() => { return Ok(()); }
            }
        }
    }
    async fn prepare(
        &self,
        claim: &store::Claim,
    ) -> Result<(Execution, Arc<HandlerService>, Value)> {
        let row = &claim.row;
        let worker = self.queue.workers().get(row.get("worker_id"))?;
        if worker.module != row.get::<_, &str>("worker_module") {
            return Err(Error::Invalid);
        }
        let bound = self.handlers.get(worker.id).ok_or(Error::Invalid)?;
        let client = self.db.get().await?;
        let (identity, scope) = match row.get::<_, &str>("actor_kind") {
            "user" => {
                let scope = self
                    .authentication
                    .authenticate(
                        &**client,
                        row.get("tenant_id"),
                        row.get("actor_id"),
                        String::new(),
                    )
                    .await?;
                // Captured identity facets must still be current. Never substitute a new
                // organisation or affiliation simply because the account changed.
                if scope.actor().person().map(|id| id.get())
                    != row.get::<_, Option<i64>>("person_id")
                    || scope.actor().organisation().map(|id| id.get())
                        != row.get::<_, Option<i64>>("organisation_id")
                    || scope.actor().affiliation().map(|id| id.get())
                        != row.get::<_, Option<i64>>("affiliation_id")
                {
                    return Err(Error::Unauthorized);
                }
                bilibi_authz::authorize_key(&**client, &scope, bound.capability).await?;
                let sealed = Box::new(scope.clone());
                (Identity::User(Box::new(scope)), sealed)
            }
            "system" => {
                // Declaration and durable grant are read live; a principal whose
                // declaration was removed, or a tenant no longer active, is refused.
                let principal: Option<String> = row.get("system_principal");
                let scope =
                    SystemScope::rehydrate(&**client, row.get("tenant_id"), principal.as_deref())
                        .await?;
                if scope.principal().is_none() {
                    return Err(Error::Unauthorized);
                }
                bilibi_authz::authorize_system(&**client, &scope, bound.capability).await?;
                // The principal's sealed scope for this attempt (SP1): owner
                // facades decide on it by the principal's name.
                let sealed = Box::new(self.authentication.for_system(&**client, &scope).await?);
                (Identity::System(scope), sealed)
            }
            _ => return Err(Error::Invalid),
        };
        let args: Value = row.get("args");
        arguments::validate(&args)?;
        let args = (worker.definition.validate)(args)?;
        arguments::validate(&args)?;
        Ok((
            Execution {
                job_id: row.get("id"),
                attempt: row.get("attempt"),
                max_attempts: row.get("max_attempts"),
                identity,
                scope,
                token: claim.token.clone(),
                trace: row.get("trace_id"),
                capability: bound.capability,
                authentication: self.authentication.clone(),
                boundary: self.boundary.clone(),
            },
            bound.service.clone(),
            args,
        ))
    }
    async fn execute(self: &Arc<Self>, claim: store::Claim) -> Result<()> {
        let id = claim.row.get("id");
        let attempt: i32 = claim.row.get("attempt");
        let max: i32 = claim.row.get("max_attempts");
        let token = claim.token.clone();
        // Preparation runs in a child too: validator panics and slow verifiers
        // are isolated, bounded, and heartbeat-protected just like owner code.
        let executor = self.clone();
        let facts = JobFacts {
            worker_id: claim.row.get("worker_id"),
            attempt,
            max_attempts: max,
        };
        let mut child = JoinSet::new();
        child.spawn(async move {
            let work: HandlerFuture<'_> = Box::pin(async {
                match executor.prepare(&claim).await {
                    Ok((execution, handler, args)) => handler.0.perform(&execution, args).await,
                    Err(Error::Unauthorized | Error::Forbidden) => Outcome::Cancel("actor_refused"),
                    Err(Error::Invalid | Error::NotFound) => Outcome::Cancel("worker_args_refused"),
                    Err(_) => Outcome::Retry("prepare_unavailable"),
                }
            });
            executor
                .observers
                .iter()
                .fold(work, |work, observer| {
                    observer.0.observe(facts.clone(), work)
                })
                .await
        });
        let mut heartbeat =
            tokio::time::interval(Duration::from_secs(self.config.lease_seconds as u64 / 3));
        let deadline = tokio::time::sleep(self.config.max_runtime);
        tokio::pin!(deadline);
        let outcome = loop {
            tokio::select! {
                result = child.join_next() => break match result {Some(Ok(outcome))=>outcome,_=>Outcome::Retry("worker_panicked")},
                _ = &mut deadline => break Outcome::Retry("worker_timed_out"),
                _ = heartbeat.tick() => {
                    let renewed = tokio::time::timeout(
                        Duration::from_secs(self.config.lease_seconds as u64 / 3),
                        store::heartbeat(&self.db,id,&token,self.config.lease_seconds),
                    ).await.map_err(|_|Error::Internal)??;
                    if !renewed { return Ok(()); }
                }
            }
        };
        child.abort_all();
        while child.join_next().await.is_some() {}
        let (state, code) = match outcome {
            Outcome::Completed => ("completed", None),
            Outcome::Cancel(code) if valid_code(code) => ("cancelled", Some(code)),
            Outcome::Retry(code) if valid_code(code) => (
                if attempt >= max {
                    "discarded"
                } else {
                    "retryable"
                },
                Some(code),
            ),
            _ => (
                if attempt >= max {
                    "discarded"
                } else {
                    "retryable"
                },
                Some("invalid_worker_result"),
            ),
        };
        store::finish(&self.db, id, &token, state, code).await?;
        Ok(())
    }
}
