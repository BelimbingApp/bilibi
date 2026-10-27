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
/// Have a report delivered on a schedule.
pub const SCHEDULE: &str = "reports.schedule";

pub const PAGE_SIZE_KEY: &str = "reports.page_size";
pub const MAX_GROUPS_KEY: &str = "reports.max_groups";
pub const EXPORT_ROWS_KEY: &str = "reports.export_rows";
pub const QUEUED_ROWS_KEY: &str = "reports.queued_export_rows";
pub const EXPORT_DAYS_KEY: &str = "reports.export_days";
pub const MAX_SUBSCRIPTIONS_KEY: &str = "reports.max_subscriptions";
pub const STATEMENT_MS_KEY: &str = "reports.statement_ms";
pub const MAX_COST_KEY: &str = "reports.max_cost";
pub const QUEUED_STATEMENT_MS_KEY: &str = "reports.queued_statement_ms";
pub const QUEUED_MAX_COST_KEY: &str = "reports.queued_max_cost";

inventory::submit! { Contribution::new(VOCABULARY, "base/reports", Provenance { module: "base/reports", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &["reports"],
    verbs: &["view", "author", "share", "export", "manage", "schedule"],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(AUTHOR), CapabilityDefinition::tenant(SHARE), CapabilityDefinition::tenant(EXPORT), CapabilityDefinition::tenant(MANAGE), CapabilityDefinition::tenant(SCHEDULE)],
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
// Grid's `page_keys` reads 1 to 100 rows a page.
bound!(
    PAGE_SIZE_KEY,
    50,
    10,
    100,
    "Rows per page",
    "How many rows a list report shows at a time."
);
bound!(MAX_GROUPS_KEY, 500, 10, bilibi_grid::MAX_RESULT_GROUPS, "Most groups in a summary", "A summary with more groups than this is refused; the reader adds a filter or groups more coarsely.");
// An export reads every page, and a page starts at most at Grid's offset bound.
bound!(
    EXPORT_ROWS_KEY,
    1_000,
    100,
    bilibi_grid::MAX_OFFSET,
    "Most rows in a download",
    "A larger list report is prepared in the background instead, and the reader is told when it is ready."
);
// A prepared export pages through Grid too, so it stops at the same offset bound.
bound!(
    QUEUED_ROWS_KEY,
    bilibi_grid::MAX_OFFSET,
    100,
    bilibi_grid::MAX_OFFSET,
    "Most rows in a prepared export",
    "A larger result is refused even in the background; the reader adds a filter first."
);
bound!(
    EXPORT_DAYS_KEY,
    7,
    1,
    90,
    "Days a prepared export is kept",
    "After this many days a prepared export is deleted and its link stops working."
);
bound!(
    MAX_SUBSCRIPTIONS_KEY,
    20,
    1,
    200,
    "Scheduled reports per person",
    "How many report schedules one person may hold."
);
// Read bounds. The interactive pair defaults to Grid's own bound, so a report
// on a page is no heavier than a list; prepared exports and deliveries run in
// the background and may take the larger pair.
bound!(
    STATEMENT_MS_KEY,
    2_000,
    500,
    30_000,
    "Report time bound (ms)",
    "How long one read of a report on a page may run before it is stopped."
);
bound!(
    MAX_COST_KEY,
    100_000,
    1_000,
    bilibi_grid::MAX_COST_BOUND,
    "Report cost bound",
    "The planner cost above which a report on a page is refused before it runs; the reader can prepare it as an export instead."
);
bound!(
    QUEUED_STATEMENT_MS_KEY,
    60_000,
    1_000,
    bilibi_grid::MAX_STATEMENT_MS,
    "Background report time bound (ms)",
    "How long one read of a prepared export or scheduled delivery may run."
);
bound!(
    QUEUED_MAX_COST_KEY,
    10_000_000,
    1_000,
    bilibi_grid::MAX_COST_BOUND,
    "Background report cost bound",
    "The planner cost above which a prepared export or scheduled delivery is refused."
);
