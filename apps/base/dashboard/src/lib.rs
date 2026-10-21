//! Dashboard owns immutable entry metadata and each account's arrangement,
//! never a contributor's tables. Owners draw their entry through a discovered
//! `Embed` (context `Dashboard`) mounted as an independent live panel.
//!
//! A contributor adds two declarations (see `docs`/README): a [`contribute!`]
//! entry here and an `Embed` with `EmbedContext::Dashboard` whose service is a
//! `PageService`; its `mount` reads [`PanelArgs`] and its `update` renders one
//! root element whose id is [`PanelArgs::id`].
mod web;

use bilibi_database::Error;
use bilibi_module_registry::{
    contributions::{Embed, EmbedContext, EMBEDS},
    Catalog, Consumer, Entry, RegistryError,
};
use bilibi_settings::{Definition, ScopeKind, Setting, Validator, ValueType, DEFINITIONS};
use bilibi_tenancy::Scope;
use bilibi_ui::page::PageRequest;
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

pub const ENTRIES: &str = "base/dashboard.entries";
/// Per-account grid arrangement: ordered entry ids. Declared here until a
/// dedicated preferences module owns it; a second declaration fails boot.
pub const LAYOUT: &str = "ui.dashboard.layout";
/// Per-account arrangement of the full-width sections below the grid.
pub const SECTIONS: &str = "ui.dashboard.sections";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Placement {
    Grid,
    Section,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Size {
    Small,
    Medium,
    Large,
}
#[derive(Clone, Copy, Debug)]
pub struct Widget {
    /// Durable preference key. Keep stable when the label changes.
    pub id: &'static str,
    pub label: &'static str,
    /// Id of the `Embed` contribution (context `Dashboard`) that draws it.
    pub embed: &'static str,
    pub placement: Placement,
    pub size: Size,
    pub order: u32,
    /// Offers the entry. The embed's own `access` still guards every read.
    pub capability: Option<&'static str>,
    /// Milliseconds the dashboard waits between refreshes of its panels while
    /// this entry is shown; zero asks for no refresh of its own.
    pub refresh_interval: u32,
}
impl Widget {
    pub const fn new(id: &'static str, label: &'static str, embed: &'static str) -> Self {
        Self {
            id,
            label,
            embed,
            placement: Placement::Grid,
            size: Size::Small,
            order: 0,
            capability: None,
            refresh_interval: 0,
        }
    }
    pub const fn section(self) -> Self {
        Self {
            placement: Placement::Section,
            ..self
        }
    }
    pub const fn size(self, size: Size) -> Self {
        Self { size, ..self }
    }
    pub const fn order(self, order: u32) -> Self {
        Self { order, ..self }
    }
    pub const fn requires(self, key: &'static str) -> Self {
        Self {
            capability: Some(key),
            ..self
        }
    }
    pub const fn refresh(self, milliseconds: u32) -> Self {
        Self {
            refresh_interval: milliseconds,
            ..self
        }
    }
}
#[doc(hidden)]
pub use bilibi_module_registry::{Contribution, Provenance};
#[doc(hidden)]
pub use inventory;
/// Contribute an entry from the owning module:
/// `bilibi_dashboard::contribute!("core/x", Widget::new("x-open", "Open", "x.dashboard-open"))`.
#[macro_export]
macro_rules! contribute {
    ($module:literal, $widget:expr) => {
        $crate::inventory::submit! {{
            const WIDGET: $crate::Widget = $widget;
            $crate::Contribution::new($crate::ENTRIES, WIDGET.id,
                $crate::Provenance { module: $module, source: file!(), line: line!() }, &WIDGET)
        }}
    };
}

#[derive(Clone, Default)]
pub struct Dashboard {
    entries: Vec<&'static Widget>,
}
impl Dashboard {
    /// Also proves every entry names an installed dashboard embed: a missing
    /// panel is a contributor defect that fails boot, not a blank card.
    pub fn from_catalog(catalog: &Catalog) -> bilibi_module_registry::Result<Self> {
        let dashboard = Self::from_entries(catalog.entries::<Widget>(ENTRIES)?)?;
        let embeds = catalog.entries::<Embed>(EMBEDS)?;
        for entry in catalog.entries::<Widget>(ENTRIES)? {
            let widget = entry.get::<Widget>()?;
            let installed = embeds
                .iter()
                .find(|e| e.id == widget.embed)
                .map(|e| e.get::<Embed>())
                .transpose()?
                .is_some_and(|e| matches!(e.context, EmbedContext::Dashboard));
            if !installed {
                return Err(RegistryError::Contribution(format!(
                    "dashboard entry {} at {}:{} names embed {}, which no module contributes with the Dashboard context",
                    widget.id, entry.provenance.source, entry.provenance.line, widget.embed
                )));
            }
        }
        Ok(dashboard)
    }
    fn from_entries(entries: &[Entry]) -> bilibi_module_registry::Result<Self> {
        let mut ids = BTreeSet::new();
        let mut widgets = Vec::new();
        for entry in entries {
            let widget = entry.get::<Widget>()?;
            if widget.id != entry.id
                || widget.id.is_empty()
                || widget.id.len() > 90
                || !widget
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                || widget.label.trim().is_empty()
                || widget.embed.trim().is_empty()
                || widget.capability.is_some_and(|key| key.trim().is_empty())
                || !ids.insert(widget.id)
            {
                return Err(RegistryError::Contribution(format!(
                    "invalid/duplicate dashboard entry {} at {}:{}",
                    entry.id, entry.provenance.source, entry.provenance.line
                )));
            }
            widgets.push(widget);
        }
        // The live host bounds the complete set of child panels at 32.
        if widgets.len() > 32 {
            return Err(RegistryError::Contribution(
                "dashboard supports at most 32 installed entries".into(),
            ));
        }
        widgets.sort_by_key(|w| (w.order, w.id));
        Ok(Self { entries: widgets })
    }
    pub fn entries(&self) -> &[&'static Widget] {
        &self.entries
    }
    pub fn widgets(&self) -> Vec<&'static Widget> {
        self.placed(Placement::Grid)
    }
    pub fn sections(&self) -> Vec<&'static Widget> {
        self.placed(Placement::Section)
    }
    pub fn fetch_widget(&self, id: &str) -> Option<&'static Widget> {
        self.widgets().into_iter().find(|w| w.id == id)
    }
    fn placed(&self, placement: Placement) -> Vec<&'static Widget> {
        self.entries
            .iter()
            .copied()
            .filter(|w| w.placement == placement)
            .collect()
    }
}

/// DOM id of an entry's panel. The page renders an empty anchor with this id and
/// the panel's fragment replaces it; Web keeps the anchor when a panel is refused.
pub fn panel_id(entry: &str) -> String {
    format!("dashboard-panel-{entry}")
}
fn valid_entry_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 90
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
}

/// What the dashboard tells a panel when it mounts it, carried in the server
/// owned `PanelSpec` so a browser event can neither name nor change it. A
/// changed `editing` or `refresh` is a changed spec, which remounts the panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PanelArgs {
    entry: String,
    /// True while the account customizes the layout. The page lays its move and
    /// remove controls over the panel's top-right corner, so a panel withholds
    /// navigation (links) meanwhile.
    pub editing: bool,
    /// Minimum time between two reads; zero means the panel reads once.
    pub refresh: Duration,
}
impl PanelArgs {
    pub(crate) fn spec(widget: &Widget, editing: bool) -> bilibi_ui::page::PanelSpec {
        bilibi_ui::page::PanelSpec {
            id: panel_id(widget.id),
            embed: widget.embed.into(),
            path: [("entry".to_owned(), widget.id.to_owned())].into(),
            query: [
                ("editing".to_owned(), editing.to_string()),
                ("refresh".to_owned(), widget.refresh_interval.to_string()),
            ]
            .into(),
        }
    }
    /// Reads the arguments in a panel's mount request; anything else refuses.
    pub fn from_request(request: &PageRequest<'_, Scope>) -> bilibi_database::Result<Self> {
        let entry = request.path.get("entry").filter(|id| valid_entry_id(id));
        let editing = match request.query.get("editing").map(String::as_str) {
            Some("true") => true,
            Some("false") => false,
            _ => return Err(Error::Invalid),
        };
        let refresh = request
            .query
            .get("refresh")
            .and_then(|ms| ms.parse::<u32>().ok());
        match (entry, refresh) {
            (Some(entry), Some(ms)) if request.path.len() == 1 && request.query.len() == 2 => {
                Ok(Self {
                    entry: entry.clone(),
                    editing,
                    refresh: Duration::from_millis(ms.into()),
                })
            }
            _ => Err(Error::Invalid),
        }
    }
    pub fn entry(&self) -> &str {
        &self.entry
    }
    /// The id the panel's fragment must carry on its root element.
    pub fn id(&self) -> String {
        panel_id(&self.entry)
    }
}

/// A panel's last rendering, so the dashboard's frequent refreshes (its own
/// timer, subscription wakes and every arrangement event) do not turn into
/// reads: `fresh` answers the stored HTML until the entry's `refresh_interval`
/// has passed, or for good when the entry asks for no refresh.
#[derive(Debug, Default)]
pub struct PanelCache {
    stored: Option<(String, Instant)>,
}
impl PanelCache {
    pub fn fresh(&self, args: &PanelArgs) -> Option<&str> {
        self.stored
            .as_ref()
            .filter(|(_, read)| args.refresh.is_zero() || read.elapsed() < args.refresh)
            .map(|(html, _)| html.as_str())
    }
    pub fn store(&mut self, html: String) -> String {
        self.stored = Some((html.clone(), Instant::now()));
        html
    }
}

fn valid_arrangement(value: &Value) -> bool {
    value.as_array().is_some_and(|ids| {
        ids.len() <= 32
            && ids
                .iter()
                .all(|id| id.as_str().is_some_and(|s| !s.is_empty() && s.len() <= 90))
            && ids
                .iter()
                .filter_map(Value::as_str)
                .collect::<BTreeSet<_>>()
                .len()
                == ids.len()
    })
}
macro_rules! preference {
    ($key:expr) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key,
        Provenance { module: "base/dashboard", source: file!(), line: line!() },
        &Setting::Definition(Definition {
            value_type: ValueType::Array, scopes: &[ScopeKind::User], default: || json!([]),
            nullable: false, encrypted: false, minimum: None, maximum: None,
            validator: Some(Validator { accepts: valid_arrangement, message: "must be a unique bounded list of dashboard entry ids" }), editor: None,
        })) }
    };
}
preference!(LAYOUT);
preference!(SECTIONS);
inventory::submit! { Consumer::new::<Widget>(ENTRIES, "base/dashboard", |entries| Dashboard::from_entries(entries).map(|_| ())) }
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/dashboard", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/ui", "base/settings", "base/database", "base/tenancy", "base/authz", "base/menu"],
        migrations: &[], services: &[
            bilibi_module_registry::Service { id: "base/dashboard.catalog", factory: |_| {
                let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
                let dashboard = Dashboard::from_catalog(&catalog)?;
                // Capability keys are proven here, not in the validator: the
                // authz snapshot initialises while validators run.
                let vocabulary = bilibi_authz::vocabulary().map_err(|_| RegistryError::Service("authz vocabulary unavailable".into()))?;
                for widget in dashboard.entries() {
                    if let Some(key) = widget.capability {
                        if !vocabulary.capabilities.contains_key(key) {
                            return Err(RegistryError::Contribution(format!("dashboard entry {} requires unknown capability {key}", widget.id)));
                        }
                    }
                }
                Ok(Arc::new(dashboard))
            }},
            bilibi_module_registry::Service { id: "base/dashboard.pages", factory: |services| {
                Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, bilibi_database::Error>(Arc::new(web::Pages::new(
                    services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                    services.get("base/dashboard.catalog")?, services.get("base/settings.store")?,
                    services.get("base/database.mutations")?,
                )))))
            }},
        ],
    }
}
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::ROUTES, "dashboard.index",
Provenance { module: "base/dashboard", source: file!(), line: line!() },
&bilibi_module_registry::contributions::Route { method: bilibi_module_registry::contributions::Method::Get, path: "/dashboard", service: "base/dashboard.pages", operation: "index", access: bilibi_module_registry::contributions::Access::SignedIn, framed: true }) }
// No menu item: as in Bilimbi, the status-bar brand link (`app-brand`) and
// sign-in reach the dashboard, and the sidebar holds no `nav-dashboard`.
