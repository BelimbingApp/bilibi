//! Core identities and personal contacts. No HR or implicit account creation.
mod agent_operations;
mod contributions;
mod directory;
mod files;
mod identity;
mod model;
mod privacy;
mod records;
mod step_up;
mod web;

use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_tenancy::{Authentication, Scope};
pub use contributions::{CAPS, CREATE, LIST, RETIRE, SYSTEM_MANAGE, UPDATE, VIEW};
pub use files::{PersonFiles, ARTIFACT_OWNER, ATTACHMENT_SUBJECT, FILES};
// Other owners' transactions: a person read, searched, previewed or created
// on the caller's connection, and the account binder's locks and standing.
pub use identity::{
    account_standing, create as create_person, lock_for_account_binding, preview as preview_person,
    read as read_person, search as search_persons, BindingIdentity,
};
pub use model::{
    validate as validate_person, DuplicateQuery, FieldError, Page, Person, PersonId, PersonInput,
    PersonKind, Search, Update,
};
use std::sync::{Arc, OnceLock};
use tokio::sync::broadcast;

pub const SERVICE: &str = "core/person.persons";
#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Backend(#[from] Error),
    #[error("Invalid contact fields")]
    Validation(Vec<FieldError>),
    #[error("This identity is retired")]
    Retired,
    #[error("The platform system identity is protected")]
    SystemIdentity,
}
#[derive(Clone, Debug)]
pub struct PersonChanged {
    pub tenant: i64,
    pub id: PersonId,
}
static CHANGES: OnceLock<broadcast::Sender<PersonChanged>> = OnceLock::new();
pub(crate) fn publish(tx: &mut MutationTransaction<'_>, tenant: i64, id: PersonId) {
    if let Some(changes) = CHANGES.get().cloned() {
        tx.after_commit(move || {
            let _ = changes.send(PersonChanged { tenant, id });
        });
    }
}

pub struct PersonService {
    db: Db,
    mutations: Arc<MutationBoundary>,
    authentication: Arc<Authentication>,
}
impl PersonService {
    pub fn subscribe(&self) -> broadcast::Receiver<PersonChanged> {
        CHANGES
            .get()
            .expect("PersonService installs change sender")
            .subscribe()
    }
    /// Refresh account/tenant eligibility without renewing idle sessions.
    /// HTTP adapters still validate their durable session on every request.
    async fn fresh(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Scope> {
        // Keeps the impersonator and agent facet the edge sealed.
        self.authentication.reprove(client, scope).await
    }
    pub async fn read(&self, scope: &Scope, id: PersonId) -> Result<Person> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        identity::read(&**client, &scope, id).await
    }
    pub async fn search(&self, scope: &Scope, query: &Search) -> Result<Page> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        identity::search(&**client, &scope, query).await
    }
    pub async fn duplicates(&self, scope: &Scope, query: &DuplicateQuery) -> Result<Vec<Person>> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        identity::duplicates(&**client, &scope, query).await
    }
    pub async fn create(
        &self,
        scope: &Scope,
        input: &PersonInput,
    ) -> std::result::Result<Person, WriteError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.fresh(&**client, scope).await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("person.create"))
            .await?;
        let person = identity::create(&mut tx, &scope, input).await?;
        tx.commit().await?;
        Ok(person)
    }
    /// Compose account provisioning in the caller's audited transaction.
    /// The same owner authorization, validation and notifications apply.
    pub async fn create_in(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        scope: &Scope,
        input: &PersonInput,
    ) -> std::result::Result<Person, WriteError> {
        let scope = self.fresh(tx.sql(), scope).await?;
        identity::create(tx, &scope, input).await
    }
    /// Create a Person for an account a system principal provisions, in the
    /// caller's audited `Context::system` transaction. No capability is checked:
    /// the caller has authorized the creation. A user context is refused.
    pub async fn provision_in(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        input: &PersonInput,
    ) -> std::result::Result<Person, WriteError> {
        identity::provision(tx, input).await
    }
    pub async fn update(
        &self,
        scope: &Scope,
        id: PersonId,
        update: &Update,
    ) -> std::result::Result<Person, WriteError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.fresh(&**client, scope).await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("person.update"))
            .await?;
        let person = identity::update(&mut tx, &scope, id, update).await?;
        tx.commit().await?;
        Ok(person)
    }
    pub async fn retire(
        &self,
        scope: &Scope,
        id: PersonId,
        version: i64,
    ) -> std::result::Result<Person, WriteError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.fresh(&**client, scope).await?;
        step_up::retire(&scope, id).await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("person.retire"))
            .await?;
        let person = identity::retire(&mut tx, &scope, id, version).await?;
        tx.commit().await?;
        Ok(person)
    }
    pub async fn ensure_platform_orchestrator(
        &self,
        scope: &Scope,
    ) -> std::result::Result<(Person, bool), WriteError> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let scope = self.fresh(&**client, scope).await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("person.system.provision"),
            )
            .await?;
        let result = identity::ensure_platform_orchestrator(&mut tx, &scope).await?;
        if result.1 {
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(result)
    }
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/person", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/audit", "base/database", "base/tenancy", "base/authz", "base/grid", "base/search", "base/agent_api", "base/principal_directory", "base/ui", "base/menu", "base/datetime", "base/locale", "base/media", "base/artifacts", "base/step_up", "base/records", "base/personal_data", "base/privacy", "core/party"],
        migrations: &[
            ("001-core-person", include_str!("../migrations/001-person.sql")),
            ("core-person-002-search", include_str!("../migrations/002-search.sql")),
            ("core-person-003-erasure", include_str!("../migrations/003-erasure.sql")),
        ],
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            let parties = services.get::<bilibi_tenancy::PartyProofs>("base/tenancy.parties")?;
            parties.bind_persons(Arc::new(identity::Provider))?;
            files::bind(parties)?;
            privacy::bind(services.get("base/media.store")?, services.get("base/artifacts.store")?)?;
            let directory = services.get::<bilibi_principal_directory::PrincipalDirectory>("base/principal_directory.directory")?;
            directory.bind("core/person", Arc::new(directory::Provider(bilibi_principal_directory::PrincipalKind::Person)))?;
            directory.bind("core/person", Arc::new(directory::Provider(bilibi_principal_directory::PrincipalKind::Agent)))?;
            CHANGES.set(broadcast::channel(128).0).map_err(|_| bilibi_module_registry::RegistryError::Service("duplicate person change sender".into()))?;
            let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
            agent_operations::bind(db.clone())?;
            step_up::bind(services.get(bilibi_step_up::SERVICE)?)?;
            Ok(Arc::new(PersonService { db, mutations: services.get("base/database.mutations")?, authentication: services.get("base/tenancy.authentication")? }))
        }}, bilibi_module_registry::Service { id: web::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                person_panels: web::person_panels()?,
                persons: services.get(SERVICE)?, contracts: services.get(bilibi_agent_api::page::SERVICE)?, views: services.get(bilibi_grid::PAGE_VIEWS)?,
                display: services.get("base/datetime.display")?, locales: services.get("base/locale.locales")?,
            }))))
        }}],
    }
}
