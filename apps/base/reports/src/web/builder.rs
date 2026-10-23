//! The definition editor on a report's Definition tab: one `data-change`
//! form whose values the page holds between round trips, so adding a filter
//! or switching the kind redraws without losing what was typed, and a Save
//! button that sends the held definition through `base.reports.update`.
//! Choices come from the reader's own catalog (`Reports::columns`), so a
//! field they cannot see is never offered.
use super::render;
use crate::{ColumnInfo, Kind, Report};
use askama::Template;
use bilibi_database::Result;
use bilibi_ui::{
    choices, error_sentence, AddMore, Button, ButtonVariant, Choice, ErrorSummary, Field, Html,
    IconButton, IconKind, InputKind, MultiSelect,
};
use serde_json::{json, Value};
use std::collections::BTreeMap;

/// Filter rows the editor shows at most (the definition allows more).
const MAX_FILTER_ROWS: usize = 8;
/// Measure rows the editor shows.
const MEASURE_ROWS: usize = 4;
const TESTS: [(&str, &str); 7] = [
    ("is", "equals"),
    ("is one of", "one_of"),
    ("contains", "contains"),
    ("is between", "range"),
    ("is within", "within"),
    ("is empty", "empty"),
    ("is not empty", "not_empty"),
];
const PERIODS: [(&str, &str); 7] = [
    ("Today", "today"),
    ("This week", "this_week"),
    ("Last 7 days", "last_7_days"),
    ("Last 30 days", "last_30_days"),
    ("This month", "this_month"),
    ("This quarter", "this_quarter"),
    ("This year", "this_year"),
];
const BUCKETS: [(&str, &str); 5] = [
    ("By day", "day"),
    ("By week", "week"),
    ("By month", "month"),
    ("By quarter", "quarter"),
    ("By year", "year"),
];
const CHART_CHOICES: [(&str, &str); 7] = [
    ("No chart", ""),
    ("Number", "kpi"),
    ("Bars", "bar"),
    ("Stacked bars", "stacked_bar"),
    ("Line", "line"),
    ("Pie", "pie"),
    ("Donut", "donut"),
];

#[derive(Clone, Default)]
struct FilterRow {
    column: String,
    test: String,
    value: String,
    to: String,
    ask: bool,
}

/// What the form holds, as text: the definition being edited.
#[derive(Clone)]
pub(crate) struct Builder {
    pub name: String,
    pub description: String,
    pub kind: String,
    columns: Vec<String>,
    sort: String,
    descending: bool,
    totals: bool,
    groupings: [(String, String); 2],
    measures: [(String, String); MEASURE_ROWS],
    chart: String,
    filters: Vec<FilterRow>,
    pub errors: BTreeMap<String, Vec<String>>,
}

fn text(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

impl Builder {
    pub fn from_report(report: &Report) -> Self {
        let d = &report.definition;
        let mut groupings: [(String, String); 2] = Default::default();
        for (slot, g) in groupings.iter_mut().zip(&d.groupings) {
            *slot = (g.column.clone(), g.bucket.clone().unwrap_or_default());
        }
        let mut measures: [(String, String); MEASURE_ROWS] = Default::default();
        for (slot, m) in measures.iter_mut().zip(&d.measures) {
            *slot = (m.column.clone(), m.agg.clone());
        }
        let filters = d
            .filters
            .iter()
            .map(|f| {
                let (value, to) = match (f.test.as_str(), &f.value) {
                    ("one_of", Value::Array(values)) => (
                        values.iter().map(text).collect::<Vec<_>>().join(", "),
                        String::new(),
                    ),
                    ("range", range) => (text(&range["min"]), text(&range["max"])),
                    (_, value) => (text(value), String::new()),
                };
                FilterRow {
                    column: f.column.clone(),
                    test: f.test.clone(),
                    value,
                    to,
                    ask: f.ask,
                }
            })
            .collect();
        Self {
            name: report.name.clone(),
            description: report.description.clone(),
            kind: report.kind.key().into(),
            columns: d.columns.clone(),
            sort: d.sort.first().map(|s| s.column.clone()).unwrap_or_default(),
            descending: d.sort.first().is_some_and(|s| s.descending),
            totals: d.totals,
            groupings,
            measures,
            chart: d.chart.clone().unwrap_or_default(),
            filters,
            errors: BTreeMap::new(),
        }
    }

    /// Takes the posted form: every field the editor draws, under `def`.
    pub fn read(&mut self, value: &Value) {
        let fields = bilibi_ui::params::form_values(value, "def");
        let get = |key: &str| fields.get(key).cloned().unwrap_or_default();
        self.name = get("name");
        self.description = get("description");
        if Kind::from_key(&get("kind")).is_some() {
            self.kind = get("kind");
        }
        self.columns = value
            .get("def[columns][]")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    // The picker's hidden companion posts "" so an empty
                    // choice still arrives; it is not a column.
                    .filter(|spec| !spec.is_empty())
                    .map(str::to_owned)
                    .take(crate::MAX_COLUMNS)
                    .collect()
            })
            .unwrap_or_default();
        self.sort = get("sort");
        self.descending = get("sort_dir") == "desc";
        self.totals = get("totals") == "true";
        for (i, slot) in self.groupings.iter_mut().enumerate() {
            *slot = (get(&format!("g{i}")), get(&format!("b{i}")));
        }
        for (i, slot) in self.measures.iter_mut().enumerate() {
            *slot = (get(&format!("m{i}")), get(&format!("a{i}")));
        }
        self.chart = get("chart");
        for (i, row) in self.filters.iter_mut().enumerate() {
            *row = FilterRow {
                column: get(&format!("f{i}_column")),
                test: get(&format!("f{i}_test")),
                value: get(&format!("f{i}_value")),
                to: get(&format!("f{i}_to")),
                ask: get(&format!("f{i}_ask")) == "true",
            };
        }
    }

    pub fn add_filter(&mut self) {
        if self.filters.len() < MAX_FILTER_ROWS {
            self.filters.push(FilterRow {
                test: "equals".into(),
                ..FilterRow::default()
            });
        }
    }
    pub fn remove_filter(&mut self, index: usize) {
        if index < self.filters.len() {
            self.filters.remove(index);
        }
    }

    /// The contract's input for this report: name, description, kind and
    /// the definition, with blank rows left out.
    pub fn input(&self, report: &Report) -> Value {
        let kind = Kind::from_key(&self.kind).unwrap_or(report.kind);
        // The agent dialect (`definition::schema`): no nulls, a list as
        // `values`, a range as `min`/`max`, as an agent would send it.
        let filters: Vec<Value> = self
            .filters
            .iter()
            .filter(|f| !f.column.is_empty())
            .map(|f| {
                let mut filter = json!({"column": f.column, "test": f.test, "ask": f.ask});
                match f.test.as_str() {
                    "one_of" => {
                        let values: Vec<&str> = f
                            .value
                            .split(',')
                            .map(str::trim)
                            .filter(|v| !v.is_empty())
                            .collect();
                        if !values.is_empty() {
                            filter["values"] = json!(values);
                        }
                    }
                    "range" => {
                        for (key, bound) in [("min", &f.value), ("max", &f.to)] {
                            if !bound.trim().is_empty() {
                                filter[key] = json!(bound.trim());
                            }
                        }
                    }
                    "empty" | "not_empty" => {}
                    _ if !f.value.is_empty() => filter["value"] = json!(f.value),
                    _ => {}
                }
                filter
            })
            .collect();
        let mut definition = json!({"filters": filters});
        match kind {
            Kind::Rows => {
                definition["columns"] = json!(self.columns);
                if !self.sort.is_empty() {
                    definition["sort"] =
                        json!([{"column": self.sort, "descending": self.descending}]);
                }
                if self.totals {
                    definition["totals"] = json!(true);
                }
            }
            Kind::Summary => {
                let groupings: Vec<Value> = self
                    .groupings
                    .iter()
                    .filter(|(column, _)| !column.is_empty())
                    .map(|(column, bucket)| {
                        if bucket.is_empty() {
                            json!({"column": column})
                        } else {
                            json!({"column": column, "bucket": bucket})
                        }
                    })
                    .collect();
                let measures: Vec<Value> = self
                    .measures
                    .iter()
                    .filter(|(column, agg)| !column.is_empty() && !agg.is_empty())
                    .map(|(column, agg)| json!({"column": column, "agg": agg}))
                    .collect();
                definition["groupings"] = json!(groupings);
                definition["measures"] = json!(measures);
                if !self.chart.is_empty() {
                    definition["chart"] = json!(self.chart);
                }
            }
        }
        json!({"id": report.id, "version": report.version, "name": self.name.trim(), "description": self.description, "record_type": report.record_type, "kind": kind.key(), "definition": definition})
    }

    /// Draws the editor. `editable` false shows the definition read-only.
    pub fn render(&self, offered: &[ColumnInfo], editable: bool) -> Result<Html> {
        let field = |key: &str, label: &str| {
            let mut field = Field::in_form("def", key)
                .id(format!("def-{}", key.replace('_', "-")))
                .label(label);
            if !editable {
                field = field.disabled();
            }
            field
        };
        let column_choices = |prompt: &str, accept: &dyn Fn(&ColumnInfo) -> bool| -> Vec<Choice> {
            std::iter::once(Choice::new(prompt, ""))
                .chain(
                    offered
                        .iter()
                        .filter(|c| accept(c))
                        .map(|c| Choice::new(&c.label, &c.spec)),
                )
                .collect()
        };
        let ty = |spec: &str| {
            offered
                .iter()
                .find(|c| c.spec == spec)
                .map(|c| c.ty)
                .unwrap_or("")
        };
        let dated = |spec: &str| matches!(ty(spec), "date" | "datetime");
        let numeric = |spec: &str| matches!(ty(spec), "integer" | "float" | "decimal");

        let mut summary = Html::EMPTY;
        if !self.errors.is_empty() {
            let mut list = ErrorSummary::new("report-builder-errors");
            for (key, messages) in &self.errors {
                for message in messages {
                    list.push(target(key), error_sentence(&subject(key, offered), message));
                }
            }
            summary = render(list)?;
        }
        let about = vec![
            render(
                field("name", "Name")
                    .value(&self.name)
                    .required()
                    .errors(self.errors.get("name").cloned().unwrap_or_default()),
            )?,
            render(
                field("description", "Description")
                    .kind(InputKind::Textarea)
                    .value(&self.description)
                    .hint("What question it answers, for the people you share it with."),
            )?,
            render(
                field("kind", "Kind")
                    .kind(InputKind::Select)
                    .options(choices(&[("List", "rows"), ("Summary", "summary")]))
                    .value(&self.kind),
            )?,
        ];

        let mut shape = Vec::new();
        let mut wide = Html::EMPTY;
        if self.kind == "rows" {
            let mut columns = MultiSelect::new("def-columns", "def[columns]")
                .label("Columns")
                .options(
                    offered
                        .iter()
                        .map(|c| Choice::new(&c.label, &c.spec))
                        .collect(),
                )
                .hint("Fields one link away read as Link › Field.")
                .errors(
                    self.errors
                        .get("definition.columns")
                        .cloned()
                        .unwrap_or_default(),
                );
            columns.values = self.columns.clone();
            columns.selection_label = ":count column|:count columns".into();
            columns.placeholder = "Choose columns…".into();
            columns.chips = true;
            columns.wrap_chips = true;
            if !editable {
                columns.attrs = bilibi_ui::Attrs {
                    disabled: true,
                    ..bilibi_ui::Attrs::default()
                };
            }
            wide = render(columns)?;
            shape.push(render(
                field("sort", "Sort by")
                    .kind(InputKind::Select)
                    .options(column_choices("No sort", &|_| true))
                    .value(&self.sort),
            )?);
            shape.push(render(
                field("sort_dir", "Order")
                    .kind(InputKind::Select)
                    .options(choices(&[("Ascending", "asc"), ("Descending", "desc")]))
                    .value(if self.descending { "desc" } else { "asc" }),
            )?);
            shape.push(render(
                field("totals", "Totals line")
                    .kind(InputKind::Checkbox)
                    .checked(self.totals)
                    .hint("Adds a line summing each number column over every row."),
            )?);
        } else {
            for (i, (column, bucket)) in self.groupings.iter().enumerate() {
                let label = if i == 0 { "Group by" } else { "Then by" };
                let prompt = if i == 0 {
                    "No grouping: one total"
                } else {
                    "No second grouping"
                };
                shape.push(render(
                    field(&format!("g{i}"), label)
                        .kind(InputKind::Select)
                        .options(column_choices(prompt, &|_| true))
                        .value(column),
                )?);
                if dated(column) {
                    shape.push(render(
                        field(&format!("b{i}"), "Dates")
                            .kind(InputKind::Select)
                            .options(choices(&BUCKETS))
                            .value(if bucket.is_empty() { "month" } else { bucket }),
                    )?);
                }
            }
        }

        let mut measures = Vec::new();
        if self.kind == "summary" {
            for (i, (column, agg)) in self.measures.iter().enumerate() {
                let label = format!("Measure {}", i + 1);
                let mut aggs = vec![("", "")];
                aggs.push(("Count of values", "count"));
                if numeric(column) {
                    aggs.extend([("Sum", "sum"), ("Average", "avg")]);
                }
                if numeric(column) || dated(column) {
                    aggs.extend([("Lowest", "min"), ("Highest", "max")]);
                }
                let row = [
                    render(
                        field(&format!("m{i}"), &label)
                            .kind(InputKind::Select)
                            .options(column_choices("None", &|_| true))
                            .value(column),
                    )?,
                    render(
                        field(&format!("a{i}"), "Total")
                            .kind(InputKind::Select)
                            .options(choices(&aggs))
                            .value(agg),
                    )?,
                ];
                measures.push(row);
            }
            measures.push([
                render(
                    field("chart", "Chart")
                        .kind(InputKind::Select)
                        .options(choices(&CHART_CHOICES))
                        .value(&self.chart),
                )?,
                Html::EMPTY,
            ]);
        }

        let mut filters = Vec::new();
        for (i, row) in self.filters.iter().enumerate() {
            let value = match row.test.as_str() {
                "empty" | "not_empty" => Html::EMPTY,
                "within" => render(
                    field(&format!("f{i}_value"), "Period")
                        .kind(InputKind::Select)
                        .options(choices(&PERIODS))
                        .value(&row.value),
                )?,
                "one_of" => render(
                    field(&format!("f{i}_value"), "Values")
                        .value(&row.value)
                        .hint("Separate values with commas."),
                )?,
                "range" => render(
                    field(&format!("f{i}_value"), "From")
                        .value(&row.value)
                        .kind(if dated(&row.column) {
                            InputKind::Date
                        } else {
                            InputKind::Text
                        }),
                )?,
                _ => render(
                    field(&format!("f{i}_value"), "Value")
                        .value(&row.value)
                        .kind(if dated(&row.column) {
                            InputKind::Date
                        } else {
                            InputKind::Text
                        }),
                )?,
            };
            let to = if row.test == "range" {
                render(field(&format!("f{i}_to"), "To").value(&row.to).kind(
                    if dated(&row.column) {
                        InputKind::Date
                    } else {
                        InputKind::Text
                    },
                ))?
            } else {
                Html::EMPTY
            };
            let remove_id = format!("def-f{i}-remove");
            let index = i.to_string();
            let remove = if editable {
                render(IconButton {
                    id: &remove_id,
                    kind: IconKind::Neutral,
                    event: "remove_filter",
                    name: "index",
                    value: &index,
                    ..IconButton::new("delete", "Remove this filter")
                })?
            } else {
                Html::EMPTY
            };
            filters.push(FilterView {
                id: format!("def-filter-{i}"),
                column: render(
                    field(&format!("f{i}_column"), "Field")
                        .kind(InputKind::Select)
                        .options(column_choices("Choose a field", &|_| true))
                        .value(&row.column),
                )?,
                test: render(
                    field(&format!("f{i}_test"), "Test")
                        .kind(InputKind::Select)
                        .options(choices(&TESTS))
                        .value(&row.test),
                )?,
                value,
                to,
                ask: render(
                    field(&format!("f{i}_ask"), "Ask when run")
                        .kind(InputKind::Checkbox)
                        .checked(row.ask),
                )?,
                remove,
            });
        }
        let add_filter = if editable && self.filters.len() < MAX_FILTER_ROWS {
            render(AddMore::new("def-add-filter", "Add filter", "add_filter"))?
        } else {
            Html::EMPTY
        };
        let actions = if editable {
            let mut actions = render(Button {
                id: "def-save",
                button_type: "button",
                event: "save_definition",
                variant: ButtonVariant::Primary,
                working: "Saving…",
                body: Html::text("Save report"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                id: "def-reset",
                button_type: "button",
                event: "reset_definition",
                body: Html::text("Discard changes"),
                ..Button::default()
            })?);
            actions
        } else {
            Html::EMPTY
        };
        render(BuilderView {
            summary,
            about,
            wide,
            shape_heading: if self.kind == "rows" {
                "Columns"
            } else {
                "Groups and totals"
            },
            shape,
            measures,
            filters,
            add_filter,
            actions,
        })
    }
}

/// The field an error names, as the summary's link target.
fn target(key: &str) -> String {
    match key {
        "name" => "def-name".into(),
        "description" => "def-description".into(),
        "definition.columns" => "def-columns".into(),
        _ => "report-builder".into(),
    }
}

/// An error's subject in words: a column path becomes its label.
fn subject(key: &str, offered: &[ColumnInfo]) -> String {
    let named = |spec: &str| {
        offered
            .iter()
            .find(|c| c.spec == spec)
            .map_or_else(|| spec.to_owned(), |c| c.label.clone())
    };
    if let Some(spec) = key.strip_prefix("column ") {
        return named(spec);
    }
    if let Some(spec) = key.strip_prefix("filter ") {
        return format!("The filter on {}", named(spec));
    }
    match key {
        "name" => "Name".into(),
        "description" => "Description".into(),
        "definition.columns" => "Columns".into(),
        "definition.groupings" => "Groupings".into(),
        "definition.measures" => "Measures".into(),
        "definition.chart" => "Chart".into(),
        "record_type" => "Records".into(),
        _ => "The report".into(),
    }
}

struct FilterView {
    id: String,
    column: Html,
    test: Html,
    value: Html,
    to: Html,
    ask: Html,
    remove: Html,
}

#[derive(Template)]
#[template(path = "reports_builder.html")]
struct BuilderView {
    summary: Html,
    about: Vec<Html>,
    shape_heading: &'static str,
    /// A rows report's column picker, across the whole section.
    wide: Html,
    shape: Vec<Html>,
    measures: Vec<[Html; 2]>,
    filters: Vec<FilterView>,
    add_filter: Html,
    actions: Html,
}
