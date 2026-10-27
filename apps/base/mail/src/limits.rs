//! Installation limits, following Bilimbi's contributed Settings definitions.
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_settings::{Definition, Editor, ScopeKind, Setting, ValueType, DEFINITIONS};
use serde_json::json;

pub const MAX_MESSAGE_BYTES: &str = "mail.max_message_bytes";
pub const MAX_RECIPIENTS: &str = "mail.max_recipients";

/// A resolved snapshot for the IO-free renderer and standalone mailers.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub max_message_bytes: usize,
    pub max_recipients: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message_bytes: 25 * 1024 * 1024,
            max_recipients: 50,
        }
    }
}
macro_rules! setting {
    ($key:expr, $default:expr, $label:literal, $help:literal) => {
        inventory::submit! { Contribution::new(DEFINITIONS, $key, Provenance {
            module: "base/mail", source: file!(), line: line!(),
        }, &Setting::Definition(Definition {
            value_type: ValueType::Integer, scopes: &[ScopeKind::Global],
            default: || json!($default), nullable: false, encrypted: false,
            minimum: Some(1.0), maximum: Some(u32::MAX as f64), validator: None,
            editor: Some(Editor { group: "Mail", label: $label, help: $help,
                capability: None, reveal_duration_ms: 5_000 }),
        })) }
    };
}
setting!(
    MAX_MESSAGE_BYTES,
    Limits::default().max_message_bytes,
    "Maximum email size (bytes)",
    "Includes encoded attachments and headers; server owner only."
);
setting!(
    MAX_RECIPIENTS,
    Limits::default().max_recipients,
    "Maximum email recipients",
    "Counts To, Cc and Bcc together; server owner only."
);
