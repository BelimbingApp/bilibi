//! The shell consumes a contributed bell; Base UI never imports Core User.
//! `/notifications` is the bell's full list.
//! The account's self-service pages are discovered routes served by
//! `core/user.pages`; Web establishes the session before dispatching.
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{Access, Embed, EmbedContext, Method, Route, EMBEDS, ROUTES},
    Contribution, Provenance,
};
use bilibi_settings::{Definition, ScopeKind, Setting, Validator, DEFINITIONS};

/// Belimbing's durable account-administration keys (Bilimbi
/// `Core.User.Contributions`). K9 asks `view` to read an account's links and
/// `update` for the link commands; K13/K16 bind the rest to their screens.
pub const LIST: &str = "admin.user.list";
pub const VIEW: &str = "admin.user.view";
pub const CREATE: &str = "admin.user.create";
pub const UPDATE: &str = "admin.user.update";
pub const DELETE: &str = "admin.user.delete";
const READ: &[&str] = &[LIST, VIEW];
const ALL: &[&str] = &[
    LIST,
    VIEW,
    CREATE,
    UPDATE,
    DELETE,
    crate::sso::config::MANAGE,
];
const fn role(
    code: &'static str,
    name: &'static str,
    description: &'static str,
    capabilities: &'static [&'static str],
) -> RoleDefinition {
    RoleDefinition {
        code,
        name,
        description,
        grant_all: false,
        capabilities,
    }
}
// Bilimbi's two user-management roles, and the tenant owner's share.
inventory::submit! { Contribution::new(VOCABULARY, "core/user", Provenance { module: "core/user", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &[], verbs: &[],
    capabilities: &[C::tenant(LIST), C::tenant(VIEW), C::tenant(CREATE), C::tenant(UPDATE), C::tenant(DELETE),C::tenant(crate::sso::config::MANAGE)],
    roles: &[
        role("user_viewer", "User Viewer", "Read-only access to user management.", READ),
        role("user_editor", "User Editor", "Read-write access to user management.", ALL),
        role("tenant_owner", "Tenant Owner", "Contributed system role; reconciled explicitly", ALL),
    ],
    system_principals: &[],
}) }

// Bilimbi's `admin.user` menu entry: Core User owns the navigation to the
// tenant-wide `/users` index, whose page User Administration contributes.
bilibi_menu::contribute!(
    "core/user",
    bilibi_menu::MenuItem::page("admin.user", "Users", "/users")
        .under("admin")
        .icon("users")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(30)
);

// What Core User puts in the Grid catalog (Bilimbi `Core.User.GridTables`):
// the `users` table, its link to the Person it names and the inbound
// many-link back to the accounts.
static GRID: bilibi_grid::GridContribution = bilibi_grid::GridContribution {
    record_fields: &[bilibi_grid::RecordFields { table: "users", fields: crate::record_fields::ALL }],
    tables: &[bilibi_grid::TableDecl::new("users", "Users", LIST,
        bilibi_grid::Source::tenant("SELECT u.id,u.name,i.email,i.email_verified_at,u.created_at,u.updated_at,u.person_id FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE u.tenant_id=$1 AND u.deleted_at IS NULL AND u.kind='staff'"), "id", &[
            bilibi_grid::Field::new("id", "ID", bilibi_grid::FieldType::Integer),
            bilibi_grid::Field::new("name", "Name", bilibi_grid::FieldType::String),
            bilibi_grid::Field::new("email", "Email", bilibi_grid::FieldType::String),
            bilibi_grid::Field::new("email_verified_at", "Email verified", bilibi_grid::FieldType::DateTime),
            bilibi_grid::Field::new("created_at", "Created", bilibi_grid::FieldType::DateTime),
            bilibi_grid::Field::new("updated_at", "Updated", bilibi_grid::FieldType::DateTime),
            bilibi_grid::Field::new("person_id", "Person", bilibi_grid::FieldType::Integer).hidden(),
        ]).label_field("name").time_field("created_at").record_types(&["user"])],
    links: &[
        bilibi_grid::LinkDecl::on("person", "Person", bilibi_grid::LinkKind::One, "users", "person_id", "persons", "id"),
        bilibi_grid::LinkDecl::on("accounts", "Accounts", bilibi_grid::LinkKind::Many, "persons", "id", "users", "person_id"),
    ],
};
bilibi_grid::contribute!("core/user", "user.grid", &GRID);

// The account administration pages (Bilimbi `FormLive`, `ShowLive`). The
// tenant-wide `/users` index is User Administration's (K13); these two are the
// create form and the read-first detail page it links to.
inventory::submit! {
    Contribution::new(ROUTES, "core/user.create", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method: Method::Get, path: "/users/new", service: "core/user.pages", operation: "create", access: Access::Capability(CREATE), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/user.detail", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method: Method::Get, path: "/users/{id}", service: "core/user.pages", operation: "detail", access: Access::Capability(VIEW), framed: true,
    })
}

// The signed-in account's own name, email and landing page (Bilimbi
// `ProfileLive`); the session alone names the account.
inventory::submit! {
    Contribution::new(ROUTES, "core/user.settings.profile", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method: Method::Get, path: "/settings/profile", service: "core/user.pages", operation: "profile", access: Access::SignedIn, framed: true,
    })
}
// Bilimbi `ui.landing_menu_id`: the menu page a sign-in opens, at the
// account's own scope; the profile page validates it against the menu the
// account can reach, since the definition can only bound its shape.
fn landing_accepts(value: &serde_json::Value) -> bool {
    value
        .as_str()
        .is_some_and(|id| id.len() <= 255 && !id.contains(char::is_control))
}
inventory::submit! {
    Contribution::new(DEFINITIONS, crate::accounts::LANDING_KEY, Provenance { module: "core/user", source: file!(), line: line!() }, &Setting::Definition(Definition {
        value_type: bilibi_settings::ValueType::String, scopes: &[ScopeKind::User], default: || serde_json::json!(""),
        nullable: false, encrypted: false, minimum: None, maximum: None,
        validator: Some(Validator { accepts: landing_accepts, message: "must name a menu page" }),
        editor: None,
    }))
}

inventory::submit! {
    Contribution::new(ROUTES, "core/user.settings.password", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method: Method::Get, path: "/settings/password", service: "core/user.pages", operation: "password", access: Access::SignedIn, framed: true,
    })
}

// Authenticator two-factor (`crate::two_factor`): the account's own setup
// page, and the step-up prompt the host's `Access::StepUp` guard sends a
// browser to. Signed in only: each acts on the session's own account.
inventory::submit! {
    Contribution::new(ROUTES, "core/user.settings.two_factor", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
        method: Method::Get, path: crate::TWO_FACTOR_PATH, service: "core/user.pages", operation: "two_factor", access: Access::SignedIn, framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/user.settings.two_factor.confirm", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Company,
        method: Method::Get, path: crate::CONFIRM_PATH, service: "core/user.pages", operation: "confirm_access", access: Access::SignedIn, framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/user.settings.appearance", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method: Method::Get, path: "/settings/appearance", service: "core/user.pages", operation: "appearance", access: Access::SignedIn, framed: true,
    })
}

// No capability: the page and its writes act only on the signed-in account
// (Bilimbi guards it by sign-in alone).
inventory::submit! {
    Contribution::new(ROUTES, "core/user.notifications.index", Provenance { module: "core/user", source: file!(), line: line!() }, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method: Method::Get, path: "/notifications", service: "core/user.pages", operation: "notifications", access: Access::SignedIn, framed: true,
    })
}

// The account panels other owners' pages mount by embed id (`crate::panels`;
// Bilimbi `company.users`, `employee.accounts`, `dashboard.users`,
// `dashboard.people`). A record panel's access is the host route's gate for
// the company one (read-only, as Bilimbi's) and `admin.user.view` for the
// Person one, whose link controls then prove `admin.user.update` per write.
// The dashboard embeds stay signed-in only: the entry's capability offers the
// widget and the panel shows an em dash without it (see `base/dashboard`).
macro_rules! embed {
    ($id:literal, $slot:literal, $context:expr, $operation:literal, $access:expr) => {
        inventory::submit! {
            Contribution::new(EMBEDS, $id, Provenance { module: "core/user", source: file!(), line: line!() }, &Embed {
                slot: $slot, context: $context, service: "core/user.pages", operation: $operation, access: $access,
            })
        }
    };
}
embed!(
    "user.company-users",
    "company.users",
    EmbedContext::Record {
        kind: "core/company"
    },
    "company_users",
    Access::Capability(bilibi_company::VIEW)
);
embed!(
    "user.person-accounts",
    "person.accounts",
    EmbedContext::Record {
        kind: "core/person"
    },
    "person_accounts",
    Access::Capability(VIEW)
);
embed!(
    "user.dashboard-users",
    "dashboard.users",
    EmbedContext::Dashboard,
    "dashboard_users",
    Access::SignedIn
);
embed!(
    "user.dashboard-people",
    "dashboard.people",
    EmbedContext::Dashboard,
    "dashboard_people",
    Access::SignedIn
);
bilibi_dashboard::contribute!(
    "core/user",
    bilibi_dashboard::Widget::new("base-dashboard-user-stats", "Users", "user.dashboard-users")
        .order(20)
        .requires(LIST)
        .refresh(60_000)
);
bilibi_dashboard::contribute!(
    "core/user",
    bilibi_dashboard::Widget::new(
        "recent-users",
        "People in this workspace",
        "user.dashboard-people"
    )
    .section()
    .order(20)
    .requires(LIST)
    .refresh(60_000)
);

// The top-bar bell. Web renders and mounts every `EmbedContext::Shell`
// contribution itself, keyed by the slot's element id (`app-notifications`).
inventory::submit! {
    Contribution::new(EMBEDS, "core/user.notifications", Provenance { module: "core/user", source: file!(), line: line!() }, &Embed {
        slot: "shell.notifications", context: EmbedContext::Shell, service: "core/user.pages", operation: "bell", access: Access::SignedIn,
    })
}

embed!(
    "user.company-desks",
    "company.desks",
    EmbedContext::Record {
        kind: "core/company"
    },
    "company_desks",
    Access::Capability(bilibi_company::VIEW)
);
embed!(
    "user.person-desks",
    "person.desks",
    EmbedContext::Record {
        kind: "core/person"
    },
    "person_desks",
    Access::Capability(bilibi_company::VIEW)
);
