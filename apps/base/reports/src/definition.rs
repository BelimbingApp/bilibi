//! A report's saved question: what it reads and how, as stored in
//! `base_reports.definition`. Parsing refuses unknown fields and checks every
//! bound and shape here; whether each column exists and suits its use is the
//! reader's Grid catalog's answer, at save time for the author and again at
//! every run for the reader.
use bilibi_agent_api::schema::FieldErrors;
use bilibi_grid::{
    Agg, Bucket, Grouping, Measure, Period, Sort, Test, MAX_CHOICES, MAX_FILTERS, MAX_GROUPINGS,
    MAX_MEASURES, MAX_SORTS, MAX_TEXT,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// Columns a Rows report shows at most (Grid's projection bound).
pub const MAX_COLUMNS: usize = 32;
/// Chart kinds a Summary report may draw (`bilibi_ui::ChartKind` keys).
pub const CHARTS: [&str; 6] = ["kpi", "bar", "stacked_bar", "line", "pie", "donut"];
const TESTS: [&str; 7] = [
    "equals",
    "one_of",
    "contains",
    "range",
    "within",
    "empty",
    "not_empty",
];

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// A filtered, sorted list of chosen columns.
    Rows,
    /// Rows grouped by up to two columns, with measures, subtotals and a total.
    Summary,
}
impl Kind {
    pub fn key(self) -> &'static str {
        match self {
            Self::Rows => "rows",
            Self::Summary => "summary",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "rows" => Some(Self::Rows),
            "summary" => Some(Self::Summary),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// Rows: the column paths shown, in order.
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub filters: Vec<FilterDef>,
    /// Summary: up to two.
    #[serde(default)]
    pub groupings: Vec<GroupingDef>,
    /// Summary: up to eight; the row count is always given.
    #[serde(default)]
    pub measures: Vec<MeasureDef>,
    /// Rows: up to three.
    #[serde(default)]
    pub sort: Vec<SortDef>,
    /// Summary: one of [`CHARTS`], or none.
    #[serde(default)]
    pub chart: Option<String>,
}

/// One filter. `value` depends on `test`: a scalar for `equals`, an array
/// for `one_of`, text for `contains`, `{min,max}` for `range` and a period
/// key for `within`; nothing for `empty` and `not_empty`. With `ask`, the
/// reader gives the value at run time and the stored one is the default.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilterDef {
    pub column: String,
    pub test: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub ask: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupingDef {
    pub column: String,
    /// `day`, `week`, `month`, `quarter` or `year`, for a date column.
    #[serde(default)]
    pub bucket: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasureDef {
    pub column: String,
    /// `count`, `sum`, `avg`, `min` or `max`.
    pub agg: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SortDef {
    pub column: String,
    #[serde(default)]
    pub descending: bool,
}

/// The JSON schema of a definition, for agent operations.
pub fn schema() -> Value {
    let column = json!({"type":"string","maxLength":160});
    json!({"type":"object","properties":{
        "columns":{"type":"array","items":column},
        "filters":{"type":"array","items":{"type":"object","required":["column","test"],"properties":{
            "column":column,
            "test":{"type":"string","enum":TESTS},
            "value":{"type":"string","maxLength":256,"description":"For equals a value; for contains the text; for within a period (today, this_week, last_7_days, last_30_days, this_month, this_quarter, this_year). For one_of and range use values and min/max."},
            "values":{"type":"array","items":{"type":"string","maxLength":256}},
            "min":{"type":"string","maxLength":32},
            "max":{"type":"string","maxLength":32},
            "ask":{"type":"boolean"}
        }}},
        "groupings":{"type":"array","items":{"type":"object","required":["column"],"properties":{
            "column":column,
            "bucket":{"type":"string","enum":["day","week","month","quarter","year"]}
        }}},
        "measures":{"type":"array","items":{"type":"object","required":["column","agg"],"properties":{
            "column":column,
            "agg":{"type":"string","enum":["count","sum","avg","min","max"]}
        }}},
        "sort":{"type":"array","items":{"type":"object","required":["column"],"properties":{
            "column":column,
            "descending":{"type":"boolean"}
        }}},
        "chart":{"type":"string","enum":CHARTS}
    }})
}

fn err(errors: &mut FieldErrors, field: impl Into<String>, message: &str) {
    errors.entry(field.into()).or_default().push(message.into());
}

impl Definition {
    /// Parses the agent schema's flattened filter shape (`value`, `values`,
    /// `min`, `max`) or the stored shape (`value` only) into a definition.
    pub fn from_input(input: &Value) -> Result<Self, FieldErrors> {
        let mut input = input.clone();
        if let Some(filters) = input.get_mut("filters").and_then(Value::as_array_mut) {
            for filter in filters {
                let Some(object) = filter.as_object_mut() else {
                    continue;
                };
                let values = object.remove("values");
                let min = object.remove("min");
                let max = object.remove("max");
                match object.get("test").and_then(Value::as_str) {
                    Some("one_of") if values.is_some() => {
                        object.insert("value".into(), values.unwrap_or_default());
                    }
                    Some("range") if min.is_some() || max.is_some() => {
                        object.insert(
                            "value".into(),
                            json!({"min": min.unwrap_or(Value::Null), "max": max.unwrap_or(Value::Null)}),
                        );
                    }
                    _ => {}
                }
            }
        }
        serde_json::from_value(input).map_err(|error| {
            let mut errors = FieldErrors::new();
            err(
                &mut errors,
                "definition",
                &format!("is not a report definition: {error}"),
            );
            errors
        })
    }

    /// The bounds and shapes for a report of `kind`. Column existence and
    /// type fit are the catalog's answer ([`crate::Reports`] checks them).
    pub fn check(&self, kind: Kind) -> Result<(), FieldErrors> {
        let mut errors = FieldErrors::new();
        match kind {
            Kind::Rows => {
                if self.columns.is_empty() || self.columns.len() > MAX_COLUMNS {
                    err(
                        &mut errors,
                        "definition.columns",
                        "a rows report shows 1 to 32 columns",
                    );
                }
                if !self.groupings.is_empty() || !self.measures.is_empty() || self.chart.is_some() {
                    err(
                        &mut errors,
                        "definition",
                        "a rows report has no groupings, measures or chart",
                    );
                }
                if self.sort.len() > MAX_SORTS {
                    err(&mut errors, "definition.sort", "at most 3 sorts");
                }
            }
            Kind::Summary => {
                if !self.columns.is_empty() || !self.sort.is_empty() {
                    err(
                        &mut errors,
                        "definition",
                        "a summary report has no columns or sort",
                    );
                }
                if self.groupings.len() > MAX_GROUPINGS {
                    err(&mut errors, "definition.groupings", "at most 2 groupings");
                }
                if self.measures.len() > MAX_MEASURES {
                    err(&mut errors, "definition.measures", "at most 8 measures");
                }
                if let Some(chart) = &self.chart {
                    if !CHARTS.contains(&chart.as_str()) {
                        err(&mut errors, "definition.chart", "is not a chart kind");
                    }
                }
            }
        }
        if self.filters.len() > MAX_FILTERS {
            err(&mut errors, "definition.filters", "at most 16 filters");
        }
        for (i, filter) in self.filters.iter().enumerate() {
            if let Err(message) = filter.test(None) {
                err(&mut errors, format!("definition.filters[{i}]"), message);
            }
        }
        for (i, grouping) in self.groupings.iter().enumerate() {
            if let Some(bucket) = &grouping.bucket {
                if Bucket::from_key(bucket).is_none() {
                    err(
                        &mut errors,
                        format!("definition.groupings[{i}].bucket"),
                        "is not a period",
                    );
                }
            }
        }
        for (i, measure) in self.measures.iter().enumerate() {
            if !matches!(
                measure.agg.as_str(),
                "count" | "sum" | "avg" | "min" | "max"
            ) {
                err(
                    &mut errors,
                    format!("definition.measures[{i}].agg"),
                    "is not count, sum, avg, min or max",
                );
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors)
        }
    }

    /// Every column path the definition names, for catalog checks.
    pub fn specs(&self) -> Vec<&str> {
        self.columns
            .iter()
            .map(String::as_str)
            .chain(self.filters.iter().map(|f| f.column.as_str()))
            .chain(self.groupings.iter().map(|g| g.column.as_str()))
            .chain(self.measures.iter().map(|m| m.column.as_str()))
            .chain(self.sort.iter().map(|s| s.column.as_str()))
            .collect()
    }

    pub fn groupings(&self) -> Vec<Grouping> {
        self.groupings
            .iter()
            .map(|g| Grouping {
                column: g.column.clone(),
                bucket: g.bucket.as_deref().and_then(Bucket::from_key),
            })
            .collect()
    }
    pub fn measures(&self) -> Vec<Measure> {
        self.measures
            .iter()
            .filter_map(|m| {
                Agg::from_key(&m.agg).map(|agg| Measure {
                    column: m.column.clone(),
                    agg,
                })
            })
            .collect()
    }
    pub fn sorts(&self) -> Vec<Sort> {
        self.sort
            .iter()
            .map(|s| Sort {
                column: s.column.clone(),
                descending: s.descending,
            })
            .collect()
    }
}

impl FilterDef {
    /// The Grid test for this filter, with the reader's answer for an `ask`
    /// filter (`None` keeps the stored value). An `ask` filter left without
    /// any value is skipped (`Ok(None)`).
    pub fn test(&self, answer: Option<&Value>) -> Result<Option<Test>, &'static str> {
        let value = match (self.ask, answer) {
            (true, Some(answer)) => answer,
            _ => &self.value,
        };
        let blank = value.is_null() || value.as_str().is_some_and(str::is_empty);
        if self.ask && blank && !matches!(self.test.as_str(), "empty" | "not_empty") {
            return Ok(None);
        }
        let text = |v: &Value| v.as_str().is_some_and(|s| s.len() <= MAX_TEXT);
        let scalar = |v: &Value| v.is_number() || v.is_boolean() || text(v);
        Ok(Some(match self.test.as_str() {
            "equals" if scalar(value) => Test::Equals(value.clone()),
            "equals" => return Err("equals takes one number, text or yes/no"),
            "one_of" => match value.as_array() {
                Some(values)
                    if !values.is_empty()
                        && values.len() <= MAX_CHOICES
                        && values.iter().all(scalar) =>
                {
                    Test::OneOf(values.clone())
                }
                _ => return Err("one of takes 1 to 100 values"),
            },
            "contains" if text(value) && !blank => {
                Test::Contains(value.as_str().unwrap_or_default().to_owned())
            }
            "contains" => return Err("contains takes text"),
            "range" => {
                let bound = |key: &str| value.get(key).filter(|v| !v.is_null()).cloned();
                let (min, max) = (bound("min"), bound("max"));
                if min.is_none() && max.is_none() {
                    return Err("a range needs a minimum or a maximum");
                }
                // Text from a form or an agent: a number when it reads as one.
                let typed = |v: Value| match v {
                    Value::String(s) => s
                        .parse::<f64>()
                        .ok()
                        .and_then(serde_json::Number::from_f64)
                        .map(Value::Number)
                        .unwrap_or(Value::String(s)),
                    other => other,
                };
                Test::Range {
                    min: min.map(typed),
                    max: max.map(typed),
                }
            }
            "within" => match value.as_str().and_then(Period::from_key) {
                Some(period) => Test::Within(period),
                None => return Err("within takes a period such as this_month"),
            },
            "empty" => Test::Empty,
            "not_empty" => Test::NotEmpty,
            _ => return Err("is not a filter test"),
        }))
    }
}
