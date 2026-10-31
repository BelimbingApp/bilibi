//! Shared authenticated chrome. Owners supply already-authorized navigation and
//! account metadata; no identity or capability decision is made by a template.
//! Theme, time display and pins are the account's own (Core User); the
//! company shown is the one the session works in and the companies offered
//! are the ones the host says the person reaches (Web's `shell.rs`).
use crate::forms::{field_base_class, Field, InputKind};
use crate::Html;
use askama::Template;

#[derive(Clone, Debug)]
pub struct DisplayPreferences {
    pub theme: String,
    pub mode: String,
    pub timezone: String,
}
impl Default for DisplayPreferences {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            mode: "company".into(),
            timezone: "UTC".into(),
        }
    }
}
impl DisplayPreferences {
    /// The theme toggles as (value, label, pressed), in Bilimbi's order.
    pub fn themes(&self) -> [(&'static str, &'static str, bool); 3] {
        [("light", "Light"), ("dark", "Dark"), ("system", "System")]
            .map(|(value, label)| (value, label, self.theme == value))
    }
    /// The time-display choices as (value, label, detail, pressed), in
    /// Bilimbi's order; company time names its zone.
    pub fn modes(&self) -> [(&'static str, &'static str, &str, bool); 3] {
        [
            ("company", "Company time", self.timezone.as_str()),
            ("local", "This device's local time", ""),
            ("utc", "Stored UTC", ""),
        ]
        .map(|(value, label, detail)| (value, label, detail, self.mode == value))
    }
    pub fn mode_label(&self) -> &str {
        match self.mode.as_str() {
            "utc" => "UTC",
            "local" => "Local",
            _ => "Company",
        }
    }
}

/// A visible menu node. Build it from Menu::visible_tree, never the full catalog.
#[derive(Clone, Debug)]
pub struct NavNode {
    pub id: String,
    pub label: String,
    pub icon: String,
    pub route: Option<String>,
    pub active: bool,
    pub children: Vec<NavNode>,
}
impl NavNode {
    fn contains_active(&self) -> bool {
        self.active || self.children.iter().any(Self::contains_active)
    }
    fn render(&self, tileable: bool, pinnable: bool) -> askama::Result<Html> {
        let children = navigation_options(&self.children, tileable, pinnable)?;
        Html::render(&NavBranch {
            node: self,
            dom_id: self.id.replace('.', "-"),
            expanded: self.contains_active(),
            children,
            pinnable,
            tileable: tileable && self.route.as_deref() != Some("/workspace"),
        })
    }
}
pub fn navigation(nodes: &[NavNode], tileable: bool) -> askama::Result<Html> {
    navigation_options(nodes, tileable, true)
}
/// The shell's own rows without sidebar-only pin/tile controls, for specimens.
pub fn navigation_unpinned(nodes: &[NavNode]) -> askama::Result<Html> {
    navigation_options(nodes, false, false)
}
fn navigation_options(nodes: &[NavNode], tileable: bool, pinnable: bool) -> askama::Result<Html> {
    nodes
        .iter()
        .map(|node| node.render(tileable, pinnable))
        .collect()
}
#[derive(Template)]
#[template(path = "navigation.html")]
struct NavBranch<'a> {
    node: &'a NavNode,
    dom_id: String,
    expanded: bool,
    children: Html,
    tileable: bool,
    pinnable: bool,
}

/// One company the top bar's switcher offers: an explicit live desk the host
/// proved for the person. `current` marks the workplace of this request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompanyChoice {
    pub id: i64,
    pub name: String,
    pub current: bool,
}

/// Past this many companies the switcher adds a type-to-find box.
const COMPANY_FILTER_FROM: usize = 8;

/// The top bar's company switcher (`templates/company_bar.html`): the
/// company the session works in (`current`, `None` for no desk) and the
/// person's desks. With one desk, none, or a
/// tenant-wide page (`all_workplaces`), it is a plain label. The host supplies
/// `csrf` for the form and `path` as the address to return to after a switch.
pub fn company_bar(
    current: Option<&str>,
    companies: &[CompanyChoice],
    csrf: &str,
    path: &str,
    all_workplaces: bool,
) -> askama::Result<Html> {
    let filter = (companies.len() > COMPANY_FILTER_FROM).then(|| {
        // A plain text field: a search field would add the browser's own
        // clear "×" inside the narrow panel.
        Field::named("app-company-filter")
            .kind(InputKind::Text)
            .placeholder("Find a desk…")
            .aria_label("Find a desk")
            .autocomplete("off")
            .wrapper_class("mb-0")
            .class(field_base_class(false, "px-3"))
            .attr("data-company-filter", "")
            .attr("aria-controls", "app-company-options")
    });
    Html::render(&CompanyBar {
        current,
        companies,
        csrf,
        path,
        filter,
        all_workplaces,
    })
}
#[derive(Template)]
#[template(path = "company_bar.html")]
struct CompanyBar<'a> {
    current: Option<&'a str>,
    companies: &'a [CompanyChoice],
    csrf: &'a str,
    path: &'a str,
    filter: Option<Field>,
    all_workplaces: bool,
}

/// Display-only scope. The host must obtain operator standing from an explicit
/// proof; a tenant ID or administrator role never implies it.
pub struct ShellContext<'a> {
    pub user: &'a str,
    pub email: &'a str,
    pub tenant: &'a str,
    pub tenant_id: i64,
    pub user_id: i64,
    /// The company the session works in (`Scope::current_company`): the chosen,
    /// remembered or first live desk; `None` reads "No desk".
    pub current_company: Option<String>,
    /// Its id, stamped on the shell root as `data-company-id` so the live
    /// socket binds to the company this page was rendered under
    /// (`assets/live.js` sends it when it opens `/live`).
    pub company_id: Option<i64>,
    /// The top bar's company label or switcher, from `company_bar`.
    pub company_bar: Html,
    pub platform_operator: bool,
    /// The operator tenant stands on no primary company yet: the status bar
    /// links to `/setup/platform-operator` (Bilimbi
    /// `operator_company_missing`).
    pub operator_company_missing: bool,
    /// The operator's name while this session views as `user` (Bilimbi's
    /// `current_scope.impersonator`): draws the "Viewing as" strip with its
    /// exit, locks the display controls and pins, and names the operator in
    /// the account panel, so both acting identities stay on screen.
    pub impersonator: Option<&'a str>,
    pub csrf: &'a str,
    pub preferences: DisplayPreferences,
    pub navigation: Html,
    /// Core owns the bell. This slot permits its later embedded live panel.
    pub notifications: Html,
    /// The status bar's centre: the AI tag and its panel, Core Agent Access's
    /// `shell.ai` embed (`StatusbarTag`), empty when no module contributes it.
    pub ai: Html,
    pub home: &'a str,
    pub served_routes: String,
    /// The account's pins as JSON (`[{id,label,url,icon,sort_order}]`), already
    /// limited to pages this deployment serves.
    pub pins: String,
    /// The distinct icon names of those pins. The shell renders each as a
    /// `<template data-pin-icon>` glyph, so `assets/shell.js` can draw a pin
    /// with the icon it was stored with. The page-header pin stores none, so
    /// its pin borrows the glyph of the sidebar row for its address, if any.
    pub pin_icons: Vec<String>,
    /// The shared role-based top-bar reminder (Staging only).
    pub instance: Html,
    pub instance_title: String,
    /// The shared status-bar role and label, linked for a Core Administrator.
    pub instance_status: Html,
    /// The bind address of this server, shown after the role. The host supplies
    /// it only when the role is Dev.
    pub listen_address: Option<&'a str>,
    /// The Platform version first, then each mounted Domain or Extension, from
    /// `version_badges`.
    pub versions: Vec<VersionBadge>,
    /// The open page's controls in the top bar, left of the bell (Bilimbi's
    /// `topbar_actions` slot), from `Page::topbar_actions`.
    pub topbar_actions: Html,
    /// The page search dialog (`page_search`) over the pages the person may
    /// open, opened by the top bar's search button or `/`.
    pub page_search: Html,
}
impl ShellContext<'_> {
    pub fn initials(&self) -> String {
        self.user
            .split_whitespace()
            .take(2)
            .filter_map(|part| part.chars().next())
            .collect::<String>()
            .to_uppercase()
    }
}

/// One entry of the status bar's version list (`#app-version` for the
/// Platform, `#app-version-<container>` for a mounted Domain or Extension).
/// `text` is the short form shown; `title` says the commit and the modules.
pub struct VersionBadge {
    pub id: String,
    pub text: String,
    pub title: String,
}

/// The status bar's version badges for the program's composition (docs/versions.md):
/// `v0.1.0` for the Platform, then `finance 0.1.0` for each mounted container,
/// or its commit when it is at no release, or `unpinned` when the build has no pin.
pub fn version_badges(made: &bilibi_module_registry::Composition) -> Vec<VersionBadge> {
    use bilibi_module_registry::PinState;
    let mut title = format!("Bilibi {}", made.version);
    match (&made.pin, made.tag, made.short_commit()) {
        (PinState::Stale(reason), _, _) => {
            title.push_str(&format!(" · the pin is stale: {reason}"))
        }
        (PinState::Unpinned, _, commit) => {
            if let Some(commit) = commit {
                title.push_str(&format!(" · commit {commit}"));
            }
            title.push_str(" · built without a pin");
        }
        (PinState::Pinned, Some(tag), commit) => {
            title.push_str(&format!(" · release {tag}"));
            if let Some(commit) = commit {
                title.push_str(&format!(" · commit {commit}"));
            }
        }
        (PinState::Pinned, None, commit) => {
            title.push_str(" · unreleased");
            if let Some(commit) = commit {
                title.push_str(&format!(" · commit {commit}"));
            }
        }
    }
    let mut badges = vec![VersionBadge {
        id: "app-version".into(),
        text: format!("v{}", made.version),
        title,
    }];
    for container in &made.containers {
        let layer = match container.layer {
            bilibi_module_registry::Layer::Extensions => "Extension",
            _ => "Domain",
        };
        let (text, state) = match (container.version, container.short_commit()) {
            (Some(version), Some(commit)) => (
                format!("{} {version}", container.id),
                format!("version {version} · commit {commit}"),
            ),
            (None, Some(commit)) => (
                format!("{} {commit}", container.id),
                format!("at no release · commit {commit}"),
            ),
            (_, None) => (
                format!("{} unpinned", container.id),
                "mounted without a pin, so its version is unknown".to_owned(),
            ),
        };
        badges.push(VersionBadge {
            id: format!("app-version-{}", container.id),
            text,
            title: format!(
                "{layer} {} · {state} · modules: {}",
                container.id,
                container.modules.join(", ")
            ),
        });
    }
    badges
}

#[derive(Template)]
#[template(path = "shell.html")]
pub struct AppShell<'a> {
    pub context: &'a ShellContext<'a>,
    pub live: bool,
    pub body: &'a str,
}

#[derive(Template)]
#[template(path = "framed.html")]
pub struct Framed<'a> {
    pub body: &'a str,
    pub live: bool,
    pub tenant_id: i64,
    pub user_id: i64,
    /// The company the page was rendered under (`data-company-id`), as the
    /// full shell stamps it.
    pub company_id: Option<i64>,
    pub preferences: &'a DisplayPreferences,
    pub csrf: &'a str,
    pub impersonating: bool,
}
