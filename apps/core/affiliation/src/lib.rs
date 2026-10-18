//! Person ↔ Organisation work affiliations, independent of People employment.
mod agent_operations;
pub mod contributions;
pub mod identity;
pub mod model;
pub mod web;
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_tenancy::{Authentication, Scope};
pub use identity::{LockedAffiliation, OwnCompanyAffiliation};
pub use model::{
    Affiliation, AffiliationId, AffiliationInput, CalendarDate, ContactFacts, EffectiveDates,
    OrganisationId, Page, PersonId, Search, Update,
};
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

pub const SERVICE: &str = "core/affiliation.affiliations";
#[derive(Clone, Debug)]
pub struct AffiliationChanged {
    pub tenant: i64,
    pub id: AffiliationId,
    pub person: PersonId,
    pub organisation: OrganisationId,
}
static CHANGES: OnceLock<broadcast::Sender<AffiliationChanged>> = OnceLock::new();
fn publish(tx: &mut MutationTransaction<'_>, tenant: i64, affiliation: &Affiliation) {
    if let Some(changes) = CHANGES.get().cloned() {
        let change = AffiliationChanged {
            tenant,
            id: affiliation.id,
            person: affiliation.affiliation.person_id,
            organisation: affiliation.affiliation.organisation_id,
        };
        tx.after_commit(move || {
            let _ = changes.send(change);
        });
    }
}

pub struct AffiliationService {
    db: Db,
    mutations: Arc<MutationBoundary>,
    authentication: Arc<Authentication>,
}
impl AffiliationService {
    pub fn subscribe(&self) -> broadcast::Receiver<AffiliationChanged> {
        CHANGES
            .get()
            .expect("AffiliationService installs change sender")
            .subscribe()
    }
    async fn fresh(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Scope> {
        // Keeps the impersonator and agent facet the edge sealed.
        self.authentication.reprove(client, scope).await
    }
    pub async fn read(&self, scope: &Scope, id: AffiliationId) -> Result<Affiliation> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        identity::read(&**client, &scope, id).await
    }
    pub async fn search(&self, scope: &Scope, query: &Search) -> Result<Page> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        identity::search(&**client, &scope, query).await
    }
    pub async fn create(&self, scope: &Scope, input: &AffiliationInput) -> Result<Affiliation> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("affiliation.create"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = identity::create(&mut tx, &scope, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn update(
        &self,
        scope: &Scope,
        id: AffiliationId,
        update: &Update,
    ) -> Result<Affiliation> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("affiliation.update"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = identity::update(&mut tx, &scope, id, update).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn end(
        &self,
        scope: &Scope,
        id: AffiliationId,
        version: i64,
        on: CalendarDate,
    ) -> Result<Affiliation> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("affiliation.end"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = identity::end(&mut tx, &scope, id, version, on).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn unlink(
        &self,
        scope: &Scope,
        id: AffiliationId,
        version: i64,
    ) -> Result<Affiliation> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("affiliation.unlink"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = identity::unlink(&mut tx, &scope, id, version).await?;
        tx.commit().await?;
        Ok(result)
    }
    /// Refresh actor eligibility and issue a proof within the consumer's outer
    /// mutation. People/Account writes keep all owner locks until commit.
    pub async fn lock_for_employment<'tx>(
        &self,
        tx: &'tx MutationTransaction<'_>,
        scope: &Scope,
        id: AffiliationId,
    ) -> Result<OwnCompanyAffiliation<'tx>> {
        let scope = self.fresh(tx.sql(), scope).await?;
        identity::lock_for_employment(tx, &scope, id).await
    }
    pub async fn lock_for_account_binding<'tx>(
        &self,
        tx: &'tx MutationTransaction<'_>,
        scope: &Scope,
        id: AffiliationId,
    ) -> Result<LockedAffiliation<'tx>> {
        let scope = self.fresh(tx.sql(), scope).await?;
        identity::lock_for_account_binding(tx, &scope, id).await
    }
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/affiliation", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/database", "base/tenancy", "base/authz", "base/grid", "base/agent_api", "base/ui", "base/datetime", "base/locale", "base/principal_directory", "core/party", "core/person", "core/company"],
        migrations: &[("001-core-affiliation", include_str!("../migrations/001-affiliation.sql"))],
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            CHANGES.set(broadcast::channel(128).0).map_err(|_| bilibi_module_registry::RegistryError::Service("duplicate affiliation change sender".into()))?;
            let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
            agent_operations::bind(db.clone(), services.get("base/tenancy.parties")?)?;
            Ok(Arc::new(AffiliationService {
                db,
                mutations: services.get("base/database.mutations")?,
                authentication: services.get("base/tenancy.authentication")?,
            }))
        }}, bilibi_module_registry::Service { id: web::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                affiliations: services.get(SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                proofs: services.get("base/tenancy.parties")?,
                directory: services.get("base/principal_directory.directory")?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
            }))))
        }}],
    }
}
