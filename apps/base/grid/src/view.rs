//! What a person arranged on a list's columns, as one value that round-trips
//! through the URL and through the account's saved setting: the table, the
//! column specs in order, the lens of each column, the zoom (the height of a
//! row), and the date a `delta` lens compares against.
//!
//! The page keeps its own sort, search, filters and pagination; this is only
//! what the flexible table adds. Every field parses leniently from text and
//! encodes to the short keys below, leaving defaults out so the address stays
//! short:
//!
//! ```text
//! cols=name,company.name,employees:count
//! lens=employees:count|bar
//! z=18  since=2026-01-01
//! ```
//!
//! An address that carries any of those keys says what to show. One that
//! carries none leaves it to what the account last arranged
//! ([`crate::page_views`]), and to the page's own columns when the account
//! arranged nothing.
use bilibi_ui::flex_table::{normalize_zoom, parse_zoom, DEFAULT_ZOOM};
use jiff::civil;
use serde_json::{json, Value};
use std::collections::{BTreeMap, HashMap};

/// The URL keys a view travels in.
pub const KEYS: [&str; 4] = ["cols", "lens", "z", "since"];
/// Specs an address or a stored arrangement may name at most, before the
/// catalog bounds what is resolved.
pub const MAX_SPECS: usize = 400;
/// Bytes of one spec kept from an address or a stored arrangement.
pub const MAX_SPEC_TEXT: usize = 200;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct View {
    /// The catalog table the page's rows come from: the page's name in the
    /// account's memory.
    pub table: String,
    pub columns: Vec<String>,
    /// By spec; a column without an entry wears the value lens.
    pub lenses: BTreeMap<String, String>,
    pub zoom: u16,
    pub since: Option<civil::Date>,
}

impl View {
    /// The page's own view of this table: nothing walked, no lens, normal rows.
    pub fn new(table: &str) -> Self {
        Self {
            table: table.into(),
            columns: Vec::new(),
            lenses: BTreeMap::new(),
            zoom: DEFAULT_ZOOM,
            since: None,
        }
    }

    /// Whether URL params say anything about the view, so they win over what
    /// was saved.
    pub fn carried(params: &HashMap<String, String>) -> bool {
        KEYS.iter().any(|key| params.contains_key(*key))
    }

    /// Reads a view from URL params. Unknown or malformed values fall back,
    /// never fail.
    pub fn from_params(params: &HashMap<String, String>, table: &str) -> Self {
        Self {
            table: table.into(),
            columns: split_list(params.get("cols").map(String::as_str)),
            lenses: parse_lenses(params.get("lens").map(String::as_str)),
            zoom: params
                .get("z")
                .map_or(DEFAULT_ZOOM, |text| parse_zoom(text)),
            since: params.get("since").and_then(|text| parse_date(text)),
        }
    }

    /// Writes the view as URL params, leaving defaults out so the address
    /// stays short.
    pub fn to_params(&self) -> HashMap<String, String> {
        let mut params = HashMap::new();
        if !self.columns.is_empty() {
            params.insert("cols".into(), self.columns.join(","));
        }
        if !self.lenses.is_empty() {
            params.insert("lens".into(), encode_lenses(&self.lenses));
        }
        if self.zoom != DEFAULT_ZOOM {
            params.insert("z".into(), self.zoom.to_string());
        }
        if let Some(since) = self.since {
            params.insert("since".into(), since.to_string());
        }
        params
    }

    /// Whether the view is the page's own: nothing arranged, so nothing to
    /// carry or keep.
    pub fn is_default(&self) -> bool {
        self.columns.is_empty()
            && self.lenses.is_empty()
            && self.zoom == DEFAULT_ZOOM
            && self.since.is_none()
    }

    /// The view as plain JSON, the shape the account's setting keeps.
    pub fn to_json(&self) -> Value {
        json!({
            "columns": self.columns,
            "lenses": self.lenses,
            "zoom": self.zoom,
            "since": self.since.map(|date| date.to_string()),
        })
    }

    /// A view read back from stored JSON. Anything malformed falls back,
    /// never fails.
    pub fn from_json(value: &Value, table: &str) -> Self {
        let columns = match value.get("columns") {
            Some(Value::Array(items)) => dedup(
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|spec| !spec.is_empty())
                    .map(bounded),
            ),
            _ => Vec::new(),
        };
        let lenses = match value.get("lenses") {
            Some(Value::Object(entries)) => entries
                .iter()
                .filter_map(|(spec, lens)| Some((bounded(spec), bounded(lens.as_str()?))))
                .collect(),
            _ => BTreeMap::new(),
        };
        let zoom = value
            .get("zoom")
            .and_then(Value::as_i64)
            .map_or(DEFAULT_ZOOM, normalize_zoom);
        let since = value
            .get("since")
            .and_then(Value::as_str)
            .and_then(parse_date);
        Self {
            table: table.into(),
            columns,
            lenses,
            zoom,
            since,
        }
    }

    /// Sets the row height to the offered one nearest `zoom`.
    pub fn put_zoom(&mut self, zoom: u16) {
        self.zoom = normalize_zoom(zoom.into());
    }

    /// Back to the page's own view of this table.
    pub fn reset(&mut self) {
        *self = Self::new(&self.table);
    }

    /// Adds a column spec at the end, once.
    pub fn add_column(&mut self, spec: &str) {
        if !self.columns.iter().any(|known| known == spec) {
            self.columns.push(spec.into());
        }
    }

    /// Removes a column spec and its lens.
    pub fn remove_column(&mut self, spec: &str) {
        self.columns.retain(|known| known != spec);
        self.lenses.remove(spec);
    }

    /// Moves `spec` before `before`, or to the end when `before` is none.
    pub fn move_column(&mut self, spec: &str, before: Option<&str>) {
        if !self.columns.iter().any(|known| known == spec) || before == Some(spec) {
            return;
        }
        self.columns.retain(|known| known != spec);
        let index = before
            .and_then(|before| self.columns.iter().position(|known| known == before))
            .unwrap_or(self.columns.len());
        self.columns.insert(index, spec.into());
    }

    /// The date a delta lens compares against: the view's, or thirty days ago.
    pub fn since(&self) -> civil::Date {
        self.since
            .unwrap_or_else(|| crate::query::default_since(crate::query::today()))
    }

    /// Sets the date a delta lens compares against; anything that is not a
    /// date clears it.
    pub fn put_since(&mut self, value: Option<&str>) {
        self.since = value.and_then(parse_date);
    }

    /// Sets a column's lens; `value` drops the entry so the URL stays short.
    pub fn put_lens(&mut self, spec: &str, lens: &str) {
        if lens == "value" {
            self.lenses.remove(spec);
        } else {
            self.lenses.insert(spec.into(), lens.into());
        }
    }

    /// The lens name the view holds for a spec, `value` when none.
    pub fn lens(&self, spec: &str) -> &str {
        self.lenses.get(spec).map_or("value", String::as_str)
    }
}

fn bounded(text: &str) -> String {
    text.chars().take(MAX_SPEC_TEXT).collect()
}

fn dedup(specs: impl Iterator<Item = String>) -> Vec<String> {
    let mut seen = Vec::new();
    for spec in specs {
        if !seen.contains(&spec) {
            seen.push(spec);
        }
        if seen.len() == MAX_SPECS {
            break;
        }
    }
    seen
}

fn split_list(text: Option<&str>) -> Vec<String> {
    match text {
        None | Some("") => Vec::new(),
        Some(text) => dedup(
            text.split(',')
                .map(str::trim)
                .filter(|spec| !spec.is_empty())
                .map(bounded),
        ),
    }
}

fn parse_lenses(text: Option<&str>) -> BTreeMap<String, String> {
    text.unwrap_or_default()
        .split(',')
        .filter_map(|pair| {
            let (spec, lens) = pair.split_once('|')?;
            (!spec.is_empty() && !lens.is_empty()).then(|| (bounded(spec), bounded(lens)))
        })
        .collect()
}

fn encode_lenses(lenses: &BTreeMap<String, String>) -> String {
    lenses
        .iter()
        .map(|(spec, lens)| format!("{spec}|{lens}"))
        .collect::<Vec<_>>()
        .join(",")
}

fn parse_date(text: &str) -> Option<civil::Date> {
    text.parse().ok()
}
