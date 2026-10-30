use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_grid::{
    Field, FieldType, GridContribution, LinkDecl, LinkKind, PersonalKind, Source, TableDecl,
};
use bilibi_module_registry::{Contribution, Provenance};

pub const LIST: &str = "admin.affiliation.list";
pub const VIEW: &str = "admin.affiliation.view";
pub const CREATE: &str = "admin.affiliation.create";
pub const UPDATE: &str = "admin.affiliation.update";
pub const UNLINK: &str = "admin.affiliation.unlink";
pub const PROVE: &str = "admin.affiliation.prove";
/// Import affiliations from CSV (`base/bulk_data`); each row still needs the
/// create capability, and reading the contacts it names.
pub const IMPORT: &str = "admin.affiliation.import";
pub const CAPS: &[&str] = &[LIST, VIEW, CREATE, UPDATE, UNLINK, PROVE, IMPORT];
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
inventory::submit! { Contribution::new(VOCABULARY, "core/affiliation", Provenance { module: "core/affiliation", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &[], verbs: &["unlink", "prove"],
    capabilities: &[C::tenant(LIST), C::tenant(VIEW), C::tenant(CREATE), C::tenant(UPDATE), C::tenant(UNLINK), C::tenant(PROVE), C::tenant(IMPORT)],
    roles: &[role("tenant_owner", "Tenant Owner", CAPS)],
    system_principals: &[],
}) }

// Affiliations import from CSV through `core.affiliation.create` (issue 133).
// The contact is a row of an earlier contacts import, by its source key, or
// is found by email; the organisation is a column of company ids or one
// fixed company (Company's duplicates read scores names, so it cannot be a
// lookup that must find exactly one).
inventory::submit! { Contribution::new(bilibi_bulk_data::RECORD_TYPES, "core/affiliation.affiliations", Provenance { module: "core/affiliation", source: file!(), line: line!() }, &bilibi_bulk_data::RecordType {
    label: "Affiliations", table: "person_affiliations", path: None,
    import: bilibi_bulk_data::Import {
        capability: IMPORT, create: "core.affiliation.create", id: "id",
        duplicates: None,
        references: &[bilibi_bulk_data::Reference {
            field: "person_id",
            parent: Some("core/person.contacts"),
            lookup: Some(bilibi_bulk_data::Lookup { read: "core.person.duplicates", by: "email", noun: "contact" }),
        }],
    },
}) }

// Work contacts share the same masking path as API results and audit evidence.
static GRID: GridContribution = GridContribution {
    record_fields: &[bilibi_grid::RecordFields { table: "persons", fields: crate::web::fields::ALL }],
    tables: &[TableDecl::new("person_affiliations", "Affiliations", LIST,
        Source::tenant("SELECT id,person_id,organisation_id,title,work_email,work_phone,effective_from,effective_to,unlinked_at,version,created_at,updated_at FROM person_affiliations WHERE tenant_id=$1"), "id", &[
            Field::new("id", "ID", FieldType::Integer).protected(),
            Field::new("person_id", "Person", FieldType::Integer).protected(),
            Field::new("organisation_id", "Organisation", FieldType::Integer).protected(),
            Field::new("title", "Role", FieldType::String).protected().personal(PersonalKind::FreeText),
            Field::new("work_email", "Work email", FieldType::String).personal(PersonalKind::Email),
            Field::new("work_phone", "Work phone", FieldType::String).personal(PersonalKind::Phone),
            Field::new("effective_from", "From", FieldType::Date).protected(),
            Field::new("effective_to", "To", FieldType::Date).protected(),
            Field::new("unlinked_at", "Unlinked", FieldType::DateTime).protected(),
            Field::new("version", "Version", FieldType::Integer).protected(),
            Field::new("created_at", "Created", FieldType::DateTime).protected(),
            Field::new("updated_at", "Updated", FieldType::DateTime).protected(),
        ]).time_field("created_at").record_types(&["affiliation"])], links: &[
        // Bilimbi Contacts work details, owned here rather than by Person.
        // Match the affiliation panel's inclusive current-date policy (UTC).
        LinkDecl::edge("work", "Work", LinkKind::Many, "persons", "person_affiliations",
            Source::tenant("SELECT person_id AS from_key,id AS to_key FROM person_affiliations WHERE tenant_id=$1 AND unlinked_at IS NULL AND (effective_from IS NULL OR effective_from <= (CURRENT_TIMESTAMP AT TIME ZONE 'UTC')::date) AND (effective_to IS NULL OR effective_to >= (CURRENT_TIMESTAMP AT TIME ZONE 'UTC')::date)")),
        LinkDecl::on("company", "Company", LinkKind::One, "person_affiliations", "organisation_id", "companies", "id"),
    ],
};
bilibi_grid::contribute!("core/affiliation", "affiliation.grid", &GRID);
