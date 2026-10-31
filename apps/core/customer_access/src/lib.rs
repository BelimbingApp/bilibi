//! Core Customer Access (issue 137, ADR 16): each tenant's customer portal
//! site, the invitations that turn a Person into a customer account, the
//! staff page that manages both, and the portal's own home and account pages.
//! The host owns the portal's credential pages and asks this facade which
//! tenant an address names. Bilimbi has no customer portal. Contract:
//! `README.md`.
mod facade;
mod operations;
mod portal;
mod registration;
mod web;

pub use bilibi_user::CustomerAccount;
pub use facade::{
    address_shaped, AcceptError, Checked, CustomerAccess, FieldError, Invitation, InvitationInput,
    OpenInvitation, Site, SiteInput, Status, TITLE_MAX,
};
pub use registration::{RegisterError, Registration, RegistrationStatus, COMPANY_MAX};

use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_menu::{MenuItem, Requirement, PORTAL_ROOT};
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Layer, Module, Provenance, Service,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use bilibi_tenancy::{PortalScope, Scope};
use serde_json::json;
use std::sync::Arc;

/// The registry service of the facade.
pub const SERVICE: &str = "core/customer_access.access";
/// The staff page.
const PAGES: &str = "core/customer_access.pages";
/// The portal home and account pages.
const PORTAL_PAGES: &str = "core/customer_access.portal";

/// See the portal site and the invitations.
pub const VIEW: &str = "admin.customer-access.view";
/// Set the portal site, invite customers and revoke invitations.
pub const MANAGE: &str = "admin.customer-access.manage";
/// A customer's own portal home and account.
pub const PORTAL_ACCOUNT: &str = "portal.account.view";
/// The portal role every invited customer is given.
pub const PORTAL_ROLE: &str = "portal_customer";

/// How many days an invitation link works.
pub const INVITATION_DAYS_KEY: &str = "customer_portal.invitation_days";
/// How many days a request for an account waits for its address to be
/// confirmed (installation-wide: it is read before anyone is signed in).
pub const REGISTRATION_DAYS_KEY: &str = "customer_portal.registration_days";

pub const STAFF_PATH: &str = "/customer-access";
pub const HOME_PATH: &str = "/portal";
pub const ACCOUNT_PATH: &str = "/portal/account";

pub const MIGRATION: (&str, &str) = (
    "core-customer_access-001-portal",
    include_str!("../migrations/001-portal.sql"),
);
const SITE_ENABLED_MIGRATION: (&str, &str) = (
    "core-customer_access-002-site-enabled",
    include_str!("../migrations/002-site-enabled.sql"),
);
const REGISTRATION_MIGRATION: (&str, &str) = (
    "core-customer_access-003-registration",
    include_str!("../migrations/003-registration.sql"),
);

macro_rules! provenance {
    () => {
        Provenance {
            module: "core/customer_access",
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! {
    Module {
        id: "core/customer_access", layer: Layer::Core,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/settings", "base/mail", "base/ui", "base/agent_api", "base/menu", "base/party_ids", "base/datetime", "base/locale", "base/records", "core/company", "core/person", "core/affiliation", "core/user"],
        migrations: &[MIGRATION, SITE_ENABLED_MIGRATION, REGISTRATION_MIGRATION],
        services: &[
            Service { id: SERVICE, factory: |services| {
                let access = Arc::new(CustomerAccess {
                    db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                    mutations: services.get("base/database.mutations")?,
                    users: services.get("core/user.accounts")?,
                    grants: services.get("base/authz.grants")?,
                    authentication: services.get("base/tenancy.authentication")?,
                    mailer: services.get("base/mail.mailer")?,
                    settings: services.get("base/settings.store")?,
                    origin: std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),
                });
                operations::bind(access.clone())?;
                Ok(access)
            } },
            Service { id: PAGES, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<Scope, bilibi_database::Error>(Arc::new(web::Pages {
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                access: services.get(SERVICE)?,
                persons: services.get(bilibi_person::SERVICE)?,
                records: services.get(bilibi_records::SERVICE)?,
                affiliations: services.get(bilibi_affiliation::SERVICE)?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
            })))) },
            Service { id: PORTAL_PAGES, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService::<PortalScope, bilibi_database::Error>(Arc::new(portal::Pages {
                db: services.get::<bilibi_database::Db>("base/database.pool")?.as_ref().clone(),
                access: services.get(SERVICE)?,
            })))) },
        ],
    }
}

inventory::submit! { Contribution::new(VOCABULARY, "core/customer_access", provenance!(), &VocabularyContribution {
    domains: &["portal"],
    verbs: &[],
    capabilities: &[
        CapabilityDefinition::tenant(VIEW),
        CapabilityDefinition::tenant(MANAGE),
        CapabilityDefinition::portal(PORTAL_ACCOUNT),
    ],
    roles: &[
        RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW, MANAGE] },
        RoleDefinition { code: PORTAL_ROLE, name: "Portal customer", description: "Every customer an invitation admits: its own portal home and account", grant_all: false, capabilities: &[PORTAL_ACCOUNT] },
    ],
    system_principals: &[],
}) }

inventory::submit! { Contribution::new(DEFINITIONS, INVITATION_DAYS_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Tenant], default: || json!(7), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(60.0), validator: None,
    editor: Some(Editor { group: "Customer portal", label: "Invitation link lifetime (days)", help: "How long the link in a customer invitation works. A new invitation to the same address replaces the old link.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(DEFINITIONS, REGISTRATION_DAYS_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!(3), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(30.0), validator: None,
    editor: Some(Editor { group: "Customer portal", label: "Account request confirmation (days)", help: "How long the link that confirms a request for a customer account works. An unconfirmed request is dropped after that.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(ROUTES, "core/customer_access.staff", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: STAFF_PATH, service: PAGES, operation: "customer_access",
    access: Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "core/customer_access.home", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: HOME_PATH, service: PORTAL_PAGES, operation: "home",
    access: Access::Portal(PORTAL_ACCOUNT), framed: false,
}) }
inventory::submit! { Contribution::new(ROUTES, "core/customer_access.account", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
    method: Method::Get, path: ACCOUNT_PATH, service: PORTAL_PAGES, operation: "account",
    access: Access::Portal(PORTAL_ACCOUNT), framed: false,
}) }

// Every staff write goes through the contracts agents call.
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
    service: PAGES,
    contracts: &[
        ("save_site", operations::SITE_SET),
        ("invite", operations::INVITE),
        ("confirm_revoke", operations::REVOKE),
        ("resend", operations::INVITE),
        ("confirm_revoke_access", operations::ACCOUNT_REVOKE),
        ("restore_access", operations::ACCOUNT_RESTORE),
        ("invite", operations::APPROVE),
        ("confirm_reject", operations::REJECT),
    ],
    pending: Pending::None,
}) }
// The portal pages read only.
inventory::submit! { Contribution::new(PAGE_WRITES, PORTAL_PAGES, provenance!(), &PageWrites {
    service: PORTAL_PAGES,
    contracts: &[],
    pending: Pending::None,
}) }

bilibi_menu::contribute!(
    "core/customer_access",
    MenuItem::page("admin.customer_access", "Customer portal", STAFF_PATH)
        .under("admin")
        .icon("user-group")
        .requires(Requirement::Capability(VIEW))
        .order(57)
);
bilibi_menu::contribute!(
    "core/customer_access",
    MenuItem::page("portal.home", "Home", HOME_PATH)
        .under(PORTAL_ROOT)
        .icon("home")
        .requires(Requirement::Capability(PORTAL_ACCOUNT))
        .order(0)
);
bilibi_menu::contribute!(
    "core/customer_access",
    MenuItem::page("portal.account", "My account", ACCOUNT_PATH)
        .under(PORTAL_ROOT)
        .icon("user-circle")
        .requires(Requirement::Capability(PORTAL_ACCOUNT))
        .order(1000)
);
