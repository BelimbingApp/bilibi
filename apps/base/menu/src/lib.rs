//! Contributed navigation. No tables and no IO: owners contribute `MenuItem`s,
//! the registry validates the whole set at boot, and `Menu` answers tree and
//! visibility questions. Hiding a link is presentation only; the route itself
//! must enforce the same capability.
use bilibi_module_registry::{Catalog, Consumer, Entry, RegistryError, Result};
use std::collections::{BTreeMap, BTreeSet};

#[doc(hidden)]
pub use bilibi_module_registry::{Contribution, Provenance};
#[doc(hidden)]
pub use inventory;

pub const ITEMS: &str = "base/menu.items";

/// What an item needs before it is shown. `AnyOf` opens a combined page; each
/// section and write inside still enforces its own capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Requirement {
    Open,
    Capability(&'static str),
    AnyOf(&'static [&'static str]),
    /// The installation owner only (`Access::Owner`): never a capability.
    Owner,
}
/// The key a menu decision is asked for [`Requirement::Owner`]: it answers
/// with the live owner proof (`bilibi_tenancy::prove_owner`). It has no dot,
/// so no capability can ever be declared under it; a caller that does not
/// know it answers `false` and the item stays hidden.
pub const OWNER_STANDING: &str = "installation-owner";
impl Requirement {
    pub fn is_valid(self) -> bool {
        match self {
            Self::Open | Self::Owner => true,
            Self::Capability(key) => !key.trim().is_empty(),
            Self::AnyOf(keys) => {
                !keys.is_empty()
                    && keys.iter().all(|key| !key.trim().is_empty())
                    && keys.iter().collect::<BTreeSet<_>>().len() == keys.len()
            }
        }
    }
    /// Evaluates through the caller's single-capability decision. An invalid
    /// declaration is never allowed.
    pub fn allowed(self, decide: &dyn Fn(&str) -> bool) -> bool {
        self.is_valid()
            && match self {
                Self::Open => true,
                Self::Capability(key) => decide(key),
                Self::AnyOf(keys) => keys.iter().any(|key| decide(key)),
                Self::Owner => decide(OWNER_STANDING),
            }
    }
    pub fn keys(self) -> Vec<&'static str> {
        match self {
            Self::Open | Self::Owner => Vec::new(),
            Self::Capability(key) => vec![key],
            Self::AnyOf(keys) => keys.to_vec(),
        }
    }
}

/// One entry as contributed. A container has no route and only groups children.
#[derive(Clone, Copy, Debug)]
pub struct MenuItem {
    pub id: &'static str,
    pub label: &'static str,
    pub icon: Option<&'static str>,
    pub route: Option<&'static str>,
    pub parent: Option<&'static str>,
    pub requirement: Requirement,
    pub order: i32,
}
impl MenuItem {
    pub const fn container(id: &'static str, label: &'static str) -> Self {
        Self {
            id,
            label,
            icon: None,
            route: None,
            parent: None,
            requirement: Requirement::Open,
            order: 0,
        }
    }
    pub const fn page(id: &'static str, label: &'static str, route: &'static str) -> Self {
        Self {
            route: Some(route),
            ..Self::container(id, label)
        }
    }
    pub const fn under(self, parent: &'static str) -> Self {
        Self {
            parent: Some(parent),
            ..self
        }
    }
    pub const fn icon(self, icon: &'static str) -> Self {
        Self {
            icon: Some(icon),
            ..self
        }
    }
    pub const fn requires(self, requirement: Requirement) -> Self {
        Self {
            requirement,
            ..self
        }
    }
    pub const fn order(self, order: i32) -> Self {
        Self { order, ..self }
    }
    pub fn is_container(&self) -> bool {
        self.route.is_none()
    }
}

/// Contribute an item from the owning module: `menu::contribute!("core/x", MenuItem::page(..))`.
/// The module ID is the contributor; provenance records the call site.
#[macro_export]
macro_rules! contribute {
    ($module:literal, $item:expr) => {
        $crate::inventory::submit! {{
            const ITEM: $crate::MenuItem = $item;
            $crate::Contribution::new(
                $crate::ITEMS,
                ITEM.id,
                $crate::Provenance { module: $module, source: file!(), line: line!() },
                &ITEM,
            )
        }}
    };
}
fn invalid(message: String) -> RegistryError {
    RegistryError::Contribution(message)
}
fn id_valid(id: &str) -> bool {
    let mut chars = id.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// A validated item with the module that contributed it.
#[derive(Clone, Copy, Debug)]
pub struct Item {
    pub item: &'static MenuItem,
    pub provenance: Provenance,
}

/// A parent that does not resolve drops its item (and then its descendants)
/// instead of failing boot; one dangling contributor must not remove navigation.
#[derive(Clone, Debug)]
pub struct Dropped {
    pub id: &'static str,
    pub missing_parent: &'static str,
    pub provenance: Provenance,
}
impl std::fmt::Display for Dropped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "menu item {} from {} ({}:{}) names missing parent {}; dropped",
            self.id,
            self.provenance.module,
            self.provenance.source,
            self.provenance.line,
            self.missing_parent
        )
    }
}

#[derive(Clone, Debug)]
pub struct Node {
    pub item: Item,
    pub children: Vec<Node>,
}

#[derive(Clone, Debug, Default)]
pub struct Menu {
    items: Vec<Item>,
    dropped: Vec<Dropped>,
}
impl Menu {
    /// Build from the installed catalog. Duplicate IDs, self-parents and parent
    /// cycles are contributor defects with no safe reading, so they are errors.
    pub fn from_catalog(catalog: &Catalog) -> Result<Self> {
        Self::from_entries(catalog.entries::<MenuItem>(ITEMS)?)
    }
    pub fn from_entries(entries: &[Entry]) -> Result<Self> {
        let mut by_id: BTreeMap<&str, Item> = BTreeMap::new();
        for entry in entries {
            let item = entry.get::<MenuItem>()?;
            if !id_valid(item.id)
                || item.parent.is_some_and(|parent| !id_valid(parent))
                || item.label.trim().is_empty()
                || item.route.is_some_and(|route| !route.starts_with('/'))
                || !item.requirement.is_valid()
            {
                return Err(invalid(format!(
                    "invalid menu item {} from {} at {}:{}",
                    item.id,
                    entry.provenance.module,
                    entry.provenance.source,
                    entry.provenance.line
                )));
            }
            if item.parent == Some(item.id) {
                return Err(invalid(format!(
                    "menu item {} cannot be its own parent",
                    item.id
                )));
            }
            let current = Item {
                item,
                provenance: entry.provenance,
            };
            if let Some(first) = by_id.insert(item.id, current) {
                return Err(invalid(format!(
                    "duplicate menu item {} contributed by {} and {}",
                    item.id, first.provenance.module, entry.provenance.module
                )));
            }
        }
        // Index everything first, so contribution order never decides whether a
        // parent resolves. Drop to a fixpoint: a child of a dropped item is
        // orphaned and would otherwise disappear without a diagnostic.
        let mut dropped = Vec::new();
        loop {
            let missing: Vec<_> = by_id
                .values()
                .filter_map(|current| {
                    let parent = current.item.parent?;
                    (!by_id.contains_key(parent)).then_some(Dropped {
                        id: current.item.id,
                        missing_parent: parent,
                        provenance: current.provenance,
                    })
                })
                .collect();
            if missing.is_empty() {
                break;
            }
            for gone in missing {
                by_id.remove(gone.id);
                dropped.push(gone);
            }
        }
        for start in by_id.keys() {
            let mut seen = BTreeSet::new();
            let mut at = Some(*start);
            while let Some(id) = at {
                if !seen.insert(id) {
                    return Err(invalid(format!("circular menu parent reference at {id}")));
                }
                at = by_id[id].item.parent;
            }
        }
        let mut items: Vec<_> = by_id.into_values().collect();
        items.sort_by_key(|current| (current.item.order, current.item.id));
        Ok(Self { items, dropped })
    }
    /// Every validated item ordered by `(order, id)`, regardless of visibility.
    pub fn items(&self) -> &[Item] {
        &self.items
    }
    pub fn fetch(&self, id: &str) -> Option<&Item> {
        self.items.iter().find(|current| current.item.id == id)
    }
    /// Items removed at boot because an ancestor was missing. Log them once.
    pub fn dropped(&self) -> &[Dropped] {
        &self.dropped
    }
    /// The unfiltered tree for diagnostics. Render `visible_tree` instead.
    pub fn tree(&self) -> Vec<Node> {
        build(&self.items, None)
    }
    /// Every capability the menu can ask about, so a caller can decide them all
    /// in one authorization pass before filtering.
    pub fn capability_keys(&self) -> BTreeSet<&'static str> {
        self.items
            .iter()
            .flat_map(|current| current.item.requirement.keys())
            .collect()
    }
    /// What the actor may see. `decide` answers one capability; Menu does not
    /// depend on Authz. A container with no visible child is hidden, so a
    /// section never renders as an empty heading.
    pub fn visible_tree(&self, decide: &dyn Fn(&str) -> bool) -> Vec<Node> {
        let permitted: Vec<_> = self
            .items
            .iter()
            .filter(|current| current.item.requirement.allowed(decide))
            .copied()
            .collect();
        prune(build(&permitted, None))
    }
}
fn build(items: &[Item], parent: Option<&str>) -> Vec<Node> {
    let mut nodes: Vec<_> = items
        .iter()
        .filter(|current| current.item.parent == parent)
        .map(|current| Node {
            item: *current,
            children: build(items, Some(current.item.id)),
        })
        .collect();
    // Source siblings sort by label; the stable sort keeps `(order, id)` ties.
    nodes.sort_by_key(|node| node.item.item.label.to_lowercase());
    nodes
}
fn prune(nodes: Vec<Node>) -> Vec<Node> {
    nodes
        .into_iter()
        .map(|node| Node {
            children: prune(node.children),
            ..node
        })
        .filter(|node| !(node.item.item.is_container() && node.children.is_empty()))
        .collect()
}

inventory::submit! { Consumer::new::<MenuItem>(ITEMS, "base/menu", |entries| Menu::from_entries(entries).map(|_| ())) }
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/menu", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry"], migrations: &[], services: &[],
    }
}

// Structural roots: items attach to these, and they stay hidden until a
// visible child exists. Unported Domain roots therefore never render.
contribute!(
    "base/menu",
    MenuItem::container("admin", "Administration")
        .icon("cog-6-tooth")
        .order(900)
);
contribute!(
    "base/menu",
    MenuItem::container("admin.system", "System")
        .under("admin")
        .order(90)
);
contribute!(
    "base/menu",
    MenuItem::container("admin.system.database", "Database")
        .under("admin.system")
        .order(10)
);
contribute!(
    "base/menu",
    MenuItem::container("admin.system.diagnostics", "Diagnostics")
        .under("admin.system")
        .order(20)
);
contribute!(
    "base/menu",
    MenuItem::container("admin.system.integrations", "Integrations")
        .under("admin.system")
        .order(30)
);
contribute!(
    "base/menu",
    MenuItem::container("admin.system.software", "Software")
        .under("admin.system")
        .order(40)
);
contribute!(
    "base/menu",
    MenuItem::container("production", "Production").order(200)
);
contribute!(
    "base/menu",
    MenuItem::container("procurement", "Procurement").order(300)
);
contribute!(
    "base/menu",
    MenuItem::container("finance", "Finance").order(400)
);
contribute!(
    "base/menu",
    MenuItem::container("maintenance", "Maintenance").order(500)
);
