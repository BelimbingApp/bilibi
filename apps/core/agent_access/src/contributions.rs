//! Operator settings (Administration → Settings → Operator, P3), the platform
//! capability that edits them, and audit redaction for the stored digests.
use bilibi_audit::{Redaction, REDACTIONS};
use bilibi_authz::{CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_module_registry::{
    contributions::{Access, Method, Moved, Route, MOVED, ROUTES},
    Contribution, Provenance,
};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;

/// Operator settings are platform-only: a grant alone is not enough, the
/// persistent operator identity is required too.
pub const MANAGE: &str = "admin.system.agent-access.manage";
macro_rules! provenance {
    () => {
        Provenance {
            module: "core/agent_access",
            source: file!(),
            line: line!(),
        }
    };
}
const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];

inventory::submit! { Contribution::new(VOCABULARY, "core/agent_access", provenance!(), &VocabularyContribution {
    capabilities: &[CapabilityDefinition::platform(MANAGE)], domains: &[], verbs: &[], roles: &[], system_principals: &[],
}) }

inventory::submit! { Contribution::new(DEFINITIONS, crate::CONNECTION_DAYS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(90), nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(365.0), validator: None,
    editor: Some(Editor { group: "Agent access", label: "Agent connection length (days)", help: "How long new agent connections last; existing ones keep their end dates.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, crate::AUTHORIZATION_CODE_SECONDS, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(120), nullable: false, encrypted: false, minimum: Some(30.0), maximum: Some(600.0), validator: None,
    editor: Some(Editor { group: "Agent access", label: "OAuth authorization code lifetime", help: "Seconds an app has to use its sign-in code after you approve it.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, crate::OAUTH_REQUESTS_PER_MINUTE, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(60), nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(10_000.0), validator: None,
    editor: Some(Editor { group: "Agent access", label: "OAuth requests per minute", help: "Most connection requests one address can make per minute, on each server.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }

// Audit never receives a digest from this module, and the policy also masks the
// field names should a later change capture a whole row.
inventory::submit! { Contribution::new(REDACTIONS, "core/agent_access.devices", provenance!(), &Redaction { kind: "agent_device", fields: &["device_hash", "user_hash"] }) }
inventory::submit! { Contribution::new(REDACTIONS, "core/agent_access.tokens", provenance!(), &Redaction { kind: "agent_token", fields: &["token_hash"] }) }
inventory::submit! { Contribution::new(REDACTIONS, "core/agent_access.codes", provenance!(), &Redaction { kind: "agent_authorization_code", fields: &["code_hash", "code_challenge"] }) }

// The three pages (E4). Every signed-in person may open them; `AgentAccess`
// keeps the self-service gate. The consent pages are never framed.
inventory::submit! { Contribution::new(ROUTES, "core/agent_access.device", provenance!(), &Route {
    method: Method::Get, path: "/oauth/device", service: "core/agent_access.pages", operation: "device", access: Access::SignedIn, framed: false,
}) }
inventory::submit! { Contribution::new(ROUTES, "core/agent_access.authorize", provenance!(), &Route {
    method: Method::Get, path: "/oauth/authorize", service: "core/agent_access.pages", operation: "authorize", access: Access::SignedIn, framed: false,
}) }
inventory::submit! { Contribution::new(ROUTES, "core/agent_access.connections", provenance!(), &Route {
    method: Method::Get, path: "/settings/ai-connections", service: "core/agent_access.pages", operation: "connections", access: Access::SignedIn, framed: true,
}) }
// The MCP page and the connections list are one tab. Old addresses keep working.
inventory::submit! { Contribution::new(MOVED, "core/agent_access.connections.moved", provenance!(), &Moved {
    from: "/settings/agent-connections", to: "/settings/ai-connections",
}) }
inventory::submit! { Contribution::new(MOVED, "core/agent_access.mcp.moved", provenance!(), &Moved {
    from: "/settings/mcp", to: "/settings/ai-connections",
}) }
inventory::submit! { Contribution::new(MOVED, "core/agent_access.chatgpt.moved", provenance!(), &Moved {
    from: "/settings/chatgpt-plugin", to: "/settings/ai-connections",
}) }
