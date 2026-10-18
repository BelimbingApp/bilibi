//! The Audit screens: `/audit/actions` and the dashboard's Recent Activity
//! panel (Bilimbi `Audit.Web`). The evidence itself, its capture and its reads
//! stay in `base/audit`.
//!
//! These screens live beside that crate rather than in it because of a cycle
//! Rust refuses and Elixir did not notice: Authz records its grants through
//! Audit (`base/authz` depends on `base/audit`), and a screen needs Authz to
//! ask "may this person see this" and the Dashboard, which also asks Authz.
//! Bilimbi's audit module carries its screens and learns Authz's answers through
//! an application-environment callback; here the screens are a module of their
//! own that depends downward on both. The audit capability vocabulary belongs
//! with them for the same reason: contributing a capability needs Authz's types.
//!
//! `/audit/mutations` and the `record.history` embed (P13) live here too; the
//! diff a mutation renders is shared between them (`diff`).
mod actions;
mod activity;
mod diff;
mod history;
mod mutations;
mod present;

use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, Error};
use bilibi_module_registry::{
    contributions::{Access, Embed, EmbedContext, Method, Route, EMBEDS, ROUTES},
    Contribution, Module, Provenance, Service,
};
use bilibi_tenancy::Scope;
use bilibi_ui::page::PageService;
use std::sync::Arc;

/// Opens the Audit screens and the dashboard feed.
pub const LIST: &str = "admin.audit.log.list";
/// Retains and releases an action. The control is shown only to holders, and
/// the write asks again.
pub const MANAGE: &str = "admin.audit.log.manage";
pub const VIEW: &str = "admin.audit.log.view";

const MODULE: &str = "base/audit_views";
macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

// Bilimbi `Audit.Contributions`: the `auditor` role holds all three and
// `system_viewer` may list and view. The roles exist in the Authz seed and
// extend by code; their names must match theirs.
inventory::submit! { Contribution::new(VOCABULARY, MODULE, provenance!(), &VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[CapabilityDefinition::tenant(LIST), CapabilityDefinition::tenant(MANAGE), CapabilityDefinition::tenant(VIEW)],
    roles: &[
        RoleDefinition { code: "auditor", name: "Auditor", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[LIST, MANAGE, VIEW] },
        RoleDefinition { code: "system_viewer", name: "System Viewer", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[LIST, VIEW] },
    ],
    system_principals: &[],
}) }

bilibi_menu::contribute!(
    "base/audit_views",
    bilibi_menu::MenuItem::container("admin.audit", "Audit Log")
        .icon("clipboard-document-list")
        .under("admin")
        .order(70)
);
bilibi_menu::contribute!(
    "base/audit_views",
    bilibi_menu::MenuItem::page("admin.audit.action", "Actions", "/audit/actions")
        .under("admin.audit")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(10)
);
bilibi_menu::contribute!(
    "base/audit_views",
    bilibi_menu::MenuItem::page("admin.audit.mutation", "Data Mutations", "/audit/mutations")
        .under("admin.audit")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(20)
);

inventory::submit! {
    Contribution::new(ROUTES, "base/audit_views.actions", provenance!(), &Route {
        method: Method::Get, path: "/audit/actions", service: "base/audit_views.pages",
        operation: "actions", access: Access::Capability(LIST), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "base/audit_views.mutations", provenance!(), &Route {
        method: Method::Get, path: "/audit/mutations", service: "base/audit_views.mutations",
        operation: "mutations", access: Access::Capability(LIST), framed: true,
    })
}

// A record page's History disclosure (see `history`). The kind is every
// record's: the host names the record's own audit kinds in the panel path.
inventory::submit! {
    Contribution::new(EMBEDS, "audit.record-history", provenance!(), &Embed {
        slot: "record.history", context: EmbedContext::Record { kind: "*" },
        service: "base/audit_views.history", operation: "record", access: Access::Capability(LIST),
    })
}

// The widget offers the card to holders of `LIST`; the embed is only
// `SignedIn`, so a card already on the page stays mounted when the grant goes
// and the panel itself skips the read (Bilimbi `web_routes.exs`).
inventory::submit! {
    Contribution::new(EMBEDS, "audit.dashboard-activity", provenance!(), &Embed {
        slot: "dashboard.activity", context: EmbedContext::Dashboard,
        service: "base/audit_views.activity", operation: "recent", access: Access::SignedIn,
    })
}
bilibi_dashboard::contribute!(
    "base/audit_views",
    bilibi_dashboard::Widget::new(
        "base-dashboard-recent-audit",
        "Recent Activity",
        "audit.dashboard-activity"
    )
    .size(bilibi_dashboard::Size::Medium)
    .order(30)
    .requires(LIST)
    .refresh(120_000)
);

inventory::submit! {
    Module { id: MODULE, layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/audit", "base/authz", "base/ui", "base/dashboard", "base/menu", "base/datetime", "base/locale"],
        migrations: &[],
        services: &[
            Service { id: "base/audit_views.pages", factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(actions::Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    boundary: services.get("base/database.mutations")?,
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: "base/audit_views.mutations", factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(mutations::Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: "base/audit_views.history", factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(history::Panels {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
            Service { id: "base/audit_views.activity", factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(activity::Panels {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    display: services.get("base/datetime.display")?,
                    locales: services.get("base/locale.locales")?,
                }))))
            } },
        ],
    }
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/audit_views.pages", bilibi_module_registry::Provenance { module: "base/audit_views", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/audit_views.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Exporting the action log writes through the Audit Views facade; no contract yet." },
}) }
