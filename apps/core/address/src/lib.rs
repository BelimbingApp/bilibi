//! Tenant-owned postal addresses, their Geonames-normalized location and the
//! links that attach them to Organisations and Persons. See `README.md`.
pub mod bootstrap;
pub mod contributions;
pub mod location;
pub mod model;
pub mod store;
pub mod web;
use bilibi_database::{Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_geonames::Geonames;
use bilibi_locale::Bootstrap;
use bilibi_tenancy::{Authentication, Scope};
pub use location::{Auto, Location, Options};
pub use model::{
    Address, AddressId, AddressInput, AddressKind, AttachedAddress, Attachment, AttachmentId,
    AttachmentInput, AttachmentUpdate, CalendarDate, Detail, Direction, EffectiveDates, FieldError,
    LinkedOwner, OrganisationId, Page, PartyId, PersonId, Search, Sort, Update, WriteError,
    WriteResult,
};
use std::sync::{Arc, OnceLock};
pub use store::ValidAddress;
use tokio::sync::broadcast;

pub const SERVICE: &str = "core/address.addresses";
pub const MIGRATION: (&str, &str) = (
    "core-address-001",
    include_str!("../migrations/001-address.sql"),
);

/// Published after commit: the address and the owners whose panels show it.
/// Subscribers filter by tenant and reload through the authorized API.
#[derive(Clone, Debug)]
pub struct AddressChanged {
    pub tenant: i64,
    pub address: AddressId,
    pub owners: Vec<PartyId>,
}
static CHANGES: OnceLock<broadcast::Sender<AddressChanged>> = OnceLock::new();
pub(crate) fn publish(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    address: AddressId,
    owners: &[PartyId],
) {
    if let Some(changes) = CHANGES.get().cloned() {
        let change = AddressChanged {
            tenant,
            address,
            owners: owners.to_vec(),
        };
        tx.after_commit(move || {
            let _ = changes.send(change);
        });
    }
}

pub struct AddressService {
    db: Db,
    mutations: Arc<MutationBoundary>,
    authentication: Arc<Authentication>,
    geonames: Arc<Geonames>,
}
impl AddressService {
    pub fn subscribe(&self) -> broadcast::Receiver<AddressChanged> {
        CHANGES
            .get()
            .expect("AddressService installs change sender")
            .subscribe()
    }
    pub fn geonames(&self) -> &Geonames {
        &self.geonames
    }
    async fn fresh(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Scope> {
        // Keeps the impersonator and agent facet the edge sealed.
        self.authentication.reprove(client, scope).await
    }
    /// Normalize and check the location against Geonames: the only way to a
    /// `ValidAddress`, which every store write takes.
    pub async fn validate(&self, input: &AddressInput) -> WriteResult<ValidAddress> {
        let input = model::normalize(input).map_err(WriteError::Invalid)?;
        location::check(&self.geonames, &input).await?;
        Ok(ValidAddress(input))
    }
    /// The location cascade a form runs as values change (see `location`).
    pub async fn suggest(
        &self,
        incoming: &Location,
        previous: &Location,
        previous_auto: Auto,
    ) -> Result<(Location, Auto)> {
        location::suggest(&self.geonames, incoming, previous, previous_auto).await
    }
    pub async fn options(&self, location: &Location) -> Result<Options> {
        location::options(&self.geonames, location).await
    }

    pub async fn read(&self, scope: &Scope, id: AddressId) -> Result<Address> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        store::read(&**client, &scope, id).await
    }
    pub async fn detail(&self, scope: &Scope, id: AddressId) -> Result<Detail> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        let address = store::read(&**client, &scope, id).await?;
        let owners = store::owners(&**client, &scope, id).await?;
        let country_name = match address.address.country_iso.as_deref() {
            Some(iso) => self.geonames.get_country(iso).await?.map(|c| c.country),
            None => None,
        };
        let admin1_name = match address.address.admin1_code.as_deref() {
            Some(code) => self.geonames.get_admin1(code).await?.map(|a| a.name),
            None => None,
        };
        Ok(Detail {
            address,
            country_name,
            admin1_name,
            owners,
        })
    }
    pub async fn search(&self, scope: &Scope, query: &Search) -> Result<Page> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        store::search(&**client, &scope, query).await
    }
    pub async fn attached(&self, scope: &Scope, owner: PartyId) -> Result<Vec<AttachedAddress>> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        store::attached(&**client, &scope, owner).await
    }
    pub async fn primary(&self, scope: &Scope, owner: PartyId) -> Result<Option<AttachedAddress>> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        store::primary(&**client, &scope, owner).await
    }
    pub async fn available(
        &self,
        scope: &Scope,
        owner: PartyId,
        q: &Option<String>,
    ) -> Result<Vec<Address>> {
        let client = self.db.get().await?;
        let scope = self.fresh(&**client, scope).await?;
        store::available(&**client, &scope, owner, q).await
    }
    /// Installation-locale facts for `Locales::resolve` (see `bootstrap`).
    pub async fn platform_operator_bootstrap(&self) -> Result<Option<Bootstrap>> {
        let client = self.db.get().await?;
        bootstrap::platform_operator(&**client, &self.geonames).await
    }

    pub async fn create(&self, scope: &Scope, input: &AddressInput) -> WriteResult<Address> {
        let input = self.validate(input).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("address.create"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = store::create(&mut tx, &scope, &input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn update(
        &self,
        scope: &Scope,
        id: AddressId,
        update: &Update,
    ) -> WriteResult<Address> {
        let input = self.validate(&update.address).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("address.update"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = store::update(&mut tx, &scope, id, update.version, &input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn delete(&self, scope: &Scope, id: AddressId, version: i64) -> WriteResult<()> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("address.delete"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        store::delete(&mut tx, &scope, id, version).await?;
        tx.commit().await?;
        Ok(())
    }
    pub async fn attach(
        &self,
        scope: &Scope,
        address: AddressId,
        owner: PartyId,
        input: &AttachmentInput,
    ) -> WriteResult<Attachment> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("address.attach"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = store::attach(&mut tx, &scope, address, owner, input).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn create_and_attach(
        &self,
        scope: &Scope,
        owner: PartyId,
        input: &AddressInput,
        attachment: &AttachmentInput,
    ) -> WriteResult<AttachedAddress> {
        let input = self.validate(input).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("address.create_and_attach"),
            )
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = store::create_and_attach(&mut tx, &scope, owner, &input, attachment).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn update_attachment(
        &self,
        scope: &Scope,
        id: AttachmentId,
        update: &AttachmentUpdate,
    ) -> WriteResult<Attachment> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("address.update_attachment"),
            )
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        let result = store::update_attachment(&mut tx, &scope, id, update).await?;
        tx.commit().await?;
        Ok(result)
    }
    pub async fn detach(&self, scope: &Scope, id: AttachmentId, version: i64) -> WriteResult<()> {
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("address.detach"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        store::detach(&mut tx, &scope, id, version).await?;
        tx.commit().await?;
        Ok(())
    }
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/address", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/database", "base/tenancy", "base/authz", "base/grid", "base/locale", "base/ui", "base/menu", "core/party", "core/company", "core/person", "core/geonames"],
        migrations: &[MIGRATION],
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            CHANGES.set(broadcast::channel(128).0).map_err(|_| bilibi_module_registry::RegistryError::Service("duplicate address change sender".into()))?;
            let addresses = Arc::new(AddressService {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                mutations: services.get("base/database.mutations")?,
                authentication: services.get("base/tenancy.authentication")?,
                geonames: services.get(bilibi_geonames::SERVICE)?,
            });
            // Address owns the operator's address facts, so it supplies Locale's
            // installation bootstrap (K15) without Base naming Core.
            services.get::<bilibi_locale::Locales>("base/locale.locales")?
                .bind_bootstrap(Arc::new(bootstrap::Source(addresses.clone())))?;
            Ok(addresses)
        }}, bilibi_module_registry::Service { id: web::SERVICE, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, Error>(Arc::new(web::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                addresses: services.get(SERVICE)?,
            }))))
        }}],
    }
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "core/address.pages", bilibi_module_registry::Provenance { module: "core/address", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "core/address.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Addresses write through the Address facade; no core.address contract yet." },
}) }
