use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_grid::{Field, FieldType, GridContribution, LinkDecl, LinkKind, Source, TableDecl};
use bilibi_module_registry::{Contribution, Provenance};

// Bilimbi `Address.Contributions`. Attaching an address to a company or a
// Person is that owner's update, not one of these (`store::authorize_owner`).
pub const VIEW: &str = "admin.address.view";
pub const LIST: &str = "admin.address.list";
pub const CREATE: &str = "admin.address.create";
pub const UPDATE: &str = "admin.address.update";
pub const DELETE: &str = "admin.address.delete";
const CAPS: &[&str] = &[VIEW, LIST, CREATE, UPDATE, DELETE];
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
inventory::submit! { Contribution::new(VOCABULARY, "core/address", Provenance { module: "core/address", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &[], verbs: &[],
    capabilities: &[C::tenant(VIEW), C::tenant(LIST), C::tenant(CREATE), C::tenant(UPDATE), C::tenant(DELETE)],
    roles: &[role("tenant_owner", "Tenant Owner", CAPS)],
    system_principals: &[],
}) }

// Bilimbi `Address.GridTables`: addresses with their country, and the links
// from owners to addresses. Address owns the attachments, so it declares the
// owner → address edges for both party kinds.
static GRID: GridContribution = GridContribution {
    record_fields: &[],
    tables: &[TableDecl::new("addresses", "Addresses", LIST,
        Source::tenant("SELECT id,label,line1,line2,locality,postcode,country_iso,phone,verification_status,created_at FROM addresses WHERE tenant_id=$1 AND deleted_at IS NULL"), "id", &[
            Field::new("id", "ID", FieldType::Integer).protected(),
            Field::new("label", "Label", FieldType::String).protected(),
            Field::new("line1", "Line 1", FieldType::String).protected(),
            Field::new("line2", "Line 2", FieldType::String).protected(),
            Field::new("locality", "Locality", FieldType::String).protected(),
            Field::new("postcode", "Postcode", FieldType::String).protected(),
            Field::new("country_iso", "Country code", FieldType::String).protected(),
            Field::new("phone", "Phone", FieldType::String),
            Field::choice("verification_status", "Verification", &["unverified", "suggested", "verified"]).protected(),
            Field::new("created_at", "Created", FieldType::DateTime).protected(),
        ]).label_field("label").time_field("created_at").record_types(&["address"]).page("/addresses/{key}")],
    links: &[
        LinkDecl::on("country", "Country", LinkKind::One, "addresses", "country_iso", "countries", "iso"),
        LinkDecl::edge("addresses", "Addresses", LinkKind::Many, "companies", "addresses",
            Source::tenant("SELECT t.organisation_id AS from_key, t.address_id AS to_key FROM address_attachments t JOIN addresses a ON a.tenant_id=t.tenant_id AND a.id=t.address_id WHERE t.tenant_id=$1 AND t.organisation_id IS NOT NULL AND a.deleted_at IS NULL")),
        // At most one per company: the attachment `primary` resolves to.
        LinkDecl::edge("primary_address", "Primary address", LinkKind::One, "companies", "addresses",
            Source::tenant("SELECT DISTINCT ON (t.organisation_id) t.organisation_id AS from_key, t.address_id AS to_key FROM address_attachments t JOIN addresses a ON a.tenant_id=t.tenant_id AND a.id=t.address_id WHERE t.tenant_id=$1 AND t.organisation_id IS NOT NULL AND a.deleted_at IS NULL ORDER BY t.organisation_id, t.is_primary DESC, t.priority, t.address_id")),
        LinkDecl::edge("addresses", "Addresses", LinkKind::Many, "persons", "addresses",
            Source::tenant("SELECT t.person_id AS from_key, t.address_id AS to_key FROM address_attachments t JOIN addresses a ON a.tenant_id=t.tenant_id AND a.id=t.address_id WHERE t.tenant_id=$1 AND t.person_id IS NOT NULL AND a.deleted_at IS NULL")),
    ],
};
bilibi_grid::contribute!("core/address", "address.grid", &GRID);

// Bilimbi `priv/web_routes.exs`: the administration pages, each behind its
// capability. The host checks the route's capability; every write asks again.
macro_rules! page {
    ($id:literal, $path:literal, $operation:literal, $cap:expr) => {
        inventory::submit! { Contribution::new(
        bilibi_module_registry::contributions::ROUTES, $id,
        Provenance { module: "core/address", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Route {
            method: bilibi_module_registry::contributions::Method::Get,
            path: $path, service: crate::web::SERVICE, operation: $operation,
            access: bilibi_module_registry::contributions::Access::Capability($cap), framed: true,
        }) }
    };
}
page!("address.list", "/addresses", "list", LIST);
page!("address.create", "/addresses/create", "create", CREATE);
page!("address.detail", "/addresses/{id}", "detail", VIEW);
bilibi_menu::contribute!(
    "core/address",
    bilibi_menu::MenuItem::page("admin.address", "Addresses", "/addresses")
        .under("admin")
        .icon("map-pin")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(40)
);

// The owner panels (Bilimbi `company.addresses`/`employee.addresses`). Company
// and Person detail discover them by record kind and pass the owner's ID as
// `company_id` or `person_id`; neither owner imports Address.
macro_rules! panel {
    ($id:literal, $kind:literal) => {
        inventory::submit! { Contribution::new(
        bilibi_module_registry::contributions::EMBEDS, $id,
        Provenance { module: "core/address", source: file!(), line: line!() },
        &bilibi_module_registry::contributions::Embed {
            slot: $id, context: bilibi_module_registry::contributions::EmbedContext::Record { kind: $kind },
            service: crate::web::SERVICE, operation: "addresses",
            access: bilibi_module_registry::contributions::Access::Capability(VIEW),
        }) }
    };
}
panel!("company.addresses", "core/company");
panel!("person.addresses", "core/person");
