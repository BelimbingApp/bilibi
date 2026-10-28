//! Bulk Data's vocabulary, settings, worker and menu entry.
use crate::{import, provenance, CREATE, MAX_BYTES, MAX_ROWS, VIEW, WORKER};
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
    // Each record type's own import capability uses it (`admin.person.import`).
    verbs: &["import"],
    capabilities: &[C::tenant(VIEW), C::tenant(CREATE)],
    roles: &[RoleDefinition { code: "tenant_owner", name: "Tenant Owner", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW, CREATE] }],
    system_principals: &[],
}) }

const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];
inventory::submit! { Contribution::new(DEFINITIONS, MAX_BYTES, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(10_485_760), nullable: false, encrypted: false,
    minimum: Some(1024.0), maximum: Some(bilibi_ui::upload::MAX_FILE_SIZE as f64), validator: None,
    editor: Some(Editor { group: "Imports", label: "Largest import file", help: "Bytes one CSV file may hold. Private documents' own limit (artifacts.max_bytes) applies too.", capability: None, reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, MAX_ROWS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(10_000), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(100_000.0), validator: None,
    editor: Some(Editor { group: "Imports", label: "Rows per import", help: "The most data rows one CSV file may hold; split a longer file.", capability: None, reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 10, validate: import::arguments }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: import::SERVICE, capability: CREATE }) }

bilibi_menu::contribute!(
    "base/bulk_data",
    bilibi_menu::MenuItem::page("admin.imports", "Imports", crate::web::INDEX)
        .under("admin")
        .icon("arrow-up-tray")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(90)
);
