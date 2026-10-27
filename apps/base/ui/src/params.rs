//! Shared coercion of untrusted query/form strings (Bilimbi UI.Params).

/// Trim, parse the entire value, and accept only a positive integer.
pub fn positive_integer(value: Option<&str>) -> Option<u64> {
    value?.trim().parse::<u64>().ok().filter(|n| *n > 0)
}

/// Database identities use a signed 64-bit representation.
pub fn positive_id(value: Option<&str>) -> Option<i64> {
    positive_integer(value).and_then(|n| i64::try_from(n).ok())
}

/// Whitespace is significant here; only the empty string becomes absent.
pub fn blank_to_none(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

pub fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim)
}

/// The string fields of a posted form namespace (`filters[search]` → `search`)
/// from a live event payload. Other keys and non-string values are ignored, and
/// at most 64 fields are kept. A string over `LIVE_FIELD_LIMIT` characters never
/// reaches here: the live host refuses that event first, so the field's owner
/// validates its own, shorter limit and reports it on the field.
pub fn form_values(
    payload: &serde_json::Value,
    form: &str,
) -> std::collections::HashMap<String, String> {
    let Some(object) = payload.as_object() else {
        return Default::default();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let key = key
                .strip_prefix(form)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            let value = value.as_str()?;
            Some((key.to_owned(), value.to_owned()))
        })
        .take(64)
        .collect()
}

/// The scalar fields of a live event payload as the column hosts take them
/// (`ListColumns::handle`, `PageColumns::handle`): strings as they are, numbers
/// and booleans as their text; other values and keys past the first 64 are
/// ignored. Use this for a flex table's `grid` event rather than walking the
/// object in each page. Field length is bounded as for `form_values`.
pub fn string_values(payload: &serde_json::Value) -> std::collections::HashMap<String, String> {
    let Some(object) = payload.as_object() else {
        return Default::default();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some((key.clone(), text))
        })
        .take(64)
        .collect()
}

/// `path` with `query` as its sorted, percent-encoded query string: the
/// address the live host writes for a page's canonical query, so a link built
/// here (a tab that patches its page) names the same URL the page answers with.
pub fn address(path: &str, query: &std::collections::HashMap<String, String>) -> String {
    let mut pairs: Vec<_> = query.iter().collect();
    pairs.sort_unstable();
    let query = pairs
        .into_iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    }
}

fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
