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

/// The default most characters one posted text value may hold: the bound
/// [`form_values`] and [`string_values`] apply, and the usual `limit` for
/// [`form_values_checked`] and [`string_values_checked`].
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
/// from a live event payload. Other keys and non-string values are ignored, and
/// at most 64 fields of at most 1 KiB each are kept.
///
/// Deprecated: a longer value is silently dropped, so the form acts as if the
/// field were blank. Use [`form_values_checked`], which refuses it.
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
            let value = value.as_str().filter(|value| value.len() <= 1024)?;
            Some((key.to_owned(), value.to_owned()))
        })
        .take(64)
        .collect()
}

/// [`form_values`] with the owner's own `limit`, in characters, for every
/// field (pass [`FIELD_LIMIT`] when the owner has no larger one). A longer
/// value is refused with [`TooLong`], never dropped: return it with
/// `.map_err(Error::refused)?` and the browser keeps the draft.
pub fn form_values_checked(
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
/// ignored, and a string longer than 1 KiB is dropped. Use this for a flex table's `grid`
/// event rather than walking the object in each page.
///
/// Deprecated: the dropped string is silent. Use [`string_values_checked`].
pub fn string_values(payload: &serde_json::Value) -> std::collections::HashMap<String, String> {
    let Some(object) = payload.as_object() else {
        return Default::default();
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(text) if text.len() <= 1024 => text.clone(),
                serde_json::Value::Number(number) => number.to_string(),
                serde_json::Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some((key.clone(), text))
        })
        .take(64)
        .collect()
}

/// [`string_values`] with the owner's own `limit`, in characters (pass
/// [`FIELD_LIMIT`] when the owner has no larger one). A string over it is
/// refused with [`TooLong`], never dropped.
pub fn string_values_checked(
    payload: &serde_json::Value,
    limit: usize,
) -> Result<std::collections::HashMap<String, String>, TooLong> {
    let Some(object) = payload.as_object() else {
        return Ok(Default::default());
    };
    object
        .iter()
        .filter_map(|(key, value)| {
            let text = match value {
                serde_json::Value::String(text) => bounded(key, text, limit),
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
