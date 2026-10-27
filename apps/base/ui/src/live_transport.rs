//! Shared limits for Bilibi's native live adapter; no Bilimbi module counterpart.
//! Phoenix/LiveView owns that transport; Bilibi's Web host and UI share these
//! limits, keeping JSON form events independent of the binary upload chunk size.

/// Maximum UTF-8 bytes in the complete encoded JSON intent, including its
/// request ID. One MiB carries multiple 10,000-character fields even with JSON
/// escaping (up to six bytes per UTF-16 code unit), while bounding each event.
pub const LIVE_EVENT_LIMIT: usize = 1024 * 1024;

/// The same refusal is served to the browser and returned by the host. A
/// refusal never clears the page, resets a form or enters a module handler.
pub const LIVE_EVENT_TOO_LARGE: &str = "This form is too large to send (maximum 1 MiB). Your draft is kept. Shorten the text and try again. Use a file upload for larger content.";

/// The most characters one text value of a live event may hold. It matches the
/// longest field the Platform declares (Address raw input); a field with a
/// shorter limit is still reported by its owner, on the field.
pub const LIVE_FIELD_LIMIT: usize = 10_000;

/// The key of the first text value in `payload` longer than [`LIVE_FIELD_LIMIT`],
/// looking inside nested objects and arrays. Web refuses such an event before
/// any page sees it, so no extractor has to drop a value silently.
pub fn overlong_field(payload: &serde_json::Value) -> Option<&str> {
    fn long(value: &serde_json::Value) -> bool {
        match value {
            serde_json::Value::String(text) => {
                text.len() > LIVE_FIELD_LIMIT && text.chars().count() > LIVE_FIELD_LIMIT
            }
            serde_json::Value::Array(items) => items.iter().any(long),
            serde_json::Value::Object(fields) => fields.values().any(long),
            _ => false,
        }
    }
    payload
        .as_object()?
        .iter()
        .find_map(|(key, value)| long(value).then_some(key.as_str()))
}

/// The refusal for an event holding a text value over [`LIVE_FIELD_LIMIT`];
/// like [`LIVE_EVENT_TOO_LARGE`] it keeps the draft.
pub fn field_too_long(field: &str) -> String {
    format!("The “{field}” field is too long (maximum {LIVE_FIELD_LIMIT} characters). Your draft is kept. Shorten the text and try again. Use a file upload for larger content.")
}
