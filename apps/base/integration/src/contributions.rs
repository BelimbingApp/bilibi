//! Integration's contributed vocabulary, retention and webhook settings,
//! scheduled payload retention and the audit redaction for manually deleted
//! exchanges.
use bilibi_audit::{Redaction, REDACTIONS};
use bilibi_authz::{CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;

/// Belimbing's durable keys cover both directions. Tenant-usable: an ordinary
/// tenant sees its own exchanges; only the operator scope widens visibility.
pub const LIST: &str = "admin.system.outbound-exchange.list";
pub const PAYLOAD: &str = "admin.system.outbound-exchange.payload.view";
pub const DELETE: &str = "admin.system.outbound-exchange.delete";
/// Global operator setting: days to keep redacted previews (metadata stays).
pub const RETENTION_DAYS: &str = "integration.payload_retention_days";
pub(crate) const RETENTION_DEFAULT: i64 = 30;
pub(crate) const RETENTION_MAX: i64 = 3650;

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/integration",
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! { Contribution::new(VOCABULARY, "base/integration", provenance!(), &VocabularyContribution {
    domains: &[],
    verbs: &[],
    capabilities: &[CapabilityDefinition::tenant(LIST), CapabilityDefinition::tenant(PAYLOAD), CapabilityDefinition::tenant(DELETE)],
    // Auditor and system viewer may list, never inspect payloads or delete.
    roles: &[
        RoleDefinition { code: "auditor", name: "Auditor", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[LIST] },
        RoleDefinition { code: "system_viewer", name: "System Viewer", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[LIST] },
    ],
    system_principals: &[],
}) }
inventory::submit! { Contribution::new(DEFINITIONS, RETENTION_DAYS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!(RETENTION_DEFAULT), nullable: false, encrypted: false,
    minimum: Some(1.0), maximum: Some(RETENTION_MAX as f64), validator: None,
    editor: Some(Editor { group: "Integration", label: "API exchange payload retention",
        help: "Days to keep the saved content of API calls; the call summary stays.", capability: Some(DELETE), reveal_duration_ms: 5_000, change_needs_step_up: false }),
})) }
// Deleting an exchange is audited with metadata only; these names never carry
// a second copy of a payload or a raw endpoint into the audit ledger.
inventory::submit! { Contribution::new(REDACTIONS, "base/integration.exchange", provenance!(), &Redaction {
    kind: "base/integration.exchange",
    fields: &["endpoint", "request_headers", "request_body", "response_headers", "response_body", "error_message"],
}) }

/// Platform-wide inbound webhook limits the host reads once per admission
/// window (Bilimbi `webhooks.*`): bytes per body, verified deliveries per
/// handler per window, attempts per sender per handler per window before
/// verification, the window length and the body read timeout.
pub const WEBHOOK_SETTINGS: WebhookLimits<&str> = WebhookLimits {
    max_bytes: "webhooks.max_bytes",
    rate_limit: "webhooks.rate_limit",
    sender_rate_limit: "webhooks.sender_rate_limit",
    window_ms: "webhooks.window_ms",
    read_timeout_ms: "webhooks.read_timeout_ms",
};
/// The five webhook settings, as keys or as values read together.
#[derive(Clone, Copy, Debug)]
pub struct WebhookLimits<T> {
    pub max_bytes: T,
    pub rate_limit: T,
    pub sender_rate_limit: T,
    pub window_ms: T,
    pub read_timeout_ms: T,
}
macro_rules! webhook_setting {
    ($key:expr, $default:literal, $max:literal, $label:literal, $help:literal) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, provenance!(), &Setting::Definition(Definition {
            value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!($default), nullable: false, encrypted: false,
            minimum: Some(1.0), maximum: Some($max), validator: None,
            editor: Some(Editor { group: "Webhooks", label: $label, help: $help, capability: None, reveal_duration_ms: 5_000, change_needs_step_up: false }),
        })) }
    };
}
webhook_setting!(
    WEBHOOK_SETTINGS.max_bytes,
    1_048_576,
    67_108_864.0,
    "Webhook body limit",
    "Maximum bytes per inbound webhook."
);
webhook_setting!(
    WEBHOOK_SETTINGS.rate_limit,
    120,
    1_000_000.0,
    "Webhook delivery limit",
    "Most webhooks each receiver accepts per rate window, on each server."
);
webhook_setting!(
    WEBHOOK_SETTINGS.sender_rate_limit,
    120,
    1_000_000.0,
    "Webhook sender limit",
    "Most webhook attempts one sender can make to each receiver per rate window, on each server."
);
webhook_setting!(
    WEBHOOK_SETTINGS.window_ms,
    60_000,
    3_600_000.0,
    "Webhook rate window",
    "Rate window in milliseconds."
);
webhook_setting!(
    WEBHOOK_SETTINGS.read_timeout_ms,
    15_000,
    120_000.0,
    "Webhook read timeout",
    "Longest wait, in milliseconds, to receive a webhook's content."
);
