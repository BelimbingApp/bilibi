//! Personal data (ADR 05): the requests, legal holds, retention sweeps and
//! masked-field reveals above the `base/personal_data` contract, in which
//! every module that keeps a person's data answers for it. Data-subject
//! requests (access, portability, erasure) reach every store through one
//! approved, audited Workflow flow and end with an export bundle or a
//! completion certificate; an erasure is approved through Workflow's shared
//! approval case (`approval`, ADR 15). Base never reads another module's tables; it asks
//! each store's handler, with a sealed grant only this module issues.
mod agent;
mod approval;
mod documents;
mod due;
mod holds;
mod masking;
mod registry;
mod requests;
mod retention;
mod runner;
mod web;
pub use approval::{erasure_rules, DECIDE as ERASURE_DECIDE, ERASURE_CASE};
pub use documents::{RequestDocuments, ARTIFACT_OWNER, CERTIFICATE_YEARS, DOCUMENTS, EXPORT_DAYS};
pub use due::{DUE_SCHEDULE, DUE_WORKER, NOTICE_DAYS};
pub use holds::{Hold, HoldEvent, HoldPage, HoldTarget};
pub use requests::{
    ConfirmedRecord, NewRequest, RequestKind, RequestView, StoreProgress, DUE_DAYS, SUBJECT,
};
pub use retention::{
    period_accepts, setting_keys, ClassView, Companies, RecordClass, RetentionAction, SweepView,
    Sweeper, Swept, AUDIT_EVIDENCE, RECORD_CLASSES, SWEEP_SCHEDULE, SWEEP_WORKER,
};
pub use runner::{FINISH_WORKER, STORE_WORKER};

use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_module_registry::{
    Catalog, Consumer, Contribution, Layer, Module, Provenance, Ready, Result as RegistryResult,
    Service,
};
use bilibi_personal_data::{Issuer, Stores};
use bilibi_queue::{HandlerBinding, HandlerService, Queue, Worker, HANDLERS, WORKERS};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::sync::Arc;
use tokio_postgres::{Client, GenericClient};

/// What `record_class!` expands to; not part of the facade.
#[doc(hidden)]
pub mod __private {
    pub use crate::retention::period_accepts;
    pub use bilibi_module_registry::{Contribution, Provenance};
    pub use bilibi_settings::{
        Constraint, Definition, Editor, ScopeKind, Setting, ValueType, CONSTRAINTS, DEFINITIONS,
    };
    pub use inventory;
    pub use serde_json::{json, Value};
}

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
/// Approve requests; the request jobs run as the approver. An erasure is
/// decided through its approval case, by someone independent of it.
pub const REQUEST_APPROVE: &str = "privacy.request.approve";
/// Publish the erasure approval rules and review their exceptions.
pub const APPROVAL_MANAGE: &str = "privacy.approval.manage";
/// Read record classes, the tenant's retention periods and sweep results.
pub const RETENTION_VIEW: &str = "privacy.retention.view";
/// Set the tenant's retention periods.
pub const RETENTION_MANAGE: &str = "privacy.retention.manage";
/// See the full value of a field masked for the reader, with a reason.
pub const FIELD_REVEAL: &str = "privacy.field.reveal";
/// The Step-up operation a reveal asks for once the tenant turns it on.
pub const STEP_UP_REVEAL: &str = "base.privacy.field.reveal";
/// The Step-up operation each erasure approval decision asks for; tenants
/// cannot turn it off.
pub const STEP_UP_ERASURE: &str = "base.privacy.erasure.approve";
/// Company setting: days audit evidence keeps personal values after an
/// erasure (its legal minimum); the retention sweep erases them after.
pub const AUDIT_MINIMUM: &str = "privacy.audit.legal_minimum_days";
pub const EXPORT_PURGE_WORKER: &str = "base/privacy-export-purge";
pub const EXPORT_PURGE_SCHEDULE: &str = "base/privacy.export-purge";

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
    issuer: Issuer,
    stores: &'static Stores,
    classes: retention::Classes,
}
impl Privacy {
    /// Every mounted store key with its label, sorted by key.
    pub fn stores(&self) -> impl Iterator<Item = (&'static str, &'static str)> + '_ {
        self.stores.iter().map(|(key, _, data)| (key, data.label))
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
            HoldTarget::Record { kind, .. } if self.stores.kind(kind).is_none() => {
                Err(Error::Invalid)
            }
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
    /// One page of the tenant's holds, active first then newest: `search`
    /// matches a hold, person or record ID or a record kind; `limit` is 1–500.
    pub async fn holds(
        &self,
        client: &impl GenericClient,
        scope: &Scope,
        search: &str,
        offset: i64,
        limit: i64,
    ) -> Result<HoldPage> {
        holds::list(client, scope, search, offset, limit).await
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
inventory::submit! { Consumer::new::<RecordClass>(RECORD_CLASSES, "base/privacy", retention::validate) }
inventory::submit! { Contribution::new(VOCABULARY, "base/privacy.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["privacy"],
    verbs: &["view", "manage", "receive", "approve", "reveal"],
    capabilities: &[
        CapabilityDefinition::tenant(HOLD_VIEW), CapabilityDefinition::tenant(HOLD_MANAGE),
        CapabilityDefinition::tenant(REQUEST_VIEW), CapabilityDefinition::tenant(REQUEST_RECEIVE),
        CapabilityDefinition::tenant(REQUEST_APPROVE), CapabilityDefinition::tenant(APPROVAL_MANAGE),
        CapabilityDefinition::tenant(RETENTION_VIEW), CapabilityDefinition::tenant(RETENTION_MANAGE),
        CapabilityDefinition::tenant(FIELD_REVEAL),
    ],
    roles: &[],
    system_principals: &[],
}) }
bilibi_step_up::declare!(
    "base/privacy",
    // The owner's rule: a fresh 2FA check on every erasure decision, on every
    // deployment; no tenant, installation floor or development setting relaxes it.
    bilibi_step_up::Operation::new(STEP_UP_ERASURE, "Approve a personal-data erasure").always()
);
bilibi_step_up::declare!(
    "base/privacy",
    bilibi_step_up::Operation::new(STEP_UP_REVEAL, "Reveal a masked personal field")
        .off_by_default()
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
inventory::submit! { Contribution::new(DEFINITIONS, NOTICE_DAYS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global, ScopeKind::Tenant], default: || json!(due::NOTICE_DEFAULT),
    nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(60.0), validator: None,
    editor: Some(Editor { group: "Personal data", label: "Request notice days",
        help: "Days before a request's due date that the people handling it are notified.", capability: Some(REQUEST_APPROVE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, AUDIT_MINIMUM, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Company], default: || serde_json::Value::Null,
    nullable: true, encrypted: false, minimum: Some(1.0), maximum: Some(36500.0), validator: None,
    editor: Some(Editor { group: "Personal data retention", label: "Audit evidence: legal minimum days",
        help: "Days audit evidence keeps a person's values after an erasure, as the law requires this company. Empty erases them at once; the retention sweep erases them once the period has passed.",
        capability: Some(RETENTION_MANAGE), reveal_duration_ms: 5_000 }),
})) }
// Retention sweeps: a daily scheduled task, run only after an operator enables it.
inventory::submit! { Contribution::new(WORKERS, SWEEP_WORKER, provenance!(), &Worker { max_attempts: 3, validate: |args| bilibi_schedule::worker::validate(args, retention::arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, SWEEP_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.sweep_job", capability: bilibi_schedule::EXECUTE }) }
inventory::submit! { Contribution::new(bilibi_schedule::DEFINITIONS, SWEEP_SCHEDULE, provenance!(), &bilibi_schedule::Definition {
    name: "Personal-data retention sweep", expression: "0 2 * * *", timezone: "UTC",
    task_name: "Personal-data retention sweep", worker: SWEEP_WORKER, args: || json!({}),
    overlap: bilibi_schedule::Overlap::Forbid, misfire: bilibi_schedule::Misfire::Coalesce, owner_route: Some(web::RETENTION_PATH),
}) }
// Expired exports: a daily scheduled task, run only after an operator enables it.
inventory::submit! { Contribution::new(WORKERS, EXPORT_PURGE_WORKER, provenance!(), &Worker { max_attempts: 3, validate: |args| bilibi_schedule::worker::validate(args, retention::arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, EXPORT_PURGE_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.export_purge_job", capability: bilibi_schedule::EXECUTE }) }
inventory::submit! { Contribution::new(bilibi_schedule::DEFINITIONS, EXPORT_PURGE_SCHEDULE, provenance!(), &bilibi_schedule::Definition {
    name: "Personal-data export purge", expression: "30 3 * * *", timezone: "UTC",
    task_name: "Personal-data export purge", worker: EXPORT_PURGE_WORKER, args: || json!({}),
    overlap: bilibi_schedule::Overlap::Forbid, misfire: bilibi_schedule::Misfire::Coalesce, owner_route: Some(web::REQUESTS_PATH),
}) }
// Due notices: a daily scheduled task, run only after an operator enables it.
inventory::submit! { Contribution::new(WORKERS, DUE_WORKER, provenance!(), &Worker { max_attempts: 3, validate: |args| bilibi_schedule::worker::validate(args, due::arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, DUE_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.due_job", capability: bilibi_schedule::EXECUTE }) }
inventory::submit! { Contribution::new(bilibi_schedule::DEFINITIONS, DUE_SCHEDULE, provenance!(), &bilibi_schedule::Definition {
    name: "Personal-data request due notices", expression: "0 7 * * *", timezone: "UTC",
    task_name: "Personal-data request due notices", worker: DUE_WORKER, args: || json!({}),
    overlap: bilibi_schedule::Overlap::Forbid, misfire: bilibi_schedule::Misfire::Coalesce, owner_route: Some(web::REQUESTS_PATH),
}) }
inventory::submit! { Contribution::new(WORKERS, STORE_WORKER, provenance!(), &Worker { max_attempts: 25, validate: runner::store_arguments }) }
inventory::submit! { Contribution::new(HANDLERS, STORE_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.store_job", capability: REQUEST_APPROVE }) }
inventory::submit! { Contribution::new(WORKERS, FINISH_WORKER, provenance!(), &Worker { max_attempts: 25, validate: runner::finish_arguments }) }
inventory::submit! { Contribution::new(HANDLERS, FINISH_WORKER, provenance!(), &HandlerBinding { service: "base/privacy.finish_job", capability: REQUEST_APPROVE }) }

use bilibi_module_registry::contributions::{
    Access, Download, Method, PageWrites, Pending, Route, DOWNLOADS, PAGE_WRITES, ROUTES,
};
macro_rules! page {
    ($id:literal, $path:expr, $operation:literal, $capability:expr) => {
        inventory::submit! { Contribution::new(ROUTES, $id, provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
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
page!(
    "base/privacy.retention",
    web::RETENTION_PATH,
    "retention",
    RETENTION_VIEW
);
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
        ("submit", "base.privacy.erasure.submit"),
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
bilibi_menu::contribute!(
    "base/privacy",
    bilibi_menu::MenuItem::page("admin.privacy.retention", "Retention", web::RETENTION_PATH)
        .icon("clock")
        .under("admin.privacy")
        .requires(bilibi_menu::Requirement::Capability(RETENTION_VIEW))
        .order(30)
);

fn build(services: &bilibi_module_registry::Services) -> RegistryResult<Privacy> {
    let modules = bilibi_module_registry::discover()?;
    let catalog = Catalog::discover(&modules)?;
    let step_up = services.get::<bilibi_step_up::StepUp>(bilibi_step_up::SERVICE)?;
    let parties = services.get::<bilibi_tenancy::PartyProofs>("base/tenancy.parties")?;
    requests::bind(requests::Bound {
        parties: parties.clone(),
    })?;
    let issuer = Issuer::claim().ok_or_else(|| {
        bilibi_module_registry::RegistryError::Service(
            "the personal-data grant issuer was already claimed".into(),
        )
    })?;
    let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
    bilibi_personal_data::bind_holds(Arc::new(holds::Source { db: db.clone() }))?;
    Ok(Privacy {
        db,
        boundary: services.get("base/database.mutations")?,
        engine: services.get("base/workflow.engine")?,
        queue: services.get("base/queue.store")?,
        step_up,
        settings: services.get("base/settings.store")?,
        parties,
        artifacts: services.get("base/artifacts.store")?,
        issuer,
        stores: Stores::installed()?,
        classes: retention::Classes::from_catalog(&catalog, &modules)?,
    })
}

inventory::submit! {
    Module {
        id: "base/privacy", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/grid", "base/party_ids", "base/personal_data",
            "base/audit", "base/workflow", "base/queue", "base/step_up", "base/settings", "base/artifacts", "base/agent_api", "base/ui", "base/menu", "base/datetime", "base/locale", "base/notifications", "base/schedule"],
        migrations: &[
            ("base-privacy-001-holds", include_str!("../migrations/001-holds.sql")),
            ("base-privacy-002-requests", include_str!("../migrations/002-requests.sql")),
            ("base-privacy-003-due-notices", include_str!("../migrations/003-due-notices.sql")),
            ("base-privacy-004-retention", include_str!("../migrations/004-retention.sql")),
            ("base-privacy-005-erasure-authority", include_str!("../migrations/005-erasure-authority.sql")),
            ("base-privacy-006-erasure-approval", include_str!("../migrations/006-erasure-approval.sql")),
        ],
        services: &[
            Service { id: SERVICE, factory: |services| Ok(Arc::new(build(services)?)) },
            Service { id: "base/privacy.agent", factory: |services| {
                if !agent::bind(services.get(SERVICE)?) {
                    return Err(bilibi_module_registry::RegistryError::Service("duplicate privacy agent binding".into()));
                }
                Ok(Arc::new(()))
            } },
            Service { id: "base/privacy.approvals", factory: |services| {
                approval::bind(services.get(bilibi_workflow::SERVICE)?, services.get(SERVICE)?, services.get("base/tenancy.authentication")?)?;
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
            Service { id: "base/privacy.sweep_job", factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(retention::SweepJob {
                    privacy: services.get(SERVICE)?,
                    schedule: services.get("base/schedule.store")?,
                    authentication: services.get("base/tenancy.authentication")?,
                }))))
            } },
            Service { id: "base/privacy.export_purge_job", factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(retention::ExportPurgeJob {
                    privacy: services.get(SERVICE)?,
                    schedule: services.get("base/schedule.store")?,
                    authentication: services.get("base/tenancy.authentication")?,
                }))))
            } },
            Service { id: "base/privacy.due_job", factory: |services| {
                Ok(Arc::new(HandlerService(Arc::new(due::DueJob {
                    privacy: services.get(SERVICE)?,
                    schedule: services.get("base/schedule.store")?,
                    authentication: services.get("base/tenancy.authentication")?,
                    notifications: services.get("base/notifications.sink")?,
                }))))
            } },
        ],
    }
}
inventory::submit! {
    Ready { module: "base/privacy", check: |services| {
        let privacy = services.get::<Privacy>(SERVICE)?;
        registry::check_marks(privacy.stores)
    } }
}
