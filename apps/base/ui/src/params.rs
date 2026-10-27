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

/// The most characters one posted text value may hold unless its owner names a
/// larger limit with [`form_values_within`].
pub const FIELD_LIMIT: usize = 1024;

/// A posted text value over its limit. Refuse the whole event with it, so the
/// browser keeps the draft, rather than act on a form with the value blanked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TooLong {
    field: String,
    limit: usize,
}
impl std::fmt::Display for TooLong {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "The “{}” field is too long (maximum {} characters). Your draft is kept. Shorten the text and try again.",
            self.field, self.limit
        )
    }
}
impl std::error::Error for TooLong {}

/// The string fields of a posted form namespace (`filters[search]` → `search`)
/// from a live event payload, each at most [`FIELD_LIMIT`] characters. Other
/// keys and non-string values are ignored, and at most 64 fields are read. A
/// longer value is refused with [`TooLong`], never dropped. A text field whose
/// owner allows more reads through [`form_values_within`].
pub fn form_values(
    payload: &serde_json::Value,
    form: &str,
) -> Result<std::collections::HashMap<String, String>, TooLong> {
    form_values_within(payload, form, FIELD_LIMIT)
}

/// [`form_values`] with the owner's own limit, in characters, for every field.
pub fn form_values_within(
    payload: &serde_json::Value,
    form: &str,
    limit: usize,
) -> Result<std::collections::HashMap<String, String>, TooLong> {
    let Some(object) = payload.as_object() else {
        return Ok(Default::default());
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let key = key
                .strip_prefix(form)?
                .strip_prefix('[')?
                .strip_suffix(']')?;
            Some((key, value.as_str()?))
        })
        .take(64)
        .map(|(key, value)| Ok((key.to_owned(), bounded(key, value, limit)?)))
        .collect()
}

/// The scalar fields of a live event payload as the column hosts take them
/// (`ListColumns::handle`, `PageColumns::handle`): strings as they are, numbers
/// and booleans as their text; other values and keys past the first 64 are
/// ignored, and a string over [`FIELD_LIMIT`] characters is refused with
/// [`TooLong`]. Use this for a flex table's `grid` event rather than walking
/// the object in each page.
pub fn string_values(
    payload: &serde_json::Value,
) -> Result<std::collections::HashMap<String, String>, TooLong> {
    let Some(object) = payload.as_object() else {
        return Ok(Default::default());
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(text) => bounded(key, text, FIELD_LIMIT),
                serde_json::Value::Number(number) => Ok(number.to_string()),
                serde_json::Value::Bool(flag) => Ok(flag.to_string()),
                _ => return None,
            };
            Some(text.map(|text| (key.clone(), text)))
        })
        .take(64)
        .collect()
}

fn bounded(field: &str, value: &str, limit: usize) -> Result<String, TooLong> {
    if value.len() > limit && value.chars().count() > limit {
        return Err(TooLong {
            field: field.to_owned(),
            limit,
        });
    }
    Ok(value.to_owned())
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
