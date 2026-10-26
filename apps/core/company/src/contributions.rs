//! What Company adds to the shared catalogs (Bilimbi
//! `Bilimbi.Core.Company.Contributions`): its capability vocabulary and the
//! roles that carry it, the Company pages' routes and the Companies menu
//! entry. Web owns dispatch and scope guards; a route here bypasses nothing.
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_grid::{Field, FieldType::*, GridContribution, LinkDecl, LinkKind, Source, TableDecl};
use bilibi_menu::{MenuItem, Requirement};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Provenance,
};

/// Belimbing's durable keys, shared by the pages, catalog and business APIs.
pub const VIEW: &str = "admin.company.view";
pub const LIST: &str = "admin.company.list";
pub const CREATE: &str = "admin.company.create";
pub const UPDATE: &str = "admin.company.update";
pub const DELETE: &str = "admin.company.delete";
/// Reach beyond the actor's own company to any company of the tenant. It
/// never authorizes an operation by itself.
pub const TENANT_WIDE: &str = "admin.company.tenant-wide.manage";
pub const CAPABILITIES: [&str; 6] = [VIEW, LIST, CREATE, UPDATE, DELETE, TENANT_WIDE];

macro_rules! provenance {
    () => {
        Provenance {
            module: "core/company",
            source: file!(),
            line: line!(),
        }
    };
}
const fn role(
    code: &'static str,
    name: &'static str,
    capabilities: &'static [&'static str],
) -> RoleDefinition {
    RoleDefinition {
        code,
        name,
        description: "Contributed system role; reconciled explicitly",
        grant_all: false,
        capabilities,
    }
}
// `tenant_owner` receives the whole set as the source grants it.
inventory::submit! { Contribution::new(VOCABULARY, "core/company", provenance!(), &VocabularyContribution {
    domains: &[], verbs: &[],
    capabilities: &[C::tenant(VIEW), C::tenant(LIST), C::tenant(CREATE), C::tenant(UPDATE), C::tenant(DELETE), C::tenant(TENANT_WIDE)],
    roles: &[role("tenant_owner", "Tenant Owner", &CAPABILITIES)],
    system_principals: &[],
}) }

inventory::submit! {
    Contribution::new(ROUTES, "core/company.list", provenance!(), &Route {
        method: Method::Get, path: "/companies", service: "core/company.pages", operation: "list", access: Access::Capability(LIST), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/company.detail", provenance!(), &Route {
        method: Method::Get, path: "/companies/{id}", service: "core/company.pages", operation: "detail", access: Access::Capability(VIEW), framed: true,
    })
}
bilibi_menu::contribute!(
    "core/company",
    MenuItem::page("admin.company", "Companies", "/companies")
        .icon("building-office-2")
        .under("admin")
        .requires(Requirement::Capability(LIST))
        .order(10)
);
// The two global catalogs sit under Companies, as the source's menu nests
// them; their routes are operator pages (Bilimbi `operator: true`), which the
// shell's menu reflects through the route's access.
bilibi_menu::contribute!(
    "core/company",
    MenuItem::page(
        "admin.company.department-type",
        "Department Types",
        "/companies/department-types"
    )
    .under("admin.company")
    .requires(Requirement::Capability(LIST))
    .order(10)
);
bilibi_menu::contribute!(
    "core/company",
    MenuItem::page(
        "admin.company.legal-entity-type",
        "Legal Entity Types",
        "/companies/legal-entity-types"
    )
    .under("admin.company")
    .requires(Requirement::Capability(LIST))
    .order(20)
);
inventory::submit! {
    Contribution::new(ROUTES, "core/company.legal_entity_types", provenance!(), &Route {
        method: Method::Get, path: "/companies/legal-entity-types", service: "core/company.pages", operation: "legal_entity_types", access: Access::OperatorCapability(LIST), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/company.department_types", provenance!(), &Route {
        method: Method::Get, path: "/companies/department-types", service: "core/company.pages", operation: "department_types", access: Access::OperatorCapability(LIST), framed: true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/company.departments", provenance!(), &Route {
        method: Method::Get, path: "/companies/{id}/departments", service: "core/company.pages", operation: "departments", access: Access::Capability(VIEW), framed: true,
    })
}

// The Companies catalog includes the Organisation facts. `record_types`
// names the audit kind a company is written under, so a restriction on a
// company field reaches the Audit screens and a record's History
// (`bilibi_authz::fields::withheld_by_record_type`).
static GRID: GridContribution = GridContribution {
    record_fields: &[bilibi_grid::RecordFields { table: "companies", fields: crate::web::fields::ALL }],
    tables: &[TableDecl::new(
        "companies",
        "Companies",
        LIST,
        Source::tenant(
            "SELECT id, parent_id, code, name, legal_name, registration_number, legal_entity_type_id, jurisdiction, website, status, email, phone, tax_id, revenue, created_at, updated_at, search_vector FROM companies WHERE tenant_id = $1 AND deleted_at IS NULL",
        ),
        "id",
        &[
            Field::new("id", "ID", Integer),
            Field::new("code", "Code", String).protected(),
            Field::new("legal_name", "Legal Name", String).protected(),
            Field::new("registration_number", "Registration Number", String),
            Field::new("jurisdiction", "Jurisdiction", String),
            Field::new("website", "Website", String),
            Field::new("parent_id", "Parent", Integer).hidden(),
            Field::new("legal_entity_type_id", "Legal entity type", Integer).hidden(),
            Field::new("name", "Name", String),
            Field::choice("status", "Status", &["active", "suspended", "pending", "archived"]).protected(),
            Field::new("email", "Email", String),
            Field::new("phone", "Phone", String),
            Field::new("tax_id", "Tax ID", String),
            Field::new("revenue", "Revenue", Integer),
            Field::new("created_at", "Created", DateTime),
            Field::new("updated_at", "Updated", DateTime),
            Field::new("search_vector", "Search terms", String).hidden(),
        ],
    )
    .label_field("name")
    .time_field("created_at")
    .record_types(&["company"])
    .page("/companies/{key}"),
    // A department is in the tenant through its company, so the source joins
    // the tenant's live companies (Bilimbi `Grid.DepartmentsSource`); the
    // head is the affiliation id, which the affiliation owner links.
    TableDecl::new(
        "departments",
        "Departments",
        LIST,
        Source::tenant(
            "SELECT d.id, d.company_id, d.head_affiliation_id AS head_id, d.status, t.code AS type_code, t.name AS type_name, d.created_at FROM company_departments d JOIN companies c ON c.tenant_id = d.tenant_id AND c.id = d.company_id AND c.deleted_at IS NULL LEFT JOIN company_department_types t ON t.id = d.department_type_id WHERE d.tenant_id = $1",
        ),
        "id",
        &[
            Field::new("id", "ID", Integer),
            Field::new("type_name", "Type", String),
            Field::new("type_code", "Type code", String),
            Field::choice("status", "Status", &["active", "inactive", "suspended"]),
            Field::new("created_at", "Created", DateTime),
            Field::new("company_id", "Company", Integer).hidden(),
            Field::new("head_id", "Head", Integer).hidden(),
        ],
    )
    .label_field("type_name")
    .time_field("created_at"),
    // Global reference data: no tenant parameter (Bilimbi `Grid.LegalEntityTypesSource`).
    TableDecl::new(
        "legal_entity_types",
        "Legal entity types",
        LIST,
        Source::global("SELECT id, code, name, is_active FROM company_legal_entity_types"),
        "id",
        &[
            Field::new("id", "ID", Integer),
            Field::new("code", "Code", String),
            Field::new("name", "Name", String),
            Field::new("is_active", "Active", Boolean),
        ],
    )
    .label_field("name")],
    links: &[
        LinkDecl::on("parent", "Parent company", LinkKind::One, "companies", "parent_id", "companies", "id"),
        LinkDecl::on("children", "Subsidiaries", LinkKind::Many, "companies", "id", "companies", "parent_id"),
        LinkDecl::on("legal_entity_type", "Legal entity type", LinkKind::One, "companies", "legal_entity_type_id", "legal_entity_types", "id"),
        LinkDecl::on("departments", "Departments", LinkKind::Many, "companies", "id", "departments", "company_id"),
        LinkDecl::on("company", "Company", LinkKind::One, "departments", "company_id", "companies", "id"),
    ],
};
bilibi_grid::contribute!("core/company", "company.grid", &GRID);

// Global search finds live companies by name, code and legal name; an archived
// one stays findable, ranked after current ones and marked.
static SEARCH: bilibi_search::Searchable =
    bilibi_search::Searchable::new("companies", "name", "search_vector", "/companies/{id}")
        .subtitle(&["code", "legal_name"])
        .state("status", &["archived"])
        .updated("updated_at");
bilibi_search::contribute!("core/company", "company.search", &SEARCH);

inventory::submit! {
    Contribution::new(ROUTES, "core/company.create", provenance!(), &Route {
        method: Method::Get, path: "/companies/create", service: "core/company.pages", operation: "create", access: Access::Capability(CREATE), framed: true,
    })
}

// Bilimbi gates the setup screen on `admin.company.update` and turns any
// other tenant away in its mount; here the route is an operator page, so the
// host refuses it before the page runs.
inventory::submit! {
    Contribution::new(ROUTES, "core/company.platform_operator_setup", provenance!(), &Route {
        method: Method::Get, path: "/setup/platform-operator", service: "core/company.pages", operation: "platform_operator_setup", access: Access::OperatorCapability(UPDATE), framed: true,
    })
}

// Belimbing's eleven common legal forms (`company.php` `legal_entity_types`).
// Codes stay lowercase snake_case, the style already stored for this catalog
// (`sdn_bhd`, `pte_ltd`). A repeat does not restore a row: the seed ledger
// skips an applied version, and `ON CONFLICT (code) DO NOTHING` leaves a
// code that was already there.
const LEGAL_ENTITY_TYPES_SEED: &str = "\
INSERT INTO company_legal_entity_types(code,name,description) VALUES
('sole_proprietorship','Sole Proprietorship','One person owns and runs the business.'),
('partnership','Partnership','Two or more people own the business and share its profits and debts.'),
('llp','Limited Liability Partnership','Partners own the business together, with limited personal risk.'),
('private_limited','Private Limited Company','Owners risk only what they put in, and the shares stay private.'),
('public_listed','Public Listed Company','Anyone can buy its shares on a stock exchange.'),
('public_unlisted','Public Unlisted Company','The public can own shares, but they are not traded on a stock exchange.'),
('corporation','Corporation','The law treats the company as separate from the people who own it.'),
('cooperative','Cooperative','Members own it and run it for their shared benefit.'),
('non_profit','Non-Profit Organisation','It serves a public purpose, such as charity or education, rather than paying its owners.'),
('government','Government Entity','A government body, or a company the government owns.'),
('branch','Branch Office','A local office of a company registered in another country.')
ON CONFLICT (code) DO NOTHING";

inventory::submit! { Contribution::new(bilibi_module_registry::contributions::SEEDS, "core/company.legal-entity-types", provenance!(), &bilibi_module_registry::contributions::Seed {
    version: "company-legal-entity-types-v1",
    mode: bilibi_module_registry::contributions::SeedMode::Production,
    action: bilibi_module_registry::contributions::SeedAction::Sql(LEGAL_ENTITY_TYPES_SEED),
}) }

// Development sample data (Bilimbi's `priv/dev_seed.exs`): the Engineering
// and Operations department types, and one department of each on the
// platform operator's primary company. Only `bilibi-web seed` runs it, on the
// bootstrap that creates the operator.
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::SEEDS, "core/company.development", provenance!(), &bilibi_module_registry::contributions::Seed {
    version: "company-development-sample",
    mode: bilibi_module_registry::contributions::SeedMode::Development,
    action: bilibi_module_registry::contributions::SeedAction::Sql("INSERT INTO company_department_types(code,name,category) VALUES('ENG','Engineering','operational'),('OPS','Operations','operational') ON CONFLICT (code) DO NOTHING;
INSERT INTO company_departments(tenant_id,company_id,department_type_id,status)
SELECT p.tenant_id,p.company_id,t.id,'active' FROM platform_operator o JOIN tenant_primary_companies p ON p.tenant_id=o.tenant_id CROSS JOIN company_department_types t WHERE t.code IN ('ENG','OPS') ON CONFLICT (company_id,department_type_id) DO NOTHING"),
}) }

// The dashboard entries (Bilimbi `dashboard.companies` and
// `dashboard.company`, `crate::web::dashboard`). Neither requires a
// capability, as Bilimbi's do not; each panel offers its link only to a
// reader the linked page admits.
macro_rules! dashboard_embed {
    ($id:literal, $slot:literal, $operation:literal) => {
        inventory::submit! {
            Contribution::new(bilibi_module_registry::contributions::EMBEDS, $id, provenance!(), &bilibi_module_registry::contributions::Embed {
                slot: $slot, context: bilibi_module_registry::contributions::EmbedContext::Dashboard, service: "core/company.pages", operation: $operation, access: Access::SignedIn,
            })
        }
    };
}
dashboard_embed!(
    "company.dashboard-companies",
    "dashboard.companies",
    "dashboard_companies"
);
dashboard_embed!(
    "company.dashboard-company",
    "dashboard.company",
    "dashboard_company"
);
bilibi_dashboard::contribute!(
    "core/company",
    bilibi_dashboard::Widget::new(
        "base-dashboard-company-stats",
        "Companies",
        "company.dashboard-companies"
    )
    .order(10)
    .refresh(60_000)
);
bilibi_dashboard::contribute!(
    "core/company",
    bilibi_dashboard::Widget::new(
        "current-company",
        "Your Company",
        "company.dashboard-company"
    )
    .section()
    .order(10)
    .refresh(60_000)
);

// A built-in report (read-only, copyable; base/reports): each reader counts
// only the companies their own access shows.
bilibi_reports::builtin!(
    "core/company",
    bilibi_reports::Builtin {
        key: "companies-by-status",
        name: "Companies by status",
        description: "How many companies are active, suspended, pending or archived.",
        record_type: "companies",
        kind: bilibi_reports::Kind::Summary,
        definition: || serde_json::json!({"groupings": [{"column": "status"}], "chart": "donut"}),
    }
);
