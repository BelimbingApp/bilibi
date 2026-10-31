//! Bulk Data's vocabulary, settings, worker and menu entry.
use crate::{
    export, import, provenance, update, CREATE, EXPORT_MAX_ROWS, FILE_DAYS, MAX_BYTES, MAX_ROWS,
    UPDATE_MAX_RECORDS, VIEW, WORKER,
};
use bilibi_authz::{CapabilityDefinition as C, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::Contribution;
use bilibi_queue::{
    execution::{HandlerBinding, HANDLERS},
    Worker, WORKERS,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;

inventory::submit! { Contribution::new(VOCABULARY, "base/bulk_data", provenance!(), &VocabularyContribution {
    domains: &[],
    // Each record type's own import, export and bulk-update capabilities use
    // them (`admin.person.import`, `admin.person.export`,
    // `admin.person.bulk-update`).
    verbs: &["import", "export", "bulk-update"],
    capabilities: &[C::tenant(VIEW), C::tenant(CREATE)],
    roles: &[RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW, CREATE] }],
    system_principals: &[],
}) }

const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];
inventory::submit! { Contribution::new(DEFINITIONS, MAX_BYTES, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(10_485_760), nullable: false, encrypted: false,
    minimum: Some(1024.0), maximum: Some(bilibi_ui::upload::MAX_FILE_SIZE as f64), validator: None,
    editor: Some(Editor { group: "Bulk data", label: "Largest import file", help: "Bytes one CSV file may hold. Private documents' own limit (artifacts.max_bytes) applies too.", capability: None, reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, MAX_ROWS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(10_000), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(100_000.0), validator: None,
    editor: Some(Editor { group: "Bulk data", label: "Rows per import", help: "The most data rows one CSV file may hold; split a longer file.", capability: None, reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(DEFINITIONS, EXPORT_MAX_ROWS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(10_000), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(100_000.0), validator: None,
    editor: Some(Editor { group: "Bulk data", label: "Rows per export", help: "The most records one CSV export takes; a longer list is refused, to be narrowed.", capability: None, reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, UPDATE_MAX_RECORDS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(1_000), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(10_000.0), validator: None,
    editor: Some(Editor { group: "Bulk data", label: "Records per bulk update", help: "The most records one bulk update changes; more ticked or matching records are refused, to be narrowed.", capability: None, reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, FILE_DAYS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(30), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(3650.0), validator: None,
    editor: Some(Editor { group: "Bulk data", label: "Days run files are kept", help: "How long an uploaded import file and an exported file are kept. Private documents' own retention (artifacts.retention_days) is the longest either is kept.", capability: None, reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 10, validate: import::arguments }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: import::SERVICE, capability: CREATE }) }
inventory::submit! { Contribution::new(WORKERS, export::WORKER, provenance!(), &Worker { max_attempts: 5, validate: export::arguments }) }
inventory::submit! { Contribution::new(HANDLERS, export::WORKER, provenance!(), &HandlerBinding { service: export::SERVICE, capability: VIEW }) }
inventory::submit! { Contribution::new(WORKERS, update::WORKER, provenance!(), &Worker { max_attempts: 10, validate: update::arguments }) }
inventory::submit! { Contribution::new(HANDLERS, update::WORKER, provenance!(), &HandlerBinding { service: update::SERVICE, capability: VIEW }) }

bilibi_menu::contribute!(
    "base/bulk_data",
    bilibi_menu::MenuItem::page("admin.imports", "Imports", crate::web::INDEX)
        .under("admin")
        .icon("arrow-up-tray")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(90)
);
