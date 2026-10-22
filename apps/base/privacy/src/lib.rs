//! Personal data (ADR 05): one registry in which every module that keeps a
//! person's data answers for it, and legal holds that stop erasure and sweeps.
//! Base never reads another module's tables; it asks each store's handler.
//! Data-subject requests, retention and masking build on this registry. No
//! Bilimbi counterpart: Bilimbi keeps per-store retention workers only.
mod holds;
mod registry;
pub use holds::{Hold, HoldEvent, HoldTarget, Holds};
pub use registry::{
    Action, Deferral, Erased, Exported, FileRef, Handler, HandlerFuture, Identifier, Kept,
    KeptReason, KindAction, KindCount, Located, PersonalData, Subject, ERASED, PERSONAL_DATA,
};

use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{MutationBoundary, Result};
use bilibi_module_registry::{
    Catalog, Consumer, Contribution, Layer, Module, Provenance, Ready, Result as RegistryResult,
    Service,
};
use bilibi_party_ids::PersonId;
use bilibi_tenancy::Scope;
use std::sync::Arc;
use tokio_postgres::{Client, GenericClient};

/// The `Privacy` service, after declaring `base/privacy`.
pub const SERVICE: &str = "base/privacy.facade";
/// Read the tenant's legal holds.
pub const HOLD_VIEW: &str = "privacy.hold.view";
/// Place and release legal holds.
pub const HOLD_MANAGE: &str = "privacy.hold.manage";

/// The mounted personal-data stores and the holds over them.
pub struct Privacy {
    boundary: Arc<MutationBoundary>,
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
    /// Places a legal hold. A second active hold on the same target is `Conflict`.
    pub async fn place_hold(
        &self,
        client: &mut Client,
        scope: &Scope,
        target: HoldTarget,
        reason: &str,
    ) -> Result<Hold> {
        holds::place(&self.boundary, client, scope, target, reason).await
    }
    /// Releases an active hold; the next erasure or sweep acts on what it kept.
    pub async fn release_hold(
        &self,
        client: &mut Client,
        scope: &Scope,
        id: i64,
        reason: &str,
    ) -> Result<Hold> {
        holds::release(&self.boundary, client, scope, id, reason).await
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
    verbs: &["view", "manage"],
    capabilities: &[CapabilityDefinition::tenant(HOLD_VIEW), CapabilityDefinition::tenant(HOLD_MANAGE)],
    roles: &[],
    system_principals: &[],
}) }

fn build(services: &bilibi_module_registry::Services) -> RegistryResult<Privacy> {
    let modules = bilibi_module_registry::discover()?;
    let catalog = Catalog::discover(&modules)?;
    Ok(Privacy {
        boundary: services.get("base/database.mutations")?,
        stores: registry::Stores::from_catalog(&catalog, &modules)?,
    })
}

inventory::submit! {
    Module {
        id: "base/privacy", layer: Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/grid", "base/party_ids"],
        migrations: &[("base-privacy-001-holds", include_str!("../migrations/001-holds.sql"))],
        services: &[Service { id: SERVICE, factory: |services| Ok(Arc::new(build(services)?)) }],
    }
}
inventory::submit! {
    Ready { module: "base/privacy", check: |services| {
        let privacy = services.get::<Privacy>(SERVICE)?;
        registry::check_marks(&privacy.stores)
    } }
}
