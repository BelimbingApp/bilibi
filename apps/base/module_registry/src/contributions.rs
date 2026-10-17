//! Immutable, eagerly validated catalogs. Consumers own their payload type and
//! validation; contributors carry source provenance and never run boot-time IO.
use crate::{Module, RegistryError, Result};
use std::{
    any::{Any, TypeId},
    collections::{BTreeMap, BTreeSet},
};

#[derive(Clone, Copy, Debug)]
pub struct Provenance {
    pub module: &'static str,
    pub source: &'static str,
    pub line: u32,
}

pub struct Consumer {
    pub id: &'static str,
    pub module: &'static str,
    payload_type: fn() -> TypeId,
    pub validate: fn(&[Entry]) -> Result<()>,
}
impl Consumer {
    pub const fn new<T: Any>(
        id: &'static str,
        module: &'static str,
        validate: fn(&[Entry]) -> Result<()>,
    ) -> Self {
        Self {
            id,
            module,
            payload_type: TypeId::of::<T>,
            validate,
        }
    }
}
inventory::collect!(Consumer);

pub struct Contribution {
    pub consumer: &'static str,
    pub id: &'static str,
    pub provenance: Provenance,
    payload: &'static (dyn Any + Send + Sync),
}
impl Contribution {
    /// Typed payload read after discovery validated the catalog at boot.
    pub fn get<T: Any>(&self) -> Result<&'static T> {
        self.payload.downcast_ref().ok_or_else(|| {
            RegistryError::Contribution(format!(
                "wrong payload type for {} at {}:{}",
                self.id, self.provenance.source, self.provenance.line
            ))
        })
    }
    pub const fn new<T: Any + Send + Sync>(
        consumer: &'static str,
        id: &'static str,
        provenance: Provenance,
        payload: &'static T,
    ) -> Self {
        Self {
            consumer,
            id,
            provenance,
            payload,
        }
    }
}
inventory::collect!(Contribution);

#[derive(Clone, Copy)]
pub struct Entry {
    pub id: &'static str,
    pub provenance: Provenance,
    payload: &'static (dyn Any + Send + Sync),
}
impl Entry {
    pub fn get<T: Any>(&self) -> Result<&'static T> {
        self.payload.downcast_ref().ok_or_else(|| {
            RegistryError::Contribution(format!(
                "wrong payload type for {} at {}:{}",
                self.id, self.provenance.source, self.provenance.line
            ))
        })
    }
}

pub struct Catalog {
    entries: BTreeMap<&'static str, Vec<Entry>>,
    types: BTreeMap<&'static str, TypeId>,
}
impl Catalog {
    pub fn discover(modules: &[&Module]) -> Result<Self> {
        Self::build(
            modules,
            inventory::iter::<Consumer>.into_iter().collect(),
            inventory::iter::<Contribution>.into_iter().collect(),
        )
    }
    /// Also supports explicit deployment catalogs without changing inventory.
    pub fn build(
        modules: &[&Module],
        consumers: Vec<&Consumer>,
        contributions: Vec<&Contribution>,
    ) -> Result<Self> {
        let owners: BTreeMap<_, _> = modules.iter().map(|m| (m.id, *m)).collect();
        let mut entries = BTreeMap::new();
        let mut types = BTreeMap::new();
        let mut validators = BTreeMap::new();
        for consumer in consumers {
            if !owners.contains_key(consumer.module)
                || !consumer.id.starts_with(&format!("{}.", consumer.module))
                || entries.insert(consumer.id, Vec::new()).is_some()
            {
                return Err(invalid(format!(
                    "unknown owner or duplicate/invalid consumer {}",
                    consumer.id
                )));
            }
            types.insert(consumer.id, (consumer.payload_type)());
            validators.insert(consumer.id, consumer.validate);
        }
        let mut keys = BTreeSet::new();
        for contribution in contributions {
            let owner = owners.get(contribution.provenance.module).ok_or_else(|| {
                invalid(format!(
                    "unknown contributor {}",
                    contribution.provenance.module
                ))
            })?;
            if contribution.id.is_empty()
                || contribution.provenance.source.is_empty()
                || contribution.provenance.line == 0
                || !keys.insert((contribution.consumer, contribution.id))
            {
                return Err(invalid(format!(
                    "invalid/duplicate {} in {} at {}:{}",
                    contribution.id,
                    contribution.consumer,
                    contribution.provenance.source,
                    contribution.provenance.line
                )));
            }
            let expected = types.get(contribution.consumer).ok_or_else(|| {
                invalid(format!(
                    "unknown consumer {} from {}",
                    contribution.consumer, owner.id
                ))
            })?;
            if *expected != contribution.payload.type_id() {
                return Err(invalid(format!(
                    "wrong payload type for {} in {}",
                    contribution.id, contribution.consumer
                )));
            }
            // Catalogs are data flowing down into Base, never Base importing Core.
            entries
                .get_mut(contribution.consumer)
                .expect("known consumer")
                .push(Entry {
                    id: contribution.id,
                    provenance: contribution.provenance,
                    payload: contribution.payload,
                });
        }
        for (consumer, items) in &mut entries {
            items.sort_by_key(|entry| (entry.provenance.module, entry.id));
            validators[consumer](items)?;
        }
        let catalog = Self { entries, types };
        catalog.validate_references(modules)?;
        Ok(catalog)
    }
    pub fn entries<T: Any>(&self, consumer: &str) -> Result<&[Entry]> {
        if self.types.get(consumer) != Some(&TypeId::of::<T>()) {
            return Err(invalid(format!(
                "unknown consumer or wrong catalog type {consumer}"
            )));
        }
        Ok(&self.entries[consumer])
    }
    fn validate_references(&self, modules: &[&Module]) -> Result<()> {
        let services: BTreeMap<_, _> = modules
            .iter()
            .flat_map(|m| m.services.iter().map(move |s| (s.id, m.id)))
            .collect();
        let check = |entry: &Entry, service: &str| -> Result<()> {
            let owner = services.get(service).ok_or_else(|| {
                invalid(format!("{} references unknown service {service}", entry.id))
            })?;
            let module = modules
                .iter()
                .find(|m| m.id == entry.provenance.module)
                .expect("validated owner");
            if *owner != module.id && !module.dependencies.contains(owner) {
                return Err(invalid(format!(
                    "{} references undeclared service dependency {service}",
                    entry.id
                )));
            }
            Ok(())
        };
        for entry in self.entries.get(ROUTES).into_iter().flatten() {
            check(entry, entry.get::<Route>()?.service)?;
        }
        for entry in self.entries.get(EMBEDS).into_iter().flatten() {
            check(entry, entry.get::<Embed>()?.service)?;
        }
        for entry in self.entries.get(DOWNLOADS).into_iter().flatten() {
            check(entry, entry.get::<Download>()?.service)?;
        }
        let mut versions = BTreeSet::new();
        for entry in self.entries.get(SEEDS).into_iter().flatten() {
            let seed = entry.get::<Seed>()?;
            if seed.version.is_empty() || !versions.insert(seed.version) {
                return Err(invalid(format!(
                    "duplicate/empty seed version {}",
                    seed.version
                )));
            }
            match seed.action {
                SeedAction::Service { service, operation } => {
                    check(entry, service)?;
                    if operation.is_empty() {
                        return Err(invalid("empty seed operation".into()));
                    }
                }
                SeedAction::Sql(sql) if sql.trim().is_empty() => {
                    return Err(invalid("empty seed SQL".into()))
                }
                SeedAction::Sql(_) => {}
            }
        }
        Ok(())
    }
}
fn invalid(message: String) -> RegistryError {
    RegistryError::Contribution(message)
}

pub const ROUTES: &str = "base/module_registry.routes";
pub const EMBEDS: &str = "base/module_registry.embeds";
pub const DOWNLOADS: &str = "base/module_registry.downloads";
pub const SEEDS: &str = "base/module_registry.seeds";
pub const MOVED: &str = "base/module_registry.moved";
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}
#[derive(Clone, Copy, Debug)]
pub enum Access {
    Public,
    SignedIn,
    Capability(&'static str),
    Operator,
    /// Both requirements apply; an operator identity is not a capability grant.
    OperatorCapability(&'static str),
    /// The installation owner only (`bilibi_tenancy::prove_owner`): the
    /// bootstrap account in the operator tenant, in its own browser session.
    /// No role, capability or grant extends it to anyone else.
    Owner,
    /// The inner access, then step-up ("sudo mode"): outside a development
    /// deployment the session must have passed an authenticator check
    /// recently (Core User's window), or a GET is sent to confirm access
    /// first. Wrap a signed-in access, never `Public` or another `StepUp`;
    /// Web refuses either at boot.
    StepUp(&'static Access),
}
impl Access {
    /// The access without its step-up wrapper: what menus and embeds check,
    /// since step-up gates opening a page, not seeing that it exists.
    pub fn base(self) -> Access {
        match self {
            Access::StepUp(inner) => *inner,
            access => access,
        }
    }
    pub fn steps_up(self) -> bool {
        matches!(self, Access::StepUp(_))
    }
}
/// Metadata consumed by the Web adapter; registration does not bypass guards.
#[derive(Clone, Copy, Debug)]
pub struct Route {
    pub method: Method,
    pub path: &'static str,
    pub service: &'static str,
    pub operation: &'static str,
    pub access: Access,
    pub framed: bool,
}
/// A GET that answers with a file instead of a page: Bilimbi's download and
/// export controllers (`/finance/attachments/{id}/download`, a return's CSV).
/// The Web adapter establishes `access` again on every request and then calls
/// the owner's `bilibi_ui::download::Download` service, whose facade authorizes
/// once more, so a grant revoked after the link was rendered refuses the next
/// click. A download is never public (anonymous bytes are Media's signed
/// stream), never framed and never live. Register one with
/// `Contribution::new(DOWNLOADS, ...)`, not a `Route` whose page writes bytes.
#[derive(Clone, Copy, Debug)]
pub struct Download {
    pub path: &'static str,
    pub service: &'static str,
    pub operation: &'static str,
    pub access: Access,
}
/// A retired page path that answers a GET with a permanent redirect to the
/// page that replaced it, so bookmarks, pins and saved workspace tiles keep
/// working after a rename. `to` names only parameters `from` has, in the same
/// segments' values; the query string is kept. Web binds it at boot.
#[derive(Clone, Copy, Debug)]
pub struct Moved {
    pub from: &'static str,
    pub to: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub enum EmbedContext {
    Shell,
    Dashboard,
    Record { kind: &'static str },
}
#[derive(Clone, Copy, Debug)]
pub struct Embed {
    pub slot: &'static str,
    pub context: EmbedContext,
    pub service: &'static str,
    pub operation: &'static str,
    pub access: Access,
}
/// `Production` seeds run once per version through the durable ledger;
/// `Development` seeds are idempotent sample data that only the development
/// seed (`bilibi-web seed`) runs, Bilimbi's module `dev_seed` scripts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeedMode {
    Production,
    Development,
}
#[derive(Clone, Copy, Debug)]
pub enum SeedAction {
    Sql(&'static str),
    Service {
        service: &'static str,
        operation: &'static str,
    },
}
/// Execution and the durable seed ledger belong to Database, not the registry.
#[derive(Clone, Copy, Debug)]
pub struct Seed {
    pub version: &'static str,
    pub mode: SeedMode,
    pub action: SeedAction,
}
fn access_valid(access: Access) -> bool {
    !matches!(
        access,
        Access::Capability("") | Access::OperatorCapability("")
    )
}
fn path_valid(path: &str, operation: &str, access: Access) -> bool {
    path.starts_with('/')
        && !path.contains(['?', '#'])
        && !operation.is_empty()
        && access_valid(access)
}
/// Parameter names do not distinguish route patterns.
fn normalized(path: &str) -> String {
    path.split('/')
        .map(|part| {
            if part.starts_with('{') && part.ends_with('}') {
                "{}"
            } else {
                part
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}
fn validate_routes(entries: &[Entry]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let route = entry.get::<Route>()?;
        if !path_valid(route.path, route.operation, route.access) {
            return Err(invalid(format!("invalid route {}", entry.id)));
        }
        if !paths.insert((route.method, normalized(route.path))) {
            return Err(invalid(format!("duplicate route {}", route.path)));
        }
    }
    Ok(())
}
/// Downloads are GETs that need a signed-in person; the Web adapter also
/// refuses one that intersects a page route or a host path.
fn validate_downloads(entries: &[Entry]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let download = entry.get::<Download>()?;
        if !path_valid(download.path, download.operation, download.access)
            || matches!(download.access, Access::Public)
        {
            return Err(invalid(format!("invalid download {}", entry.id)));
        }
        if !paths.insert(normalized(download.path)) {
            return Err(invalid(format!("duplicate download {}", download.path)));
        }
    }
    Ok(())
}
fn validate_embeds(entries: &[Entry]) -> Result<()> {
    for entry in entries {
        let embed = entry.get::<Embed>()?;
        if embed.slot.is_empty()
            || embed.operation.is_empty()
            || !access_valid(embed.access)
            || matches!(embed.context, EmbedContext::Record { kind: "" })
        {
            return Err(invalid(format!("invalid embed {}", entry.id)));
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<Route>(ROUTES, "base/module_registry", validate_routes) }
inventory::submit! { Consumer::new::<Embed>(EMBEDS, "base/module_registry", validate_embeds) }
inventory::submit! { Consumer::new::<Download>(DOWNLOADS, "base/module_registry", validate_downloads) }
fn validate_moved(entries: &[Entry]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let moved = entry.get::<Moved>()?;
        let from: BTreeSet<_> = parameters(moved.from).collect();
        if !moved.from.starts_with('/')
            || !moved.to.starts_with('/')
            || moved.from == moved.to
            || !parameters(moved.to).all(|name| from.contains(name))
            || !paths.insert(moved.from)
        {
            return Err(invalid(format!("invalid moved path {}", entry.id)));
        }
    }
    Ok(())
}
fn parameters(path: &str) -> impl Iterator<Item = &str> {
    path.split('/')
        .filter_map(|part| part.strip_prefix('{').and_then(|p| p.strip_suffix('}')))
}
inventory::submit! { Consumer::new::<Moved>(MOVED, "base/module_registry", validate_moved) }
inventory::submit! { Consumer::new::<Seed>(SEEDS, "base/module_registry", |_| Ok(())) }
