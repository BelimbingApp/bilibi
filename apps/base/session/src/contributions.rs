//! What Session contributes: its two
//! capabilities and the system roles that list sessions, the operator
//! settings (lifetime, activity touch and the live socket limits), the Diagnostics menu entry, the `/system/sessions` page and the
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
/// The live socket boundary (Web's live transport, staff and portal alike):
/// open sockets per session, open sockets on one server, and messages one
/// session may send per minute. Each socket holds state, a refresh timer and
/// a subscription, and each message can reach Authz and an owner, so one
/// session is bounded before any of that work.
pub const LIVE_SOCKETS_PER_SESSION_KEY: &str = "session.live.sockets_per_session";
pub const LIVE_SOCKETS_TOTAL_KEY: &str = "session.live.sockets_total";
pub const LIVE_MESSAGES_PER_MINUTE_KEY: &str = "session.live.messages_per_minute";
pub const LIVE_SOCKETS_PER_CUSTOMER_KEY: &str = "session.live.sockets_per_customer";
pub const LIVE_CHUNKS_PER_MINUTE_KEY: &str = "session.live.chunks_per_minute";
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
// is required too.
inventory::submit! { Contribution::new(VOCABULARY, "base/session", provenance!(), &VocabularyContribution {
    capabilities: &[CapabilityDefinition::platform(LIST), CapabilityDefinition::platform(MANAGE)],
    roles: &[role("auditor", "Auditor"), role("system_viewer", "System Viewer")],
    domains: &[], verbs: &[], system_principals: &[],
}) }

const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];
inventory::submit! { Contribution::new(DEFINITIONS, TOUCH_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(5), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(60.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Session activity update interval (minutes)",
        help: "How often, in minutes, a signed-in user's last activity is saved.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, LIFETIME_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(120), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(525_600.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Session inactivity limit (minutes)",
        help: "Minutes of inactivity before a sign-in expires.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }

inventory::submit! { Contribution::new(ROUTES, "session.index", provenance!(), &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
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
inventory::submit! { Contribution::new(DEFINITIONS, LIVE_SOCKETS_PER_SESSION_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(16), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(1_000.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Open live pages per sign-in",
        help: "Most live pages (browser tabs) one sign-in may keep open on each server. One more is refused until another closes.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, LIVE_SOCKETS_TOTAL_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(5_000), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(1_000_000.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Open live pages per side per server",
        help: "Staff and customer portals each have this many places, so a full portal never uses staff places.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, LIVE_MESSAGES_PER_MINUTE_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(240), nullable: false, encrypted: false,
    minimum: Some(10.0), maximum: Some(100_000.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Live page actions per minute",
        help: "Most clicks, form changes and navigations one sign-in may send per minute, across its live pages, on each server. Excess ones are refused without acting.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, LIVE_SOCKETS_PER_CUSTOMER_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(16), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(1_000.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Open live pages per customer",
        help: "Most portal pages one customer may keep open across all their sign-ins on each server.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, LIVE_CHUNKS_PER_MINUTE_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(6_000), nullable: false, encrypted: false,
    minimum: Some(60.0), maximum: Some(1_000_000.0), validator: None,
    editor: Some(Editor { group: "Session", label: "Upload chunks per minute",
        help: "Most file chunks one sign-in may send across its live pages on each server. Exceeding this closes the sending page's connection.",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
