//! Operator settings Media owns (Administration → Settings → Operator, P3),
//! its capabilities and the `media-scanner` principal.
use crate::{
    quarantine::{MANAGE as QUARANTINE_MANAGE, VIEW as QUARANTINE_VIEW},
    scan::{POLICY, PRINCIPAL, SCAN, TIMEOUT},
};
use bilibi_authz::{
    CapabilityDefinition, StandingGrant, SystemPrincipalDefinition, VocabularyContribution,
    STANDING_GRANTS, VOCABULARY,
};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, Validator, ValueType, DEFINITIONS};
use serde_json::{json, Value};

const MANAGE: &str = "admin.system.media.manage";
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/media",
            source: file!(),
            line: line!(),
        }
    };
}
// Operator settings are platform-only: a grant alone is not enough, the
// persistent operator identity is required too. Quarantine is the tenant's.
inventory::submit! { Contribution::new(VOCABULARY, "base/media", provenance!(), &VocabularyContribution {
    capabilities: &[
        CapabilityDefinition::platform(MANAGE),
        CapabilityDefinition::tenant(QUARANTINE_VIEW),
        CapabilityDefinition::tenant(QUARANTINE_MANAGE),
        CapabilityDefinition::tenant(SCAN),
    ],
    domains: &["media"],
    verbs: &["scan"],
    roles: &[],
    system_principals: &[SystemPrincipalDefinition { name: PRINCIPAL, capabilities: &[SCAN] }],
}) }
// Scanning is Platform work no tenant should have to switch on; a tenant
// administrator may still deny it, and that tenant's files then stay pending.
inventory::submit! { Contribution::new(STANDING_GRANTS, PRINCIPAL, provenance!(), &StandingGrant { principal: PRINCIPAL, capabilities: &[SCAN] }) }
const GLOBAL: &[ScopeKind] = &[ScopeKind::Global];
const ALLOWED: [&str; 11] = [
    "application/pdf",
    "image/jpeg",
    "image/png",
    "image/gif",
    "image/webp",
    "text/plain",
    "text/csv",
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "application/vnd.ms-excel",
    "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
];

inventory::submit! { Contribution::new(DEFINITIONS, "media.disks", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: GLOBAL, default: || json!({}), nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value| value.as_object().is_some_and(|disks| disks.len() <= 64 && disks.iter().all(|(name, root)| !name.is_empty() && name.len() <= 255 && name != "external" && root.as_str().is_some_and(|root| root.starts_with('/') && root.len() <= 1024))), message: "Map each disk name (not \"external\") to an absolute directory path" }),
    editor: Some(Editor { group: "Media", label: "Media storage disks", help: "A JSON object mapping each disk name to a private folder path; each folder needs permission 0700 and must not move while it holds files.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "media.attachment_disk", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!("local"), nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value| value.as_str().is_some_and(|disk| !disk.is_empty() && disk.len() <= 255 && disk != "external"), message: "Name a configured disk" }),
    editor: Some(Editor { group: "Media", label: "Private attachment disk", help: "The disk where new attachments are stored.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "media.max_bytes", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(10_485_760), nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(536_870_912.0), validator: None,
    editor: Some(Editor { group: "Media", label: "Maximum upload bytes", help: "Maximum size of new media uploads and attachments.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "media.attachment_allowed_types", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Array, scopes: GLOBAL, default: || Value::from(ALLOWED.to_vec()), nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value| value.as_array().is_some_and(|types| types.len() <= 200 && types.iter().all(|kind| kind.as_str().is_some_and(|kind| kind.len() <= 255 && kind.contains('/')))), message: "Provide a JSON array of MIME types" }),
    editor: Some(Editor { group: "Media", label: "Attachment file types", help: "A JSON array of allowed file types, such as \"application/pdf\"; files a browser could run, such as HTML or SVG, are always refused.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, "media.stream_ttl_seconds", provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(300), nullable: false, encrypted: false, minimum: Some(1.0), maximum: Some(86_400.0), validator: None,
    editor: Some(Editor { group: "Media", label: "File link lifetime (seconds)", help: "Seconds a file link stays valid after it is issued.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, POLICY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: GLOBAL, default: || json!("when_available"), nullable: false, encrypted: false, minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value| matches!(value.as_str(), Some("when_available" | "required")), message: "Choose \"when_available\" or \"required\"" }),
    editor: Some(Editor { group: "Media", label: "Malware scanning", help: "\"when_available\": scan every new file when a scanner is installed, and without one store files unscanned. \"required\": refuse uploads while no scanner is installed, and never hand out a file that was not scanned clean, including every file stored before; rescan those from Quarantined Files first.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, TIMEOUT, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: GLOBAL, default: || json!(60), nullable: false, encrypted: false, minimum: Some(5.0), maximum: Some(600.0), validator: None,
    editor: Some(Editor { group: "Media", label: "Malware scan time limit", help: "Seconds one scan may take before it is retried; after eight tries the file is marked failed.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
