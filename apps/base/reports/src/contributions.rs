//! What Reports declares to the platform: its capabilities and the bounds an
//! operator sets for the installation, and overrides per company. No Bilimbi counterpart.
use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;

/// Run the reports shared with you.
pub const VIEW: &str = "reports.view";
/// Build reports, and change or delete your own.
pub const AUTHOR: &str = "reports.author";
/// Share your own reports with everyone or with roles.
pub const SHARE: &str = "reports.share";
/// Download a report's result as a file.
pub const EXPORT: &str = "reports.export";
/// Set the report bounds.
pub const MANAGE: &str = "reports.manage";

pub const PAGE_SIZE_KEY: &str = "reports.page_size";
pub const MAX_GROUPS_KEY: &str = "reports.max_groups";
pub const EXPORT_ROWS_KEY: &str = "reports.export_rows";

inventory::submit! { Contribution::new(VOCABULARY, "base/reports", Provenance { module: "base/reports", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &["reports"],
    verbs: &["view", "author", "share", "export", "manage"],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(AUTHOR), CapabilityDefinition::tenant(SHARE), CapabilityDefinition::tenant(EXPORT), CapabilityDefinition::tenant(MANAGE)],
    roles: &[],
    system_principals: &[],
}) }

macro_rules! bound {
    ($key:expr, $default:expr, $min:expr, $max:expr, $label:expr, $help:expr) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, Provenance { module: "base/reports", source: file!(), line: line!() }, &Setting::Definition(Definition {
            value_type: ValueType::Integer, scopes: &[ScopeKind::Global, ScopeKind::Company],
            default: || json!($default), nullable: false, encrypted: false,
            minimum: Some($min as f64), maximum: Some($max as f64), validator: None,
            editor: Some(Editor { group: "Reports", label: $label, help: $help, capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
        })) }
    };
}
bound!(
    PAGE_SIZE_KEY,
    50,
    10,
    200,
    "Rows per page",
    "How many rows a list report shows at a time."
);
bound!(MAX_GROUPS_KEY, 500, 10, bilibi_grid::MAX_RESULT_GROUPS, "Most groups in a summary", "A summary with more groups than this is refused; the reader adds a filter or groups more coarsely.");
// An export reads every page, and a page starts at most at Grid's offset bound.
bound!(
    EXPORT_ROWS_KEY,
    5_000,
    100,
    bilibi_grid::MAX_OFFSET,
    "Most rows in an export",
    "A list report with more rows than this cannot be downloaded; the reader adds a filter first."
);
