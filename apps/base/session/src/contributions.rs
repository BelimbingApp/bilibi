//! What Session contributes (Bilimbi `Session.Contributions`): its two
//! capabilities and the system roles that list sessions, the two operator
//! settings, the Diagnostics menu entry, the `/system/sessions` page and the
//! dashboard stats widget. The scheduled sweep is declared in `expiry.rs`.
use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_dashboard::Widget;
use bilibi_module_registry::{
    contributions::{Access, Embed, EmbedContext, Method, Route, EMBEDS, ROUTES},
    Contribution, Provenance,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;

pub const LIST: &str = "admin.system.session.list";
pub const MANAGE: &str = "admin.system.session.manage";
pub const LIFETIME_KEY: &str = "session.lifetime_minutes";
pub const TOUCH_KEY: &str = "session.last_activity_touch_minutes";
pub const PATH: &str = "/system/sessions";
/// The dashboard entry id is stored in each account's arrangement: never rename.
pub const WIDGET_ID: &str = "base-dashboard-session-stats";
pub const EMBED: &str = "session.dashboard-stats";

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/session",
            source: file!(),
            line: line!(),
        }
    };
}
const fn role(code: &'static str, name: &'static str) -> RoleDefinition {
    RoleDefinition {
        code,
        name,
        description: "Contributed system role; reconciled explicitly",
        grant_all: false,
        capabilities: &[LIST],
    }
}
// Operator pages: a grant alone is not enough, the platform operator identity
// is required too, as Bilimbi's `platform_capabilities` says.
inventory::submit! { Contribution::new(VOCABULARY, "base/session", provenance!(), &VocabularyContribution {
    capabilities: &[CapabilityDefinition::platform(LIST), CapabilityDefinition::platform(MANAGE)],
    roles: &[role("auditor", "Auditor"), role("system_viewer", "System Viewer")],
    domains: &[], verbs: &[], system_principals: &[],
}) }

const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];
inventory::submit! { Contribution::new(DEFINITIONS, TOUCH_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(5), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(60.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Session activity update interval",
        help: "How often, in minutes, a signed-in user's last activity is saved.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000, change_needs_step_up: false }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, LIFETIME_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(120), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(525_600.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Session lifetime",
        help: "Minutes of inactivity before a sign-in expires.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000, change_needs_step_up: false }),
})) }

inventory::submit! { Contribution::new(ROUTES, "session.index", provenance!(), &Route {
    method: Method::Get, path: PATH, service: "base/session.pages", operation: "index",
    access: Access::OperatorCapability(LIST), framed: true,
}) }
bilibi_menu::contribute!(
    "base/session",
    bilibi_menu::MenuItem::page("admin.system.session", "Sessions", PATH)
        .under("admin.system.diagnostics")
        .requires(bilibi_menu::Requirement::Capability(LIST))
        .order(50)
);

// The catalogue capability offers the widget. Leaving it off the embed keeps a
// card that is already on the page mounted after the grant is removed; the
// panel then shows an em dash and skips the read.
bilibi_dashboard::contribute!(
    "base/session",
    Widget::new(WIDGET_ID, "Sessions", EMBED)
        .order(40)
        .requires(LIST)
        .refresh(60_000)
);
inventory::submit! { Contribution::new(EMBEDS, EMBED, provenance!(), &Embed {
    slot: "dashboard.sessions", context: EmbedContext::Dashboard,
    service: "base/session.pages", operation: "dashboard-stats", access: Access::SignedIn,
}) }
