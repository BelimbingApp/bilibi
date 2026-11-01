//! The field-level rows of one mutation's old and new values, shared by the
//! record history panel and the Data Mutations page so a diff reads the same
//! on both.
//!
//! A value that denotes an instant is recognised by its shape, never by its
//! field name: capture writes timestamps as ISO 8601 strings, a naive one is
//! UTC by platform convention, and such a value renders through the page's
//! clock like every other time on the page, with seconds so two changes inside
//! one minute read as two values. A calendar date or a bare time stays text:
//! neither denotes an instant, so neither has a zone to shift.
//!
//! A field the reader may not see (`withheld` on the entry, set by the screen
//! from the record owner's field policy through
//! `bilibi_authz::fields::withheld_by_record_type`) is still a row, so the
//! reader knows the record changed, but it carries no values and renders the
//! shared restricted marker. That is different from a sensitive key, whose
//! value capture itself redacted: it renders in the muted treatment.
use crate::present;
use askama::Template;
use bilibi_audit::MutationEntry;
use bilibi_database::{Error, Result};
use bilibi_datetime::{DisplayContext, Precision};
use bilibi_ui::Html;
use jiff::{civil, tz::TimeZone, Timestamp};
use serde_json::Value;
use std::{collections::BTreeSet, sync::LazyLock};

const SENSITIVE_FRAGMENTS: [&str; 5] = ["password", "secret", "token", "key", "hash"];
/// The most field names a collapsed row writes out before counting the rest.
const SUMMARY_NAMES: usize = 4;

/// One side of a field's change.
#[derive(Clone, Debug, PartialEq)]
pub enum DiffValue {
    Absent,
    Instant(Timestamp),
    Text(String),
}

/// One changed field.
#[derive(Clone, Debug)]
pub struct DiffRow {
    pub field: String,
    pub old: DiffValue,
    pub new: DiffValue,
    pub sensitive: bool,
    pub withheld: bool,
}

fn object(value: &Value) -> Option<&serde_json::Map<String, Value>> {
    value.as_object()
}

/// The keys `rows` would render, in field-name order, before their values are
/// classified: a create lists every recorded attribute, a delete the reverse,
/// an update only the fields whose stored old and new values differ.
fn data_keys(entry: &MutationEntry) -> Vec<&str> {
    match entry.event.as_str() {
        "created" => object(&entry.new_values)
            .map(|new| new.keys().map(String::as_str).collect::<BTreeSet<_>>())
            .unwrap_or_default()
            .into_iter()
            .collect(),
        "deleted" => object(&entry.old_values)
            .map(|old| old.keys().map(String::as_str).collect::<BTreeSet<_>>())
            .unwrap_or_default()
            .into_iter()
            .collect(),
        _ => {
            static EMPTY: LazyLock<serde_json::Map<String, Value>> =
                LazyLock::new(serde_json::Map::new);
            let old = object(&entry.old_values).unwrap_or(&EMPTY);
            let new = object(&entry.new_values).unwrap_or(&EMPTY);
            old.keys()
                .chain(new.keys())
                .map(String::as_str)
                .collect::<BTreeSet<_>>()
                .into_iter()
                .filter(|key| old.get(*key) != new.get(*key))
                .collect()
        }
    }
}

/// The changed fields of a mutation, sorted by field name, withheld ones
/// among them.
pub fn rows(entry: &MutationEntry) -> Vec<DiffRow> {
    let mut rows: Vec<DiffRow> = data_keys(entry)
        .into_iter()
        .map(|key| {
            let (old, new) = match entry.event.as_str() {
                "created" => (DiffValue::Absent, value(entry.new_values.get(key))),
                "deleted" => (value(entry.old_values.get(key)), DiffValue::Absent),
                _ => (
                    value(entry.old_values.get(key)),
                    value(entry.new_values.get(key)),
                ),
            };
            DiffRow {
                field: key.to_owned(),
                old,
                new,
                sensitive: sensitive_key(key),
                withheld: false,
            }
        })
        .collect();
    rows.extend(entry.withheld.iter().map(|field| DiffRow {
        field: field.clone(),
        old: DiffValue::Absent,
        new: DiffValue::Absent,
        sensitive: false,
        withheld: true,
    }));
    rows.sort_by(|a, b| a.field.cmp(&b.field));
    rows
}

/// The names of the fields `rows` would render, without their values: the
/// collapsed table row paints these without walking every value.
pub fn changed_fields(entry: &MutationEntry) -> Vec<String> {
    let mut fields: Vec<String> = data_keys(entry)
        .into_iter()
        .map(str::to_owned)
        .chain(entry.withheld.iter().cloned())
        .collect();
    fields.sort();
    fields
}

/// One line naming the changed fields, without their values. At most four
/// names are written out; a longer change says how many more there are.
pub fn summary_from_fields(fields: &[String]) -> String {
    match fields {
        [] => "No field changes recorded.".into(),
        [field] => field.clone(),
        _ => {
            let shown = fields[..fields.len().min(SUMMARY_NAMES)].join(", ");
            let rest = fields.len().saturating_sub(SUMMARY_NAMES);
            if rest == 0 {
                format!("{} fields: {shown}", fields.len())
            } else {
                format!("{} fields: {shown}, +{rest}", fields.len())
            }
        }
    }
}

/// Classifies one stored value for rendering.
pub fn value(stored: Option<&Value>) -> DiffValue {
    match stored {
        None | Some(Value::Null) => DiffValue::Absent,
        Some(Value::String(text)) => instant(text)
            .map(DiffValue::Instant)
            .unwrap_or_else(|| DiffValue::Text(text.clone())),
        Some(Value::Bool(flag)) => DiffValue::Text(flag.to_string()),
        Some(Value::Number(number)) => DiffValue::Text(number.to_string()),
        Some(other) => DiffValue::Text(other.to_string()),
    }
}

/// Whether a field's values are hidden from the reader by name: the same
/// substring rule both surfaces applied. Which fields capture
/// redacts is `bilibi_audit`'s decision, not this module's.
pub fn sensitive_key(key: &str) -> bool {
    let key = key.to_lowercase();
    SENSITIVE_FRAGMENTS
        .iter()
        .any(|fragment| key.contains(fragment))
}

/// A string with an offset is an instant outright; one without is the
/// platform's stored UTC. A date alone or a time alone parses as neither.
fn instant(text: &str) -> Option<Timestamp> {
    if let Ok(instant) = text.parse::<Timestamp>() {
        return Some(instant);
    }
    let naive: civil::DateTime = text.parse().ok()?;
    TimeZone::UTC.to_timestamp(naive).ok()
}

/// Renders one side of a change: an instant through the page's clock, so it
/// follows the shell's display mode, and anything else as its text.
pub fn render_value(
    display: &DisplayContext,
    id: &str,
    value: &DiffValue,
    class: &str,
) -> Result<Html> {
    Ok(match value {
        DiffValue::Instant(instant) => {
            present::time_at(display, id, *instant, Precision::Second, class)?
        }
        DiffValue::Text(text) => span(id, class, text)?,
        DiffValue::Absent => span(id, class, "—")?,
    })
}

#[derive(Template)]
#[template(
    source = r#"<span id="{{ id }}"{% if !class.is_empty() %} class="{{ class }}"{% endif %}>{{ text }}</span>"#,
    ext = "html"
)]
struct Span<'a> {
    id: &'a str,
    class: &'a str,
    text: &'a str,
}
fn span(id: &str, class: &str, text: &str) -> Result<Html> {
    Html::render(&Span { id, class, text }).map_err(|_| Error::Internal)
}
