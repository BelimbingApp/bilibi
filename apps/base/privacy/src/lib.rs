//! Personal data (ADR 05): one registry in which every module that keeps a
//! person's data answers for it, legal holds that stop erasure and sweeps,
//! and data-subject requests (access, portability, erasure) that reach every
//! store through one approved, audited Workflow flow and end with an export
//! bundle or a completion certificate. Base never reads another module's
//! tables; it asks each store's handler. No Bilimbi counterpart: Bilimbi keeps
//! per-store retention workers only.
mod agent;
mod documents;
mod holds;
mod registry;
mod requests;
mod runner;
mod web;
pub use documents::{RequestDocuments, ARTIFACT_OWNER, CERTIFICATE_YEARS, DOCUMENTS, EXPORT_DAYS};
pub use holds::{Hold, HoldEvent, HoldTarget, Holds};
pub use registry::{
    Action, Deferral, Erased, ErasedRecord, Exported, FileRef, Handler, HandlerFuture, Identifier,
    Kept, KeptReason, KindAction, KindCount, Located, PersonalData, Subject, ERASED, PERSONAL_DATA,
};
pub use requests::{NewRequest, RequestKind, RequestView, StoreProgress, DUE_DAYS};
pub use runner::{FINISH_WORKER, STORE_WORKER};

use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_module_registry::{
    Catalog, Consumer, Contribution, Layer, Module, Provenance, Ready, Result as RegistryResult,
    Service,
};
use bilibi_party_ids::PersonId;
use bilibi_queue::{
    execution::{HandlerBinding, HandlerService, HANDLERS},
    Queue, Worker, WORKERS,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::sync::Arc;
use tokio_postgres::{Client, GenericClient};

/// The `Privacy` service, after declaring `base/privacy`.
pub const SERVICE: &str = "base/privacy.facade";
/// Read the tenant's legal holds.
pub const HOLD_VIEW: &str = "privacy.hold.view";
/// Place and release legal holds.
pub const HOLD_MANAGE: &str = "privacy.hold.manage";
/// Read data-subject requests, their progress and their documents.
pub const REQUEST_VIEW: &str = "privacy.request.view";
/// Enter, verify and refuse requests.
pub const REQUEST_RECEIVE: &str = "privacy.request.receive";
/// Approve requests; the request jobs run as the approver.
pub const REQUEST_APPROVE: &str = "privacy.request.approve";
/// The Step-up operation an erasure approval asks for; tenants cannot turn it off.
pub const STEP_UP_ERASURE: &str = "base.privacy.erasure.approve";

/// The mounted personal-data stores, the holds over them and the requests
/// that reach them.
pub struct Privacy {
    db: Db,
    boundary: Arc<MutationBoundary>,
    engine: Arc<bilibi_workflow::Engine>,
    queue: Arc<Queue>,
    step_up: Arc<bilibi_step_up::StepUp>,
    settings: Arc<bilibi_settings::Settings>,
    parties: Arc<bilibi_tenancy::PartyProofs>,
    artifacts: Arc<bilibi_artifacts::Artifacts>,
    stores: registry::Stores,
}
impl Privacy {
    /// Every mounted store key with its label, sorted by key.
    pub fn stores(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.stores
            .by_key
            .iter()
            .map(|(key, (_, data))| (*key, data.label))
    }
    /// Places a legal hold. A second active hold on the same target is `Conflict`;
    /// a record kind no mounted store declares is `Invalid`, since a hold on it
    /// would protect nothing.
    pub async fn place_hold(
        &self,
        client: &mut Client,
        scope: &Scope,
        target: HoldTarget,
        reason: &str,
    ) -> Result<Hold> {
        self.known_target(&target)?;
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.hold.place"))
            .await?;
        let hold = holds::place_in(&mut tx, scope, target, reason).await?;
        tx.commit().await?;
        Ok(hold)
    }
    /// [`Privacy::place_hold`] in the caller's transaction (the agent contract).
    pub async fn place_hold_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        target: HoldTarget,
        reason: &str,
    ) -> Result<Hold> {
        self.known_target(&target)?;
        holds::place_in(tx, scope, target, reason).await
    }
    fn known_target(&self, target: &HoldTarget) -> Result<()> {
        match target {
            HoldTarget::Record { kind, .. } if !self.stores.declares(kind) => Err(Error::Invalid),
            _ => Ok(()),
        }
    }
    /// Releases an active hold; the next erasure or sweep acts on what it kept.
    pub async fn release_hold(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
        reason: &str,
    ) -> Result<Hold> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("privacy.hold.release"))
            .await?;
        let hold = holds::release_in(&mut tx, scope, id, reason).await?;
        tx.commit().await?;
        Ok(hold)
    }
    /// [`Privacy::release_hold`] in the caller's transaction (the agent contract).
    pub async fn release_hold_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        reason: &str,
    ) -> Result<Hold> {
        holds::release_in(tx, scope, id, reason).await
    }
    /// The tenant's holds, active first then newest, at most `limit` (1–500).
    pub async fn holds(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        limit: i64,
    ) -> Result<Vec<Hold>> {
        holds::list(client, scope, limit).await
    }
    /// One hold's placed and released events, oldest first.
    pub async fn hold_history(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        id: i64,
    ) -> Result<Vec<HoldEvent>> {
        holds::history(client, scope, id).await
    }
    /// The active holds over `person` and every held record in the tenant, for
    /// an erasure or sweep. Read it inside the transaction that acts, so a hold
    /// placed meanwhile waits for that transaction.
    pub async fn active_holds(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        person: Option<PersonId>,
    ) -> Result<Holds> {
        bilibi_authz::authorize_key(client, scope, HOLD_VIEW).await?;
        Holds::load(client, scope.tenant(), person).await
    }
}

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/privacy",
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! { Consumer::new::<PersonalData>(PERSONAL_DATA, "base/privacy", registry::validate) }
inventory::submit! { Contribution::new(VOCABULARY, "base/privacy.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["privacy"],
    verbs: &["view", "manage", "receive", "approve"],
    capabilities: &[
        CapabilityDefinition::tenant(HOLD_VIEW), CapabilityDefinition::tenant(HOLD_MANAGE),
        CapabilityDefinition::tenant(REQUEST_VIEW), CapabilityDefinition::tenant(REQUEST_RECEIVE),
        CapabilityDefinition::tenant(REQUEST_APPROVE),
    ],
    roles: &[],
    system_principals: &[],
}) }
bilibi_step_up::declare!(
    "base/privacy",
    bilibi_step_up::Operation::new(STEP_UP_ERASURE, "Approve a personal-data erasure").floor()
);
inventory::submit! { Contribution::new(DEFINITIONS, DUE_DAYS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global, ScopeKind::Tenant], default: || json!(requests::DUE_DEFAULT),
    nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(365.0), validator: None,
    editor: Some(Editor { group: "Personal data", label: "Request due days",
        help: "Days from receipt to answer a data-subject request (PDPA access requests: 21).", capability: Some(REQUEST_APPROVE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, EXPORT_DAYS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global, ScopeKind::Tenant], default: || json!(documents::EXPORT_DEFAULT),
    nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(365.0), validator: None,
    editor: Some(Editor { group: "Personal data", label: "Export download days",
        help: "Days an access request's export stays downloadable before it is purged.", capability: Some(REQUEST_APPROVE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, CERTIFICATE_YEARS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global, ScopeKind::Tenant], default: || json!(documents::CERTIFICATE_DEFAULT),
    nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(100.0), validator: None,
    editor: Some(Editor { group: "Personal data", label: "Erasure certificate years",
        help: "Years an erasure completion certificate is kept as a retained record.", capability: Some(REQUEST_APPROVE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(WORKERS, STORE_WORKER, provenance!(), &Worker { max_attempts: 25, validate: runner::store_arguments }) }
inventory::submit! { Contribution::new(HANDLERS, STORE_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.store_job", capability: REQUEST_APPROVE }) }
inventory::submit! { Contribution::new(WORKERS, FINISH_WORKER, provenance!(), &Worker { max_attempts: 25, validate: runner::finish_arguments }) }
inventory::submit! { Contribution::new(HANDLERS, FINISH_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.finish_job", capability: REQUEST_APPROVE }) }

use bilibi_module_registry::contributions::{
    Access, Download, Method, PageWrites, Pending, Route, DOWNLOADS, PAGE_WRITES, ROUTES,
};
macro_rules! page {
    ($id:literal, $path:expr, $operation:literal, $capability:expr) => {
        inventory::submit! { Contribution::new(ROUTES, $id, provenance!(), &Route {
            method: Method::Get, path: $path, service: web::SERVICE, operation: $operation,
            access: Access::Capability($capability), framed: true,
        }) }
    };
}
page!(
    "base/privacy.requests",
    web::REQUESTS_PATH,
    "requests",
    REQUEST_VIEW
);
page!(
    "base/privacy.request",
    "/privacy/requests/{id}",
    "request",
    REQUEST_VIEW
);
page!("base/privacy.holds", web::HOLDS_PATH, "holds", HOLD_VIEW);
inventory::submit! { Contribution::new(DOWNLOADS, "base/privacy.request.document", provenance!(), &Download {
    path: "/privacy/requests/{id}/document", service: web::DOWNLOADS, operation: "document", access: Access::Capability(REQUEST_VIEW),
}) }
// Every write on these pages is the module's agent contract (ADR 01).
inventory::submit! { Contribution::new(PAGE_WRITES, web::SERVICE, provenance!(), &PageWrites {
    service: web::SERVICE,
    contracts: &[
        ("create", "base.privacy.request.receive"),
        ("verify", "base.privacy.request.verify"),
        ("approve", "base.privacy.request.approve"),
        ("refuse", "base.privacy.request.refuse"),
        ("place", "base.privacy.hold.place"),
        ("release", "base.privacy.hold.release"),
    ],
    pending: Pending::None,
}) }
bilibi_menu::contribute!(
    "base/privacy",
    bilibi_menu::MenuItem::container("admin.privacy", "Personal data")
        .icon("shield-check")
        .under("admin")
        .order(75)
);
bilibi_menu::contribute!(
    "base/privacy",
    bilibi_menu::MenuItem::page("admin.privacy.requests", "Requests", web::REQUESTS_PATH)
        .icon("inbox-arrow-down")
        .under("admin.privacy")
        .requires(bilibi_menu::Requirement::Capability(REQUEST_VIEW))
        .order(10)
);
bilibi_menu::contribute!(
    "base/privacy",
    bilibi_menu::MenuItem::page("admin.privacy.holds", "Legal holds", web::HOLDS_PATH)
        .icon("lock-closed")
        .under("admin.privacy")
        .requires(bilibi_menu::Requirement::Capability(HOLD_VIEW))
        .order(20)
);

fn build(services: &bilibi_module_registry::Services) -> RegistryResult<Privacy> {
    let modules = bilibi_module_registry::discover()?;
    let catalog = Catalog::discover(&modules)?;
    let step_up = services.get::<bilibi_step_up::StepUp>(bilibi_step_up::SERVICE)?;
    let parties = services.get::<bilibi_tenancy::PartyProofs>("base/tenancy.parties")?;
    requests::bind(requests::Bound {
        step_up: step_up.clone(),
        parties: parties.clone(),
    })?;
    Ok(Privacy {
        db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
        boundary: services.get("base/database.mutations")?,
        engine: services.get("base/workflow.engine")?,
        queue: services.get("base/queue.store")?,
        step_up,
        settings: services.get("base/settings.store")?,
        parties,
        artifacts: services.get("base/artifacts.store")?,
        stores: registry::Stores::from_catalog(&catalog, &modules)?,
    })
}

inventory::submit! {
    Module {
        id: "base/privacy", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/grid", "base/party_ids",
            "base/audit", "base/workflow", "base/queue", "base/step_up", "base/settings", "base/artifacts", "base/agent_api", "base/ui", "base/menu", "base/datetime", "base/locale"],
        migrations: &[
            ("base-privacy-001-holds", include_str!("../migrations/001-holds.sql")),
            ("base-privacy-002-requests", include_str!("../migrations/002-requests.sql")),
        ],
        services: &[
            Service { id: SERVICE, factory: |services| Ok(Arc::new(build(services)?)) },
            Service { id: "base/privacy.agent", factory: |services| {
                if !agent::bind(services.get(SERVICE)?) {
                    return Err(bilibi_module_registry::RegistryError::Service("duplicate privacy agent binding".into()));
                }
                Ok(Arc::new(()))
            } },
            Service { id: web::SERVICE, factory: |services| {
                Ok(Arc::new(bilibi_ui::page::PageService::<Scope, bilibi_database::Error>(Arc::new(web::Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    privacy: services.get(SERVICE)?,
                    contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                    parties: services.get("base/tenancy.parties")?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: web::DOWNLOADS, factory: |services| {
                Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, bilibi_database::Error>(Arc::new(web::Documents {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    privacy: services.get(SERVICE)?,
                }))))
            } },
            Service { id: "base/privacy.store_job", factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(runner::StoreJob { privacy: services.get(SERVICE)? }))))
            } },
            Service { id: "base/privacy.finish_job", factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(runner::FinishJob { privacy: services.get(SERVICE)? }))))
            } },
        ],
    }
}
inventory::submit! {
    Ready { module: "base/privacy", check: |services| {
        let privacy = services.get::<Privacy>(SERVICE)?;
        registry::check_marks(&privacy.stores)
    } }
}
