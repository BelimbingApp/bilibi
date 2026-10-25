//! Durable insertion and lease-fenced execution. Owners must make effects idempotent.
pub mod arguments;
pub mod execution;
mod registry;
pub mod store;
pub use registry::{RegisteredWorker, Worker, Workers, WORKERS};

use bilibi_authz::SystemScope;
use bilibi_database::{Change, Error, MutationTransaction, Result};
use bilibi_module_registry::{Catalog, Module, Service};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::Arc;

/// Stable reference; transport arguments and actor metadata never escape here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct JobRef {
    id: i64,
}
impl JobRef {
    pub fn id(self) -> i64 {
        self.id
    }
}
pub struct Queue {
    workers: Workers,
}
impl Queue {
    pub fn new(workers: Workers) -> Self {
        Self { workers }
    }
    pub fn workers(&self) -> &Workers {
        &self.workers
    }

    /// Captures identity from a sealed scope, never from args or caller metadata.
    /// Scope and audit context must agree. Caller retains commit/rollback control;
    /// `JobRef` is provisional until that outer transaction commits.
    /// This grants no capability: the owner must authorize its command and Q2
    /// must rehydrate current actor eligibility and reauthorize every execution.
    /// A named system principal's scope (SP1, `Execution::scope` of a system
    /// job) enqueues a system job as that principal, like `enqueue_system`.
    pub async fn enqueue_for(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        worker_id: &str,
        args: Value,
    ) -> Result<JobRef> {
        if let Some(principal) = scope.principal() {
            if tx.context().tenant != scope.tenant()
                || tx.context().actor.kind != "system"
                || tx.context().actor.id != principal
                || tx.context().impersonator.is_some()
                || !bilibi_authz::vocabulary()?
                    .system_principals
                    .contains_key(principal)
            {
                return Err(Error::Forbidden);
            }
            let actor = Actor::System(principal.to_owned());
            return self
                .insert(tx, scope.tenant(), actor, worker_id, args)
                .await;
        }
        if tx.context().tenant != scope.tenant()
            || tx.context().actor.kind != "user"
            || tx.context().actor.id != scope.user().to_string()
            || tx.context().impersonator.is_some()
        {
            return Err(Error::Forbidden);
        }
        let actor = Actor::User {
            user: scope.user(),
            person: scope.actor().person().map(|id| id.get()),
            organisation: scope.actor().organisation().map(|id| id.get()),
            affiliation: scope.actor().affiliation().map(|id| id.get()),
        };
        self.insert(tx, scope.tenant(), actor, worker_id, args)
            .await
    }
    /// Enqueue by a named system principal (a scheduler or maintenance edge),
    /// never a browser. The audit context must already name that principal in
    /// the same tenant, and the principal must be declared in Authz. Execution
    /// re-proves the live tenant, declaration and durable grant on every attempt.
    pub async fn enqueue_system(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &SystemScope,
        worker_id: &str,
        args: Value,
    ) -> Result<JobRef> {
        let principal = scope.principal().ok_or(Error::Forbidden)?;
        if tx.context().tenant != scope.tenant()
            || tx.context().actor.kind != "system"
            || tx.context().actor.id != principal
            || tx.context().impersonator.is_some()
            || !bilibi_authz::vocabulary()?
                .system_principals
                .contains_key(principal)
        {
            return Err(Error::Forbidden);
        }
        let actor = Actor::System(principal.to_owned());
        self.insert(tx, scope.tenant(), actor, worker_id, args)
            .await
    }
    /// Enqueue, inside another actor's transaction, a job that runs as a named
    /// system principal: a Platform delivery edge (Base Events) whose work is
    /// not that actor's command. The job and its audit row commit with the
    /// owner's writes and stay attributed to the transaction's actor. The job
    /// executes only as the principal, which re-proves its live declaration and
    /// durable tenant grant on every attempt, so it gains nothing the principal
    /// is not already granted in that tenant.
    pub async fn enqueue_system_within(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &SystemScope,
        worker_id: &str,
        args: Value,
    ) -> Result<JobRef> {
        let principal = scope.principal().ok_or(Error::Forbidden)?;
        if tx.context().tenant != scope.tenant()
            || tx.context().impersonator.is_some()
            || !bilibi_authz::vocabulary()?
                .system_principals
                .contains_key(principal)
        {
            return Err(Error::Forbidden);
        }
        let actor = Actor::System(principal.to_owned());
        self.insert(tx, scope.tenant(), actor, worker_id, args)
            .await
    }
    async fn insert(
        &self,
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        actor: Actor,
        worker_id: &str,
        args: Value,
    ) -> Result<JobRef> {
        let worker = self.workers.get(worker_id)?;
        arguments::validate(&args)?;
        let args = (worker.definition.validate)(args)?;
        arguments::validate(&args)?;
        let trace = tx.context().trace_id.clone();
        tx.mutate(move |sql|Box::pin(async move {
            let (kind, actor_id, principal, person, organisation, affiliation) = match &actor {
                Actor::User { user, person, organisation, affiliation } => ("user", *user, None, *person, *organisation, *affiliation),
                Actor::System(principal) => ("system", 0, Some(principal.as_str()), None, None, None),
            };
            let row = sql.query_one("INSERT INTO base_queue_jobs(tenant_id,worker_id,worker_module,args,actor_kind,actor_id,system_principal,person_id,organisation_id,affiliation_id,trace_id,max_attempts) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12) RETURNING id", &[&tenant,&worker.id,&worker.module,&args,&kind,&actor_id,&principal,&person,&organisation,&affiliation,&trace,&worker.definition.max_attempts]).await?;
            let id = row.get("id");
            // No args, names, email or credential material enters audit evidence.
            let change = Change {kind:"base/queue.job".into(),id,old:Value::Null,new:json!({"worker":worker.id,"state":"available","actor_kind":kind,"actor_id":actor_id,"system_principal":principal,"max_attempts":worker.definition.max_attempts})};
            Ok((JobRef{id},vec![change]))
        })).await
    }
}
enum Actor {
    User {
        user: i64,
        person: Option<i64>,
        organisation: Option<i64>,
        affiliation: Option<i64>,
    },
    System(String),
}
inventory::submit! {
    Module {id:"base/queue",layer:bilibi_module_registry::Layer::Base,
        dependencies:&["base/module_registry","base/database","base/audit","base/tenancy","base/authz"],
        migrations:&[("base-queue-001",include_str!("../migrations/001-queue.sql")),("base-queue-002",include_str!("../migrations/002-queue-system-actor.sql"))],
        services:&[Service {id:"base/queue.store",factory:|_| {
            let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
            Ok(Arc::new(Queue::new(Workers::from_catalog(&catalog)?)))
        }}],
    }
}
