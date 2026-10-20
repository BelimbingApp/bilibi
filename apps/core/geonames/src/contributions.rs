//! What Geonames adds to shared catalogs: its capabilities, the Countries
//! grid table, the Geonames menu branch and the three index screens
//! (Bilimbi `Core.Geonames.Contributions` and `priv/web_routes.exs`).
use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_grid::{Field, FieldType::*, GridContribution, Source, TableDecl};
use bilibi_menu::{MenuItem, Requirement};
use bilibi_module_registry::{
    contributions::{Access, Method, Route, ROUTES},
    Contribution, Provenance,
};

/// Belimbing's durable keys. Reading is tenant-usable (the data is global and
/// harmless); editing a global table is an operator (platform) act.
pub const VIEW: &str = "admin.geonames.view";
pub const LIST: &str = "admin.geonames.list";
pub const UPDATE: &str = "admin.geonames.update";

pub const COUNTRIES_PATH: &str = "/geonames/countries";
pub const ADMIN1_PATH: &str = "/geonames/admin1";
pub const POSTCODES_PATH: &str = "/geonames/postcodes";

macro_rules! provenance {
    () => {
        Provenance {
            module: "core/geonames",
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "core/geonames", provenance!(), &VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(LIST), CapabilityDefinition::platform(UPDATE)],
    roles: &[
        RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW, LIST, UPDATE] },
    ],
    system_principals: &[],
}) }

// Global reference data: the source takes no tenant parameter. Rows are keyed
// by ISO code, which is how other tables point at a country.
static GRID: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new(
        "countries",
        "Countries",
        LIST,
        Source::global(
            "SELECT iso, iso3, country, capital, continent, population, area, currency_code, phone FROM geonames_countries",
        ),
        "iso",
        &[
            Field::new("iso", "ISO", String),
            Field::new("iso3", "ISO 3", String),
            Field::new("country", "Name", String),
            Field::new("capital", "Capital", String),
            Field::new("continent", "Continent", String),
            Field::new("population", "Population", Integer),
            Field::new("area", "Area", Float),
            Field::new("currency_code", "Currency", String),
            Field::new("phone", "Phone code", String),
        ],
    )
    .label_field("country")],
    links: &[],
};
bilibi_grid::contribute!("core/geonames", "geonames.grid", &GRID);

// The screens: each is a framed live page behind `admin.geonames.list`, as
// Bilimbi's route manifest declares. Writes ask `admin.geonames.update` again
// at the moment of each call.
inventory::submit! { Contribution::new(ROUTES, "geonames.countries", provenance!(), &Route {
    method: Method::Get, path: COUNTRIES_PATH, service: crate::PAGES_SERVICE, operation: "countries",
    access: Access::Capability(LIST), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "geonames.admin1", provenance!(), &Route {
    method: Method::Get, path: ADMIN1_PATH, service: crate::PAGES_SERVICE, operation: "admin1",
    access: Access::Capability(LIST), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "geonames.postcodes", provenance!(), &Route {
    method: Method::Get, path: POSTCODES_PATH, service: crate::PAGES_SERVICE, operation: "postcodes",
    access: Access::Capability(LIST), framed: true,
}) }

// The Administration › Geonames branch. The branch itself carries the
// capability; its pages inherit visibility from it.
bilibi_menu::contribute!(
    "core/geonames",
    MenuItem::container("admin.geonames", "Geonames")
        .under("admin")
        .icon("globe-alt")
        .requires(Requirement::Capability(LIST))
        .order(50)
);
bilibi_menu::contribute!(
    "core/geonames",
    MenuItem::page("admin.geonames.country", "Countries", COUNTRIES_PATH)
        .under("admin.geonames")
        .icon("flag")
        .order(10)
);
bilibi_menu::contribute!(
    "core/geonames",
    MenuItem::page(
        "admin.geonames.admin1-division",
        "Admin1 Divisions",
        ADMIN1_PATH
    )
    .under("admin.geonames")
    .icon("map")
    .order(20)
);
bilibi_menu::contribute!(
    "core/geonames",
    MenuItem::page("admin.geonames.postcode", "Postcodes", POSTCODES_PATH)
        .under("admin.geonames")
        .icon("map-pin")
        .order(30)
);
