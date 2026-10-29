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
pub const PAGE_WRITES: &str = "base/module_registry.page_writes";
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
    /// Any one of these capabilities opens the page (Bilimbi's
    /// `{:any_of, [..]}`): a combined page such as a person's own standing,
    /// where each section and write inside still enforces its own capability.
    /// Boot refuses an empty list, a blank key and a repeated key, and the
    /// vocabulary check refuses an undeclared one. Direct denies still win
    /// per key, so a denied key never opens the page through another's deny.
    AnyOf(&'static [&'static str]),
    /// The installation owner only (`bilibi_tenancy::prove_owner`): the
    /// bootstrap account in the operator tenant, in its own browser session.
    /// No role, capability or grant extends it to anyone else.
    Owner,
    /// The inner access, then Base Step-up's check of `operation` (a key
    /// the route's owner declared with `bilibi_step_up::declare!`): when the
    /// tenant's policy requires it, a GET without a holding check is sent to
    /// confirm first. Wrap a signed-in access, never `Public` or another
    /// `StepUp`; Web refuses either, and an undeclared operation, at boot.
    StepUp {
        operation: &'static str,
        inner: &'static Access,
    },
    /// A signed-in customer on its tenant's portal holding this portal
    /// capability (ADR 16). Only under `/portal/`, and nothing else is: the
    /// host reads the portal cookie there and the owner's page service takes
    /// a `PortalScope`. Boot refuses it elsewhere, any other access under
    /// `/portal`, a framed portal route and a key that is not a portal
    /// capability. May be wrapped in `StepUp`.
    Portal(&'static str),
}
/// The customer portal's route space (ADR 16).
pub const PORTAL: &str = "/portal";
/// The host's own portal pages: its credential pages and the portal live
/// socket (`apps/web`). No contribution claims a path under one of them.
pub const PORTAL_HOST: [&str; 7] = [
    "sign-in", "forgot", "reset", "accept", "register", "verify", "live",
];
/// The portal's owner, Core Customer Access, alone serves the portal home
/// (`/portal`) and the customer's account (`/portal/account/...`).
pub const PORTAL_OWNER: &str = "core/customer_access";
const PORTAL_OWNED: [&str; 1] = ["account"];
/// Whether `path` lies in the portal's route space.
pub fn in_portal(path: &str) -> bool {
    path == PORTAL
        || path
            .strip_prefix(PORTAL)
            .is_some_and(|rest| rest.starts_with('/'))
}
impl Access {
    /// The access without its step-up wrapper: what menus and embeds check,
    /// since step-up gates opening a page, not seeing that it exists.
    pub fn base(self) -> Access {
        match self {
            Access::StepUp { inner, .. } => *inner,
            access => access,
        }
    }
    /// The declared step-up operation of a wrapped access.
    /// Whether this access opens a portal page (through a step-up wrapper).
    pub fn portal(self) -> bool {
        matches!(self.base(), Access::Portal(_))
    }
    pub fn step_up(self) -> Option<&'static str> {
        match self {
            Access::StepUp { operation, .. } => Some(operation),
            _ => None,
        }
    }
    /// Whether `viewer` may open a page with this access, for showing it: a
    /// page search row, a workspace tile, a record search hit. Step-up is
    /// ignored, as [`Access::base`] says. Display only: the route decides
    /// again, logged, when the page is opened.
    pub fn admits(self, viewer: &Viewer<'_>) -> bool {
        let held = |key: &str| viewer.capabilities.contains(key);
        match self.base() {
            Access::Public | Access::SignedIn => true,
            Access::Capability(key) => held(key),
            Access::AnyOf(keys) => keys.iter().any(|key| held(key)),
            Access::Operator => viewer.operator,
            Access::OperatorCapability(key) => viewer.operator && held(key),
            Access::Owner => viewer.owner,
            Access::Portal(key) => held(key),
            // Boot refuses a step-up wrapped in another.
            Access::StepUp { .. } => false,
        }
    }
}
/// Who is looking, for [`Access::admits`]: the effective capabilities and
/// whether the operator and owner proofs held. The caller proves them; this
/// only reads the answers.
#[derive(Clone, Copy, Debug)]
pub struct Viewer<'a> {
    pub capabilities: &'a BTreeSet<&'static str>,
    pub operator: bool,
    pub owner: bool,
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
    Record {
        kind: &'static str,
    },
    /// A portal page's slot (`portal.home`, `portal.account`), drawn only in
    /// the portal layout. Its access must be `Access::Portal`, and no other
    /// context may use that access (ADR 16).
    Portal,
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
    match access {
        Access::Capability(key) | Access::OperatorCapability(key) | Access::Portal(key) => {
            !key.trim().is_empty()
        }
        Access::AnyOf(keys) => {
            !keys.is_empty()
                && keys.iter().all(|key| !key.trim().is_empty())
                && keys.iter().collect::<BTreeSet<_>>().len() == keys.len()
        }
        Access::StepUp { inner, .. } => access_valid(*inner),
        _ => true,
    }
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
/// The portal's route space fails closed (ADR 16): portal access only under
/// `/portal`, nothing else there, never framed, and never a path the host or
/// the portal's owner serves. A parameter as the section would capture
/// those, so it is refused too.
fn validate_portal(entry: &Entry, path: &str, access: Access, framed: bool) -> Result<()> {
    let at = format!(
        "{} at {}:{}",
        entry.id, entry.provenance.source, entry.provenance.line
    );
    if in_portal(path) {
        let section = path.split('/').nth(2);
        let owned = section.is_none_or(|s| PORTAL_OWNED.contains(&s));
        let host = section.is_some_and(|s| PORTAL_HOST.contains(&s) || s.starts_with('{'));
        if host || (owned && entry.provenance.module != PORTAL_OWNER) {
            return Err(invalid(format!(
                "{at} claims the reserved portal path {path}"
            )));
        }
    }
    match (access.portal(), in_portal(path)) {
        (true, false) => Err(invalid(format!(
            "{at} declares portal access outside {PORTAL}/: {path}"
        ))),
        (false, true) => Err(invalid(format!(
            "{at} declares non-portal access under {PORTAL}: {path}"
        ))),
        (true, true) if framed => Err(invalid(format!("{at} frames a portal route {path}"))),
        _ => Ok(()),
    }
}
fn validate_routes(entries: &[Entry]) -> Result<()> {
    let mut paths = BTreeSet::new();
    for entry in entries {
        let route = entry.get::<Route>()?;
        if !path_valid(route.path, route.operation, route.access) {
            return Err(invalid(format!("invalid route {}", entry.id)));
        }
        validate_portal(entry, route.path, route.access, route.framed)?;
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
        validate_portal(entry, download.path, download.access, false)?;
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
        if embed.access.portal() != matches!(embed.context, EmbedContext::Portal) {
            return Err(invalid(format!(
                "{} at {}:{} must pair portal access with the portal context",
                entry.id, entry.provenance.source, entry.provenance.line
            )));
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
            // A redirect never crosses into or out of the portal (ADR 16).
            || in_portal(moved.from)
            || in_portal(moved.to)
            || !parameters(moved.to).all(|name| from.contains(name))
            || !paths.insert(moved.from)
        {
            return Err(invalid(format!(
                "invalid moved path {} at {}:{}",
                entry.id, entry.provenance.source, entry.provenance.line
            )));
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

/// The write events of one page or panel service (ADR 01), contributed by
/// its owner under the service id with `Contribution::new(PAGE_WRITES, ..)`.
/// A page's write is the operation contract its event names; Agent API
/// checks each named contract exists and the host refuses, during a live
/// event, a business write the declaration does not cover
/// (`apps/base/agent_api/README.md`).
#[derive(Debug)]
pub struct PageWrites {
    pub service: &'static str,
    /// `(event, contract key)`: the event's write is that contract. An event
    /// that may call one of several contracts lists each.
    pub contracts: &'static [(&'static str, &'static str)],
    pub pending: Pending,
}

/// Events that still write through the owner's facade. Each one is a listed
/// gap in ADR 01; remove it when the event calls its contract.
#[derive(Debug)]
pub enum Pending {
    None,
    Events {
        events: &'static [&'static str],
        reason: &'static str,
    },
    /// Every event other than those in `contracts`: a page not yet migrated.
    Every {
        reason: &'static str,
    },
}

fn validate_page_writes(entries: &[Entry]) -> Result<()> {
    let reason_text = |reason: &str| !reason.trim().is_empty() && reason.chars().count() <= 200;
    for entry in entries {
        let writes = entry.get::<PageWrites>()?;
        let at = format!(
            "page writes {} at {}:{}",
            entry.id, entry.provenance.source, entry.provenance.line
        );
        if entry.id != writes.service
            || !writes
                .service
                .starts_with(&format!("{}.", entry.provenance.module))
        {
            return Err(invalid(format!(
                "{at} must be keyed by a service of its own module"
            )));
        }
        // One event may name several contracts (a form that creates or
        // updates), each pair once.
        let mut pairs = BTreeSet::new();
        for pair in writes.contracts {
            if pair.0.is_empty() || pair.1.is_empty() || !pairs.insert(*pair) {
                return Err(invalid(format!("{at} names an event's contract twice")));
            }
        }
        let mut events: BTreeSet<&str> = pairs.iter().map(|(event, _)| *event).collect();
        match writes.pending {
            Pending::None => {}
            Pending::Every { reason } if reason_text(reason) => {}
            Pending::Events {
                events: pending,
                reason,
            } if !pending.is_empty() && reason_text(reason) => {
                for event in pending {
                    if event.is_empty() || !events.insert(event) {
                        return Err(invalid(format!("{at} names an event twice")));
                    }
                }
            }
            _ => {
                return Err(invalid(format!(
                    "{at} needs its pending events and a reason of 1 to 200 characters"
                )))
            }
        }
    }
    Ok(())
}
inventory::submit! { Consumer::new::<PageWrites>(PAGE_WRITES, "base/module_registry", validate_page_writes) }
