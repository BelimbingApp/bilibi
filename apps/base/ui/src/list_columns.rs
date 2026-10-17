//! Built-in-only column arrangement. No Grid dependency, queries or persistence.
//! FlexTable consumes `flex_columns()` and `load()`; the owning list handles
//! `ColumnsOutcome::Sort` through ListState so its query remains the row order.
//! The zoom steps are FlexTable's (`crate::flex_table`).
pub use crate::flex_table::{
    normalize_zoom, row_class, row_mode, step_zoom, ColumnType, RowMode, DEFAULT_ZOOM, ZOOM_STEPS,
};
use crate::flex_table::{ColumnKind, FlexColumn};
use crate::list_state::SpecError;
use std::collections::HashMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BuiltinColumn {
    pub id: String,
    pub label: String,
    pub short_label: String,
    pub kind: ColumnType,
    pub sort: Option<String>,
    pub sort_id: Option<String>,
    pub align_right: bool,
}
impl BuiltinColumn {
    pub fn new(id: &str, label: &str) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            short_label: label.into(),
            kind: ColumnType::String,
            sort: None,
            sort_id: None,
            align_right: false,
        }
    }
    pub fn sortable(mut self, key: &str) -> Self {
        self.sort = Some(key.into());
        self
    }
    pub fn right(mut self) -> Self {
        self.align_right = true;
        self
    }
    pub fn sortable_column(&self) -> bool {
        self.sort.is_some()
    }
}
pub struct BuiltinRow<'a, T> {
    pub key: String,
    pub record: &'a T,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ListColumns {
    pub id: String,
    builtins: Vec<BuiltinColumn>,
    columns: Vec<String>,
    pub suggestions: Vec<BuiltinColumn>,
    pub add_query: String,
    pub zoom: u16,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ColumnsOutcome {
    Update,
    Sort(String),
    Noop,
}
impl ListColumns {
    pub fn mount(id: &str, builtins: Vec<BuiltinColumn>) -> Result<Self, SpecError> {
        Self::bounded(id, builtins, 64)
    }
    /// SQL result projections may be wider than an operational list. Keep the
    /// same arrangement and event contract, with a separate bounded vocabulary.
    pub fn projection(id: &str, builtins: Vec<BuiltinColumn>) -> Result<Self, SpecError> {
        Self::bounded(id, builtins, 2048)
    }
    fn bounded(id: &str, builtins: Vec<BuiltinColumn>, limit: usize) -> Result<Self, SpecError> {
        if id.is_empty() || builtins.is_empty() || builtins.len() > limit {
            return Err(SpecError(
                "a list needs an id and bounded, nonempty built-in columns",
            ));
        }
        let mut ids = Vec::new();
        for col in &builtins {
            if col.id.is_empty()
                || !col
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_')
                || ids.contains(&col.id)
            {
                return Err(SpecError(
                    "built-in column ids must be unique DOM-safe strings",
                ));
            }
            ids.push(col.id.clone());
        }
        Ok(Self {
            id: id.into(),
            builtins,
            columns: vec![],
            suggestions: vec![],
            add_query: String::new(),
            zoom: DEFAULT_ZOOM,
        })
    }
    pub fn builtins(&self) -> &[BuiltinColumn] {
        &self.builtins
    }
    pub fn visible_ids(&self) -> Vec<&str> {
        if self.columns.is_empty() {
            self.builtins.iter().map(|c| c.id.as_str()).collect()
        } else {
            self.columns.iter().map(String::as_str).collect()
        }
    }
    /// All are slot/value columns, removable, with no alternative lenses. U7's
    /// presenter can map these descriptors without querying a Grid catalog.
    pub fn column_views(&self) -> Vec<&BuiltinColumn> {
        self.visible_ids()
            .iter()
            .filter_map(|id| self.builtins.iter().find(|c| &c.id == id))
            .collect()
    }
    /// The visible columns as `FlexTable` takes them: each is drawn by the
    /// list's own fragment of the same id (`FlexRow::slot`).
    pub fn flex_columns(&self) -> Vec<FlexColumn> {
        self.column_views().into_iter().map(flex_column).collect()
    }
    /// The add box's offers as `FlexTable` takes them.
    pub fn flex_suggestions(&self) -> Vec<FlexColumn> {
        self.suggestions.iter().map(flex_column).collect()
    }
    pub fn mode(&self) -> RowMode {
        row_mode(self.zoom)
    }
    pub fn load<'a, T>(
        &mut self,
        records: &'a [T],
        key: impl Fn(&T) -> String,
    ) -> Vec<BuiltinRow<'a, T>> {
        self.suggestions.clear();
        self.add_query.clear();
        records
            .iter()
            .map(|record| BuiltinRow {
                key: key(record),
                record,
            })
            .collect()
    }
    /// Invalid/unknown operations are a no-op; never hide the last column.
    pub fn handle(&mut self, params: &HashMap<String, String>) -> ColumnsOutcome {
        let get = |key| params.get(key).map(String::as_str).unwrap_or("");
        let spec = get("spec");
        let mut visible: Vec<String> = self.visible_ids().into_iter().map(str::to_owned).collect();
        match get("op") {
            "sort" => {
                return self
                    .builtins
                    .iter()
                    .find(|c| c.id == get("sort"))
                    .and_then(|c| c.sort.clone())
                    .map_or(ColumnsOutcome::Noop, ColumnsOutcome::Sort)
            }
            "suggest" => {
                self.add_query = get("add").trim().chars().take(120).collect();
                self.suggestions = self.find_suggestions(&self.add_query);
                return ColumnsOutcome::Update;
            }
            "add"
                if self.builtins.iter().any(|c| c.id == spec)
                    && !visible.iter().any(|id| id == spec) =>
            {
                visible.push(spec.into())
            }
            "add_typed" => {
                let query: String = params
                    .get("add")
                    .map_or(self.add_query.as_str(), String::as_str)
                    .trim()
                    .chars()
                    .take(120)
                    .collect();
                let suggestions = self.find_suggestions(&query);
                let wanted = query.to_lowercase();
                let Some(column) = suggestions
                    .iter()
                    .find(|c| c.id.to_lowercase() == wanted || c.label.to_lowercase() == wanted)
                    .or(suggestions.first())
                else {
                    return ColumnsOutcome::Noop;
                };
                visible.push(column.id.clone());
            }
            "remove" if visible.len() > 1 && visible.iter().any(|id| id == spec) => {
                visible.retain(|id| id != spec)
            }
            "move" if visible.iter().any(|id| id == spec) && spec != get("before") => {
                visible.retain(|id| id != spec);
                let index = visible
                    .iter()
                    .position(|id| id == get("before"))
                    .unwrap_or(visible.len());
                visible.insert(index, spec.into());
            }
            "zoom" if matches!(get("dir"), "in" | "out") => {
                self.zoom = step_zoom(self.zoom, get("dir") == "in");
                return ColumnsOutcome::Update;
            }
            "zoom_preset" if RowMode::parse(get("preset")).is_some() => {
                self.zoom = RowMode::parse(get("preset")).map_or(DEFAULT_ZOOM, RowMode::zoom);
                return ColumnsOutcome::Update;
            }
            "reset" => {
                self.columns.clear();
                self.zoom = DEFAULT_ZOOM;
                self.clear_suggestions();
                return ColumnsOutcome::Update;
            }
            _ => return ColumnsOutcome::Noop,
        }
        self.columns = visible;
        self.clear_suggestions();
        ColumnsOutcome::Update
    }
    fn clear_suggestions(&mut self) {
        self.suggestions.clear();
        self.add_query.clear();
    }
    fn find_suggestions(&self, query: &str) -> Vec<BuiltinColumn> {
        if query.is_empty() {
            return vec![];
        }
        let shown = self.visible_ids();
        let wanted = query.to_lowercase();
        self.builtins
            .iter()
            .filter(|c| {
                !shown.contains(&c.id.as_str())
                    && (c.label.to_lowercase().contains(&wanted)
                        || c.id.to_lowercase().contains(&wanted))
            })
            .cloned()
            .collect()
    }
}

/// A built-in column as `FlexTable` takes it: a slot the list draws itself.
pub fn flex_column(builtin: &BuiltinColumn) -> FlexColumn {
    let mut column = FlexColumn::new(&builtin.id, &builtin.id, &builtin.label)
        .short(&builtin.short_label)
        .typed(builtin.kind)
        .kind(ColumnKind::Slot);
    column.sortable = builtin.sortable_column();
    column.sort_key = builtin.sort.clone();
    column.align_right = builtin.align_right;
    column.sort_id = builtin.sort_id.clone();
    column
}
