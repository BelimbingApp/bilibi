//! Owner-proven status transitions, versioned process runs, worker leases
//! and the human action gate. All callbacks share the caller's audited
//! transaction; hooks perform database effects only. Committed transitions
//! are delivered through the durable outbox.
pub mod definitions;
pub mod engine;
pub mod history;
mod intent;
pub mod maintenance;
pub mod outbox;
pub mod pending;
pub mod process;
pub mod runs;
mod seeds;
pub mod web;
use bilibi_database::{Change, Result};
use bilibi_tenancy::Scope;
pub use definitions::{
    Action, Contribution, Edge, Flow, Guard, HumanAction, HumanActionHandler, HumanOutcome,
    Registry, Status, Transition, CONTRIBUTIONS,
};
pub use engine::{Engine, Request};
pub use pending::{PendingCursor, PendingEntry, PendingPage, PendingQuery};
pub use process::{
    Dependency, DependencyMode, Process, ProcessAdapter, ProcessDefinition, RunFacts, Step,
    StepDefinition,
};
pub use runs::{
    ActionRequest, ActionResult, AvailableAction, AvailableActions, Claim, ClaimOptions, Claimed,
    Completion, Event, EventPage, Run, RunCommand, RunView, StartOptions, WorkCommand, WorkItem,
    WorkView,
};
pub use seeds::FLOW_SEEDS;
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio_postgres::Transaction;

pub type OwnerFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'a>>;
#[derive(Clone, Debug)]
pub struct Subject {
    pub id: i64,
    pub tenant: i64,
    pub status: String,
    pub version: i64,
    pub facts: Value,
}
#[derive(Clone, Copy, Debug)]
pub enum Load {
    Read,
    Lock,
}
#[derive(Clone, Copy, Debug)]
pub enum Operation {
    Read,
    Transition,
    RecordInitial,
    RecordComment,
    /// Installed owner binds an existing subject and its history, under its
    /// own explicit adoption policy; never a tenant guessed from history.
    Adopt,
    /// Process run operations, asked of both the subject and process adapters.
    /// `ReadRun` never grants execution.
    StartRun,
    ReadRun,
    ReconcileRun,
    CompleteWork,
    SignalRun,
    PauseRun,
    ResumeRun,
    SupersedeRun,
    /// Worker lease operations; a leaseholder's completion asks `CompleteWork`.
    ClaimWork,
    HeartbeatWork,
    FailWork,
    BlockClaimedWork,
    /// Owner or operator finishing unfinished work with a reason.
    WaiveWork,
    BlockWork,
    /// A signed-in person executing a human action on the locked subject;
    /// listing available actions asks `Read`. A work-bound action then asks
    /// both adapters `CompleteWork` for its run.
    ExecuteAction,
}
/// The owner facade accepts the kernel transaction, scopes every query, locks
/// and rereads on `Load::Lock`, and enforces live record policy even on edges
/// without a capability. Never open/commit another transaction in a callback.
pub trait SubjectAdapter: Send + Sync {
    /// Maintenance locks only: prove the tenant and lock the owner row using
    /// this system scope. Executes no business policy or effect. Owners opt in;
    /// the default defers their runs until they implement this seam.
    fn maintenance_load<'a>(
        &'a self,
        _tx: &'a Transaction<'_>,
        _scope: &'a bilibi_authz::SystemScope,
        _id: i64,
    ) -> OwnerFuture<'a, Subject> {
        Box::pin(async { Err(bilibi_database::Error::Invalid) })
    }
    fn load<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        id: i64,
        mode: Load,
    ) -> OwnerFuture<'a, Subject>;
    fn authorize<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        operation: Operation,
    ) -> OwnerFuture<'a, ()>;
    /// Return every owner mutation for capture. Compare against the locked
    /// status/version and increment the version; no effects outside PostgreSQL.
    fn persist<'a>(
        &'a self,
        tx: &'a Transaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        to: &'a str,
        request: &'a Request,
    ) -> OwnerFuture<'a, Vec<Change>>;
}

inventory::submit! { bilibi_module_registry::Module {
    id:"base/workflow",layer:bilibi_module_registry::Layer::Base,
    dependencies:&["base/module_registry","base/database","base/audit","base/authz","base/tenancy","base/queue","base/schedule","base/settings","base/ui","base/locale","base/datetime"],
    migrations:&[("workflow-001-status-kernel",include_str!("../migrations/001-status.sql")),
        ("workflow-002-process-graphs",include_str!("../migrations/002-process.sql")),
        ("workflow-003-human-actions",include_str!("../migrations/003-human-actions.sql")),
        ("workflow-004-outbox",include_str!("../migrations/004-outbox.sql"))],
    services:&[bilibi_module_registry::Service {id:"base/workflow.engine",factory:|services| {
        Ok(Arc::new(Engine::new(
            services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
            services.get("base/database.mutations")?, services.get("base/tenancy.authentication")?,
            Registry::from_inventory()?, services.get("base/queue.store")?,
        )))
    }}, bilibi_module_registry::Service {id:seeds::FLOW_SEEDS,factory:|services| {
        Ok(Arc::new(bilibi_database::seeds::SeedService(Arc::new(seeds::FlowSeeds {
            registry: services.get::<Engine>("base/workflow.engine")?.registry.clone(),
        }))))
    }}, bilibi_module_registry::Service {id:"base/workflow.delivery",factory:|services| {
        Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(outbox::Delivery {
            engine: services.get("base/workflow.engine")?,
        }))))
    }}, bilibi_module_registry::Service {id:"base/workflow.maintenance",factory:|services| {
        Ok(Arc::new(bilibi_queue::execution::HandlerService(Arc::new(maintenance::Maintenance {
            engine: services.get("base/workflow.engine")?, schedule: services.get("base/schedule.store")?,
            settings: services.get("base/settings.store")?,
        }))))
    }}, bilibi_module_registry::Service {id:"base/workflow.pages",factory:|services| {
        Ok(Arc::new(bilibi_ui::page::PageService::<Scope, bilibi_database::Error>(Arc::new(web::Pages {
            engine: services.get("base/workflow.engine")?,
            db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
            display: services.get("base/datetime.display")?, locales: services.get("base/locale.locales")?,
        }))))
    }}],
} }

inventory::submit! {
    bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::ROUTES, "base/workflow.reference",
        bilibi_module_registry::Provenance {module:"base/workflow",source:file!(),line:line!()},
        &bilibi_module_registry::contributions::Route { method:bilibi_module_registry::contributions::Method::Get, path:"/workflow/reference/{id}",
            service:"base/workflow.pages", operation:"reference", access:bilibi_module_registry::contributions::Access::Capability(web::APPROVE), framed:true })
}

inventory::submit! { bilibi_module_registry::Contribution::new(
bilibi_authz::VOCABULARY,"workflow.vocabulary",bilibi_module_registry::Provenance {module:"base/workflow",source:file!(),line:line!()},
&bilibi_authz::VocabularyContribution {domains:&["workflow"],verbs:&["approve"],
    capabilities:&[bilibi_authz::CapabilityDefinition::platform("admin.workflow.definition.manage"),
        bilibi_authz::CapabilityDefinition::tenant(web::APPROVE)],
    roles:&[],system_principals:&[]}) }

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/workflow.pages", bilibi_module_registry::Provenance { module: "base/workflow", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/workflow.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "The workflow reference page drives the Workflow engine directly; no contract yet." },
}) }
