use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_grid::{Field, FieldType, GridContribution, Source, TableDecl};
use bilibi_module_registry::{Contribution, Provenance};

pub const LIST: &str = "admin.person.list";
pub const VIEW: &str = "admin.person.view";
pub const CREATE: &str = "admin.person.create";
pub const UPDATE: &str = "admin.person.update";
pub const RETIRE: &str = "admin.person.retire";
pub const SYSTEM_MANAGE: &str = "admin.person.system.manage";
/// Import contacts from CSV (`base/bulk_data`); each row still needs the
/// create, update and list capabilities its contracts check.
pub const IMPORT: &str = "admin.person.import";
/// Export the contacts list to CSV (`base/bulk_data`); the list contract
/// still checks `admin.person.list`, and restricted fields come out blank.
pub const EXPORT: &str = "admin.person.export";
/// Change a field on many contacts at once (`base/bulk_data`); each one still
/// goes through `core.person.update` and its capability and version check.
pub const BULK_UPDATE: &str = "admin.person.bulk-update";
pub const CAPS: &[&str] = &[
    LIST,
    VIEW,
    CREATE,
    UPDATE,
    RETIRE,
    SYSTEM_MANAGE,
    IMPORT,
    EXPORT,
    BULK_UPDATE,
];
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
inventory::submit! { Contribution::new(VOCABULARY, "core/person", Provenance { module: "core/person", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &[], verbs: &["retire"],
    capabilities: &[C::tenant(LIST), C::tenant(VIEW), C::tenant(CREATE), C::tenant(UPDATE), C::tenant(RETIRE), C::platform(SYSTEM_MANAGE), C::tenant(IMPORT), C::tenant(EXPORT), C::tenant(BULK_UPDATE)],
    roles: &[role("tenant_owner", "Tenant Owner", CAPS)],
    system_principals: &[],
}) }

// Register contacts now so restrictions apply to every API and audit read.
// K8 composes the page from this catalog instead of defining a second table.
static GRID: GridContribution = GridContribution {
    record_fields: &[bilibi_grid::RecordFields { table: "persons", fields: crate::web::fields::ALL }],
    tables: &[TableDecl::new("persons", "Contacts", LIST,
        Source::tenant("SELECT id,name,kind,CASE WHEN retired_at IS NULL THEN 'current' ELSE 'retired' END AS identity,email,phone,system_key,retired_at,version,created_at,updated_at,search_vector FROM persons WHERE tenant_id=$1"), "id", &[
            Field::new("id", "ID", FieldType::Integer).protected(),
            Field::new("name", "Name", FieldType::String).protected(),
            Field::new("identity", "Identity", FieldType::String).protected(),
            Field::choice("kind", "Kind", &["human", "agent"]).protected(),
            Field::new("email", "Personal email", FieldType::String),
            Field::new("phone", "Personal phone", FieldType::String),
            Field::new("system_key", "System identity", FieldType::String).protected(),
            Field::new("retired_at", "Retired", FieldType::DateTime).protected(),
            Field::new("version", "Version", FieldType::Integer).protected(),
            Field::new("created_at", "Created", FieldType::DateTime).protected(),
            Field::new("updated_at", "Updated", FieldType::DateTime).protected(),
            Field::new("search_vector", "Search terms", FieldType::String).hidden(),
        ]).label_field("name").time_field("created_at").record_types(&["person"])],
    links: &[],
};
bilibi_grid::contribute!("core/person", "person.grid", &GRID);

// Global search finds contacts by name, and by the start of an email or phone
// for a reader no held role restricts them for (as the Contacts search does).
static SEARCH: bilibi_search::Searchable =
    bilibi_search::Searchable::new("persons", "name", "search_vector", "/contacts/{id}")
        .subtitle(&["email", "phone"])
        .prefix(&["email", "phone"])
        .state("identity", &["retired"])
        .updated("updated_at");
bilibi_search::contribute!("core/person", "person.search", &SEARCH);

// Contacts import from CSV through the person contracts (issue 133). A row
// matches an existing contact on its email or phone. The Contacts list
// exports through the person list contract, and its ticked (or matching)
// contacts change kind together through the person update contract.
inventory::submit! { Contribution::new(bilibi_bulk_data::RECORD_TYPES, "core/person.contacts", Provenance { module: "core/person", source: file!(), line: line!() }, &bilibi_bulk_data::RecordType {
    label: "Contacts", table: "persons", path: Some("/contacts/{id}"),
    import: bilibi_bulk_data::Import {
        capability: IMPORT, create: "core.person.create", id: "id",
        duplicates: Some(bilibi_bulk_data::Duplicates { read: "core.person.duplicates", fields: &["email", "phone"], update: "core.person.update", update_id: "person_id", update_version: "version", update_fields: "identity" }),
        references: &[],
    },
    export: Some(bilibi_bulk_data::Export { capability: EXPORT, list: "core.person.list" }),
    bulk_update: Some(bilibi_bulk_data::BulkUpdate {
        capability: BULK_UPDATE, list: "core.person.list", get: "core.person.get", get_id: "person_id",
        update: "core.person.update", update_id: "person_id", update_version: "version", update_fields: "identity",
        fields: &["kind"],
    }),
}) }

// Core has a contact directory, with no employee-type or employment controls.
macro_rules! page {
    ($id:literal, $path:literal, $operation:literal, $cap:expr) => {
        inventory::submit! { Contribution::new(
        bilibi_module_registry::contributions::ROUTES, $id,
        Provenance { module: "core/person", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Route {
            method: bilibi_module_registry::contributions::Method::Get,
            path: $path, service: crate::web::SERVICE, operation: $operation,
            access: bilibi_module_registry::contributions::Access::Capability($cap), framed: true,
        }) }
    };
}
page!("person.list", "/contacts", "list", LIST);
page!("person.create", "/contacts/new", "create", CREATE);
page!("person.detail", "/contacts/{id}", "detail", VIEW);
// The directory's addresses before Bilimbi named it Contacts.
macro_rules! moved {
    ($id:literal, $from:literal, $to:literal) => {
        inventory::submit! { Contribution::new(
        bilibi_module_registry::contributions::MOVED, $id,
        Provenance { module: "core/person", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Moved { from: $from, to: $to }) }
    };
}
moved!("person.list.persons", "/persons", "/contacts");
moved!("person.create.persons", "/persons/new", "/contacts/new");
moved!("person.detail.persons", "/persons/{id}", "/contacts/{id}");
bilibi_menu::contribute!(
    "core/person",
    bilibi_menu::MenuItem::page("admin.contacts", "Contacts", "/contacts")
        .under("admin")
        .icon("user-group")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(15)
);

// Company, affiliation and account owners may mount a proven Person's personal
// contacts without copying contact markup or reading Person's table. `id` is a
// Person ID; a company ID must never be passed as one. Affiliation owns the
// company-to-person selection and its own company panel in K19.
inventory::submit! { Contribution::new(
bilibi_module_registry::contributions::EMBEDS, "person.contacts",
Provenance { module: "core/person", source: file!(), line: line!() },
&bilibi_module_registry::contributions::Embed {
    slot: "person.contacts", context: bilibi_module_registry::contributions::EmbedContext::Record { kind: "core/person" },
    service: crate::web::SERVICE, operation: "contact",
    access: bilibi_module_registry::contributions::Access::Capability(VIEW),
}) }
