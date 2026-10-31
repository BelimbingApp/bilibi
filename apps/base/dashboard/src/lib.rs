//! Dashboard owns immutable entry metadata and each account's arrangement,
//! never a contributor's tables. Owners draw their entry through a discovered
//! `Embed` (context `Dashboard`) mounted as an independent live panel.
//!
//! A contributor adds two declarations (see `docs`/README): a [`contribute!`]
//! entry here and an `Embed` with `EmbedContext::Dashboard` whose service is a
//! `PageService`; its `mount` reads [`PanelArgs`] and its `update` renders one
//! root element whose id is [`PanelArgs::id`].
//!
//! A module whose entries exist only at run time (an operator's saved
//! reports) contributes a [`Provider`] with [`provide!`] instead: the page
//! rechecks placed entries on every update and asks for offers in Customize; each mounts as
//! its own panel through the provider's embed, like a contributed entry.
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
    future::Future,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

pub const ENTRIES: &str = "base/dashboard.entries";
pub const PROVIDERS: &str = "base/dashboard.providers";
/// Entries one provider offers a reader at most; the rest are not offered.
pub const MAX_PROVIDED: usize = 100;
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
/// One run-time entry a provider offers the reader: `key` makes its id
/// (`<prefix>-<key>`, stored in each account's arrangement, so keep it
/// stable) and `label` names it in Customize.
#[derive(Clone, Debug)]
pub struct Provided {
    pub key: String,
    pub label: String,
    /// Optional owner-supplied access/data key, rechecked on every update.
    /// A change remounts this entry's panel, discarding its cached rendering.
    pub revision: Option<String>,
}
pub type ProvidedFuture<'a> =
    Pin<Box<dyn Future<Output = bilibi_database::Result<Vec<Provided>>> + Send + 'a>>;

/// Entries that exist only at run time. The page calls `entries` with the
/// reader's sealed scope and the keys of this provider's entries the reader
/// has placed, on every update; the owner answers only what that reader may
/// see: each placed key it still may (however many others it has), then, only
/// when `offers` is true (Customize), up to [`MAX_PROVIDED`] more to offer.
/// Each entry mounts `embed` with its id as the panel's
/// `entry`, and the embed's `access` still guards every mount and refresh.
/// Provided entries are offered in Customize, never placed unasked.
#[derive(Clone, Copy)]
pub struct Provider {
    /// The id prefix; must not begin any contributed entry's id.
    pub prefix: &'static str,
    pub embed: &'static str,
    pub placement: Placement,
    /// Offers the provider's entries at all.
    pub capability: Option<&'static str>,
    pub refresh_interval: u32,
    pub entries: for<'a> fn(&'a Scope, &'a [String], bool) -> ProvidedFuture<'a>,
}

#[doc(hidden)]
pub use bilibi_module_registry::{Contribution, Provenance};
#[doc(hidden)]
pub use inventory;
/// Contribute a run-time [`Provider`] from the owning module:
/// `bilibi_dashboard::provide!("core/x", Provider { prefix: "x", .. })`.
#[macro_export]
macro_rules! provide {
    ($module:literal, $provider:expr) => {
        $crate::inventory::submit! {{
            const PROVIDER: $crate::Provider = $provider;
            $crate::Contribution::new($crate::PROVIDERS, PROVIDER.prefix,
                $crate::Provenance { module: $module, source: file!(), line: line!() }, &PROVIDER)
        }}
    };
}
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
    providers: Vec<&'static Provider>,
}

/// One entry the page may show: contributed or provided, owned so both
/// kinds arrange alike.
#[derive(Clone, Debug)]
pub(crate) struct Item {
    pub id: String,
    pub label: String,
    pub embed: &'static str,
    pub placement: Placement,
    pub refresh_interval: u32,
    pub revision: Option<String>,
}
impl Item {
    pub fn widget(widget: &Widget) -> Self {
        Self {
            id: widget.id.into(),
            label: widget.label.into(),
            embed: widget.embed,
            placement: widget.placement,
            refresh_interval: widget.refresh_interval,
            revision: None,
        }
    }
    /// A provided entry; `None` when its key would make an invalid id.
    pub fn provided(provider: &Provider, entry: Provided) -> Option<Self> {
        let id = format!("{}-{}", provider.prefix, entry.key);
        (valid_entry_id(&id) && !entry.label.trim().is_empty()).then_some(Self {
            id,
            label: entry.label,
            embed: provider.embed,
            placement: provider.placement,
            refresh_interval: provider.refresh_interval,
            revision: entry.revision,
        })
    }
}
impl Dashboard {
    /// Also proves every entry names an installed dashboard embed: a missing
    /// panel is a contributor defect that fails boot, not a blank card.
    pub fn from_catalog(catalog: &Catalog) -> bilibi_module_registry::Result<Self> {
        let mut dashboard = Self::from_entries(catalog.entries::<Widget>(ENTRIES)?)?;
        dashboard.providers = providers(catalog.entries::<Provider>(PROVIDERS)?, &dashboard)?;
        let embeds = catalog.entries::<Embed>(EMBEDS)?;
        let dashboard_embed = |id: &str| -> bilibi_module_registry::Result<bool> {
            Ok(embeds
                .iter()
                .find(|e| e.id == id)
                .map(|e| e.get::<Embed>())
                .transpose()?
                .is_some_and(|e| matches!(e.context, EmbedContext::Dashboard)))
        };
        for provider in &dashboard.providers {
            if !dashboard_embed(provider.embed)? {
                return Err(RegistryError::Contribution(format!(
                    "dashboard provider {} names embed {}, which no module contributes with the Dashboard context",
                    provider.prefix, provider.embed
                )));
            }
        }
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
        Ok(Self {
            entries: widgets,
            providers: Vec::new(),
        })
    }
    pub fn providers(&self) -> &[&'static Provider] {
        &self.providers
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

/// The contributed providers, proven: a valid unique prefix that begins no
/// contributed entry's id, and a non-empty embed.
fn providers(
    entries: &[Entry],
    dashboard: &Dashboard,
) -> bilibi_module_registry::Result<Vec<&'static Provider>> {
    let mut prefixes = BTreeSet::new();
    let mut providers = Vec::new();
    for entry in entries {
        let provider = entry.get::<Provider>()?;
        let taken = dashboard
            .entries()
            .iter()
            .any(|w| w.id.starts_with(&format!("{}-", provider.prefix)));
        if provider.prefix != entry.id
            || !valid_entry_id(provider.prefix)
            || provider.prefix.len() > 40
            || provider.embed.trim().is_empty()
            || provider.capability.is_some_and(|key| key.trim().is_empty())
            || taken
            || !prefixes.insert(provider.prefix)
        {
            return Err(RegistryError::Contribution(format!(
                "invalid/duplicate dashboard provider {} at {}:{}",
                entry.id, entry.provenance.source, entry.provenance.line
            )));
        }
        providers.push(provider);
    }
    Ok(providers)
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
/// changed `editing`, `refresh` or `revision` remounts the panel.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PanelArgs {
    entry: String,
    revision: Option<String>,
    /// True while the account customizes the layout. The page lays its move and
    /// remove controls over the panel's top-right corner, so a panel withholds
    /// navigation (links) meanwhile.
    pub editing: bool,
    /// Minimum time between two reads; zero means the panel reads once.
    pub refresh: Duration,
}
impl PanelArgs {
    pub(crate) fn spec(widget: &Item, editing: bool) -> bilibi_ui::page::PanelSpec {
        let mut spec = bilibi_ui::page::PanelSpec {
            id: panel_id(&widget.id),
            embed: widget.embed.into(),
            path: [("entry".to_owned(), widget.id.clone())].into(),
            query: [
                ("editing".to_owned(), editing.to_string()),
                ("refresh".to_owned(), widget.refresh_interval.to_string()),
            ]
            .into(),
        };
        if let Some(revision) = &widget.revision {
            spec.query.insert("revision".into(), revision.clone());
        }
        spec
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
        let revision = request.query.get("revision").cloned();
        match (entry, refresh) {
            (Some(entry), Some(ms))
                if request.path.len() == 1
                    && request.query.len() == 2 + usize::from(revision.is_some()) =>
            {
                Ok(Self {
                    entry: entry.clone(),
                    revision,
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
    /// The owner's current key, carried only in the server-owned panel spec.
    pub fn revision(&self) -> Option<&str> {
        self.revision.as_deref()
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
inventory::submit! { Consumer::new::<Provider>(PROVIDERS, "base/dashboard", |entries| providers(entries, &Dashboard::default()).map(|_| ())) }
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
                let keys = dashboard.entries().iter().map(|w| (w.id, w.capability)).chain(dashboard.providers().iter().map(|p| (p.prefix, p.capability)));
                for (id, capability) in keys {
                    if let Some(key) = capability {
                        if !vocabulary.capabilities.contains_key(key) {
                            return Err(RegistryError::Contribution(format!("dashboard entry {id} requires unknown capability {key}")));
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
&bilibi_module_registry::contributions::Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant, method: bilibi_module_registry::contributions::Method::Get, path: "/dashboard", service: "base/dashboard.pages", operation: "index", access: bilibi_module_registry::contributions::Access::SignedIn, framed: true }) }
// No menu item: as in Bilimbi, the status-bar brand link (`app-brand`) and
// sign-in reach the dashboard, and the sidebar holds no `nav-dashboard`.
