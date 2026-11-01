//! The generic settings editor at `/system/settings`. A separate module because the page needs Authz, which
//! itself depends on Settings: storage stays below Authz, the screen above it.
//! See `README.md` for the contract and the running proof.
mod form;
mod reveal;
mod web;

use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Db, Error};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Module, Provenance, Service,
};
use std::sync::Arc;

/// Route and platform-global writes; installation-global, so the persistent
/// operator identity is required as well as the grant.
pub const GLOBAL_MANAGE: &str = "base.settings.global.manage";
/// Offers the company picker; a company target is still proved per action.
pub const COMPANY_MANAGE: &str = "base.settings.company.manage";
pub const USER_MANAGE: &str = "base.settings.user.manage";
pub const SUPPORT_OVERRIDE_MANAGE: &str = "base.settings.support-override.manage";
/// Shows one stored encrypted value after Base Step-up. Granted by name
/// only: a `grant_all` role does not confer it.
pub const SECRET_VIEW: &str = "base.settings.secret.view";

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/settings_editor",
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "base/settings_editor", provenance!(), &VocabularyContribution {
    domains: &["base"],
    verbs: &[],
    capabilities: &[
        CapabilityDefinition::platform(GLOBAL_MANAGE),
        CapabilityDefinition::tenant(COMPANY_MANAGE),
        CapabilityDefinition::tenant(USER_MANAGE),
        CapabilityDefinition::tenant(SUPPORT_OVERRIDE_MANAGE),
        CapabilityDefinition::tenant(SECRET_VIEW),
    ],
    roles: &[],
    system_principals: &[],
}) }
inventory::submit! { Contribution::new(ROUTES, "settings.global", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: "/system/settings", service: "base/settings_editor.pages",
    operation: "global", access: Access::OperatorCapability(GLOBAL_MANAGE), framed: true,
}) }
// Same id, so pins and the menu inspector keep naming it.
bilibi_menu::contribute!(
    "base/settings_editor",
    bilibi_menu::MenuItem::page("admin.system.settings", "Settings", "/system/settings")
        .under("admin.system")
        .icon("settings-cog")
        .requires(bilibi_menu::Requirement::Capability(GLOBAL_MANAGE))
        .order(10)
);

inventory::submit! {
    Module { id: "base/settings_editor", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/audit", "base/settings", "base/step_up", "base/ui", "base/menu"],
        migrations: &[],
        services: &[Service { id: "base/settings_editor.pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<bilibi_tenancy::Scope, Error>(Arc::new(web::Pages::new(web::Core {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                settings: services.get("base/settings.store")?,
                boundary: services.get("base/database.mutations")?,
                parties: services.get("base/tenancy.parties")?,
                step_up: services.get(bilibi_step_up::SERVICE)?,
            })))))
        } }],
    }
}

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/settings_editor.pages", bilibi_module_registry::Provenance { module: "base/settings_editor", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/settings_editor.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "Saving and restoring settings write through the Settings facade; no contract yet." },
}) }
