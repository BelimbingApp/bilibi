//! Deep modules submit descriptors at link time. Discovery validates the graph,
//! sorts it dependency-first, and builds named typed services without host lists.
use std::{
    any::Any,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};
mod composition;
pub mod contributions;
pub use composition::{Composition, Container, PinState};
pub use contributions::{Catalog, Consumer, Contribution, Entry, Provenance};

pub type Value = Arc<dyn Any + Send + Sync>;
pub type Result<T> = std::result::Result<T, RegistryError>;
pub type Factory = fn(&Services) -> Result<Value>;
pub type Migration = (&'static str, &'static str);

#[derive(Debug, thiserror::Error)]
pub enum RegistryError {
    #[error("Invalid module graph: {0}")]
    Graph(String),
    #[error("Invalid contribution: {0}")]
    Contribution(String),
    #[error("Missing or wrong service type: {0}")]
    Service(String),
}
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Layer {
    Base,
    Core,
    Domains,
    Extensions,
}
/// Container IDs a Domain or Extension may not take; `scripts/discover-modules.py`
/// refuses the same set for mount folders.
const RESERVED_CONTAINERS: &[&str] = &["base", "core", "web", "src"];
impl Layer {
    /// Base and Core IDs are `base/<module>` and `core/<module>`. Domain and
    /// Extension IDs are `<container>/<module>`, the mounted checkout and module
    /// folders, because Bilimbi's IDs are persisted data (ledger owners, audit
    /// kinds, setting keys) and the port keeps them.
    fn accepts(self, id: &str) -> bool {
        let Some((container, module)) = id.split_once('/') else {
            return false;
        };
        match self {
            Self::Base => container == "base" && !module.is_empty(),
            Self::Core => container == "core" && !module.is_empty(),
            Self::Domains | Self::Extensions => {
                snake_case(container)
                    && snake_case(module)
                    && !RESERVED_CONTAINERS.contains(&container)
            }
        }
    }
    fn mounted(self) -> bool {
        matches!(self, Self::Domains | Self::Extensions)
    }
}
fn snake_case(segment: &str) -> bool {
    segment.starts_with(|c: char| c.is_ascii_lowercase())
        && !segment.ends_with('_')
        && !segment.contains("__")
        && segment
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}
/// A mounted module names its migrations `<container>-<module>-NNN-<slug>`
/// (underscores become hyphens), so versions stay unique across repositories
/// and say which module owns them.
fn migration_named(module: &str, version: &str) -> bool {
    let prefix = format!("{}-", module.replace(['/', '_'], "-"));
    let Some((number, slug)) = version
        .strip_prefix(&prefix)
        .and_then(|rest| rest.split_once('-'))
    else {
        return false;
    };
    number.len() == 3
        && number.chars().all(|c| c.is_ascii_digit())
        && !slug.is_empty()
        && !slug.starts_with('-')
        && !slug.ends_with('-')
        && !slug.contains("--")
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}
pub struct Service {
    pub id: &'static str,
    pub factory: Factory,
}
pub struct Module {
    pub id: &'static str,
    pub layer: Layer,
    pub dependencies: &'static [&'static str],
    pub migrations: &'static [Migration],
    pub services: &'static [Service],
}
inventory::collect!(Module);

pub fn discover() -> Result<Vec<&'static Module>> {
    let modules = order(inventory::iter::<Module>.into_iter().collect())?;
    Catalog::discover(&modules)?;
    Ok(modules)
}

pub fn order(modules: Vec<&'static Module>) -> Result<Vec<&'static Module>> {
    let mut pending = BTreeMap::new();
    let mut services = BTreeSet::new();
    let mut migrations = BTreeSet::new();
    for module in modules {
        if !module.layer.accepts(module.id) || pending.insert(module.id, module).is_some() {
            return Err(RegistryError::Graph(format!(
                "duplicate or wrongly layered module {}",
                module.id
            )));
        }
        for service in module.services {
            if !service.id.starts_with(&format!("{}.", module.id)) || !services.insert(service.id) {
                return Err(RegistryError::Graph(format!(
                    "invalid or duplicate service {}",
                    service.id
                )));
            }
        }
        for (version, _) in module.migrations {
            if module.layer.mounted() && !migration_named(module.id, version) {
                return Err(RegistryError::Graph(format!(
                    "migration {version} of {} must be named {}-NNN-<slug>",
                    module.id,
                    module.id.replace(['/', '_'], "-")
                )));
            }
            if !migrations.insert(version) {
                return Err(RegistryError::Graph(format!(
                    "duplicate migration {version}"
                )));
            }
        }
    }
    for module in pending.values() {
        for dependency in module.dependencies {
            let parent = pending.get(dependency).ok_or_else(|| {
                RegistryError::Graph(format!("{} requires missing {dependency}", module.id))
            })?;
            if parent.layer > module.layer {
                return Err(RegistryError::Graph(format!(
                    "{} depends upward on {dependency}",
                    module.id
                )));
            }
        }
    }
    let mut ordered = Vec::new();
    let mut installed = BTreeSet::new();
    while !pending.is_empty() {
        let next = pending
            .values()
            .filter(|module| module.dependencies.iter().all(|id| installed.contains(id)))
            .min_by_key(|module| (module.layer, module.id))
            .copied()
            .ok_or_else(|| RegistryError::Graph("dependency cycle".into()))?;
        pending.remove(next.id);
        installed.insert(next.id);
        ordered.push(next);
    }
    Ok(ordered)
}

/// Boot check for a composed binary: the Domain and Extension descriptors
/// registered at link time must be exactly the `(module ID, crate)` pairs that
/// `scripts/discover-modules.py` found mounted (`bilibi_domains::MODULES`,
/// `bilibi_extensions::MODULES`). A mounted crate that registers no descriptor,
/// registers one under another ID or layer, or a descriptor no mounted crate
/// accounts for is refused. Examples that link their own reference Extension
/// descriptors never run this check.
pub fn verify_composition(
    modules: &[&'static Module],
    domains: &[(&str, &str)],
    extensions: &[(&str, &str)],
) -> Result<()> {
    let expected = |layer| match layer {
        Layer::Domains => domains,
        Layer::Extensions => extensions,
        Layer::Base | Layer::Core => &[],
    };
    for layer in [Layer::Domains, Layer::Extensions] {
        for (id, krate) in expected(layer) {
            match modules.iter().find(|module| module.id == *id) {
                Some(module) if module.layer == layer => {}
                Some(module) => {
                    return Err(RegistryError::Graph(format!(
                        "{id} is mounted as {layer:?} by {krate} but its descriptor declares {:?}",
                        module.layer
                    )))
                }
                None => {
                    return Err(RegistryError::Graph(format!(
                        "{krate} is mounted but registers no {layer:?} descriptor {id}"
                    )))
                }
            }
        }
    }
    for module in modules.iter().filter(|module| module.layer.mounted()) {
        if !expected(module.layer)
            .iter()
            .any(|(id, _)| *id == module.id)
        {
            return Err(RegistryError::Graph(format!(
                "{:?} descriptor {} matches no mounted module crate",
                module.layer, module.id
            )));
        }
    }
    Ok(())
}

#[derive(Default)]
pub struct Services {
    values: BTreeMap<&'static str, Value>,
    resolving: Option<BTreeSet<&'static str>>,
    catalog: Option<Catalog>,
}
impl Services {
    pub fn provide(&mut self, id: &'static str, value: Value) -> Result<()> {
        if self.values.contains_key(id) {
            return Err(RegistryError::Service(format!("duplicate {id}")));
        }
        self.values.insert(id, value);
        Ok(())
    }
    pub fn get<T: Any + Send + Sync>(&self, id: &str) -> Result<Arc<T>> {
        if let Some(allowed) = &self.resolving {
            let owner = id.rsplit_once('.').map(|(owner, _)| owner);
            if !id.starts_with("bootstrap.") && !owner.is_some_and(|owner| allowed.contains(owner))
            {
                return Err(RegistryError::Service(format!(
                    "undeclared dependency {id}"
                )));
            }
        }
        self.values
            .get(id)
            .cloned()
            .and_then(|value| value.downcast().ok())
            .ok_or_else(|| RegistryError::Service(id.into()))
    }
    pub fn catalog(&self) -> Result<&Catalog> {
        self.catalog
            .as_ref()
            .ok_or_else(|| RegistryError::Contribution("catalog not installed".into()))
    }
    pub fn install(&mut self, modules: &[&'static Module]) -> Result<()> {
        let modules = order(modules.to_vec())?;
        let catalog = Catalog::discover(&modules)?;
        for module in &modules {
            let mut allowed: BTreeSet<_> = module.dependencies.iter().copied().collect();
            allowed.insert(module.id);
            self.resolving = Some(allowed);
            for service in module.services {
                let value = (service.factory)(self);
                match value {
                    Ok(value) => self.provide(service.id, value)?,
                    Err(error) => {
                        self.resolving = None;
                        return Err(error);
                    }
                }
            }
        }
        self.resolving = None;
        for ready in inventory::iter::<Ready> {
            if !modules.iter().any(|module| module.id == ready.module) {
                return Err(RegistryError::Graph(format!(
                    "unknown readiness owner {}",
                    ready.module
                )));
            }
            (ready.check)(self)?;
        }
        self.catalog = Some(catalog);
        Ok(())
    }
}

/// A Base-owned boundary may be bound by an upstream adapter during installation.
/// Readiness checks run after all factories and must refuse missing adapters.
pub struct Ready {
    pub module: &'static str,
    pub check: fn(&Services) -> Result<()>,
}
inventory::collect!(Ready);

inventory::submit! {
    Module {
        id:"base/module_registry",layer:Layer::Base,dependencies:&[],migrations:&[],services:&[],
    }
}
