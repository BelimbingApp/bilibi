//! A run drawn for the Result tab: a list report's page of rows in the
//! shared flex table, or a summary's chart and its lines with subtotals and
//! the total. A group shown by its own value offers a drill-down that runs
//! the report again narrowed to that group.
use super::render;
use crate::{ColumnInfo, RunResult};
use bilibi_database::{Error, Result};
use bilibi_ui::{
    BuiltinColumn, Chart, ChartKind, ColumnType, FlexRow, FlexTable, Html, IconButton, IconKind,
    ListColumns, PageCounts, Pagination, Series, TableEmpty,
};
use serde_json::Value;

/// A cell's text: numbers as they come, yes/no for booleans, blank for none.
pub(crate) fn cell(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
/// A group's key as a reader names it: a date bucket by its period ("Sep
/// 2025", "2025 Q3", "2025", "Week of 2025-09-01"), anything else as its cell.
pub(crate) fn key_text(value: &Value, bucket: Option<&str>) -> String {
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let text = cell(value);
    let (Some(bucket), Some(year), Some(month)) = (
        bucket,
        text.get(0..4),
        text.get(5..7)
            .and_then(|m| m.parse::<usize>().ok())
            .filter(|m| (1..=12).contains(m)),
    ) else {
        return text;
    };
    match bucket {
        "month" => format!("{} {year}", MONTHS[month - 1]),
        "quarter" => format!("{year} Q{}", (month - 1) / 3 + 1),
        "year" => year.to_owned(),
        "week" => format!("Week of {}", text.get(0..10).unwrap_or(&text)),
        _ => text.get(0..10).unwrap_or(&text).to_owned(),
    }
}
fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}
fn right(ty: &str) -> bool {
    matches!(ty, "integer" | "float" | "decimal")
}
fn builtin(id: &str, label: &str, ty: &str) -> BuiltinColumn {
    let mut column = BuiltinColumn::new(id, label);
    column.kind = match ty {
        "integer" => ColumnType::Integer,
        "float" => ColumnType::Float,
        "decimal" => ColumnType::Decimal,
        "boolean" => ColumnType::Boolean,
        "datetime" => ColumnType::Datetime,
        "date" => ColumnType::Date,
        "choice" => ColumnType::Enum,
        _ => ColumnType::String,
    };
    if right(ty) {
        column = column.right();
    }
    column
}

/// The table columns of a list report's result, for its `ListColumns`.
pub(crate) fn row_columns(columns: &[ColumnInfo]) -> Vec<BuiltinColumn> {
    columns
        .iter()
        .enumerate()
        .map(|(i, c)| builtin(&format!("c{i}"), &c.label, c.ty))
        .collect()
}

/// A list report's page, with its pager.
pub(crate) fn rows(
    host: &ListColumns,
    rows: &[(Value, Vec<Value>)],
    total: i64,
    page: usize,
    page_size: usize,
    sizes: &[u64],
) -> Result<Html> {
    let flex: Vec<FlexRow> = rows
        .iter()
        .enumerate()
        .map(|(n, (_, cells))| {
            cells
                .iter()
                .enumerate()
                .fold(FlexRow::new(n.to_string()), |row, (i, value)| {
                    row.slot(&format!("c{i}"), Html::text(&cell(value)))
                })
        })
        .collect();
    let mut table = FlexTable::new("report-rows", host.flex_columns(), flex);
    table.zoom = host.zoom;
    table.caption = "Report rows";
    table.framed = false;
    if total == 0 {
        table.empty = Some(TableEmpty::Content(Html::text(
            "No records match this report.",
        )));
    }
    let mut body = render(table)?;
    let counts = PageCounts::new(page as u64 + 1, page_size as u64, total.max(0) as u64);
    body.push(&render(Pagination::new(
        "report-pagination",
        counts,
        sizes,
    ))?);
    Ok(body)
}

/// The table columns of a summary: its groups, the record count, then
/// each measure.
pub(crate) fn summary_columns(result: &RunResult) -> Vec<BuiltinColumn> {
    let RunResult::Summary {
        groupings,
        measures,
        ..
    } = result
    else {
        return Vec::new();
    };
    let mut columns: Vec<BuiltinColumn> = groupings
        .iter()
        .enumerate()
        .map(|(i, (c, bucket))| {
            let label = match bucket {
                Some(bucket) => format!("{} ({bucket})", c.label),
                None => c.label.clone(),
            };
            builtin(&format!("g{i}"), &label, "string")
        })
        .collect();
    columns.push(builtin("rows", "Records", "integer"));
    columns.extend(measures.iter().enumerate().map(|(i, (c, agg))| {
        builtin(
            &format!("m{i}"),
            &format!("{} ({})", c.label, agg_label(agg)),
            "decimal",
        )
    }));
    columns
}

/// One drawn summary line, kept so a drill names a line, never a value
/// from the browser.
#[derive(Clone)]
pub(crate) struct Drillable {
    pub keys: Vec<Value>,
    pub subtotal: bool,
}

/// A summary's chart and its lines. Answers the lines a drill may name.
pub(crate) fn summary(
    title: &str,
    host: &ListColumns,
    result: &RunResult,
) -> Result<(Html, Vec<Drillable>)> {
    let RunResult::Summary {
        groupings,
        measures,
        lines,
        total,
        chart,
    } = result
    else {
        return Err(Error::Internal);
    };
    let mut out = Html::EMPTY;
    if let Some(kind) = chart.as_deref().and_then(ChartKind::from_key) {
        out.push(&render(figure(
            "report-chart",
            title,
            kind,
            groupings,
            measures,
            lines,
            total,
        ))?);
    }
    let mut drillable = Vec::new();
    let mut flex = Vec::new();
    for (n, (keys, subtotal, count, values)) in lines.iter().enumerate() {
        let mut row = FlexRow::new(n.to_string());
        for (i, key) in keys.iter().enumerate() {
            let shown = if *subtotal && i + 1 == keys.len() {
                "Subtotal".to_owned()
            } else {
                key_text(key, groupings[i].1)
            };
            let shown = if shown.is_empty() {
                "(none)".to_owned()
            } else {
                shown
            };
            let html = if groupings[i].1.is_none() && !(*subtotal && i + 1 == keys.len()) {
                drill_cell(&shown, n)?
            } else {
                Html::text(&shown)
            };
            row = row.slot(&format!("g{i}"), html);
        }
        row = row.slot("rows", Html::text(&count.to_string()));
        for (i, value) in values.iter().enumerate() {
            row = row.slot(&format!("m{i}"), Html::text(&cell(value)));
        }
        row.dom_id = Some(format!("report-line-{n}"));
        drillable.push(Drillable {
            keys: keys.clone(),
            subtotal: *subtotal,
        });
        flex.push(row);
    }
    let mut last = FlexRow::new("total");
    if !groupings.is_empty() {
        last = last.slot("g0", Html::text("Total"));
    }
    last = last.slot("rows", Html::text(&total.0.to_string()));
    for (i, value) in total.1.iter().enumerate() {
        last = last.slot(&format!("m{i}"), Html::text(&cell(value)));
    }
    last.dom_id = Some("report-line-total".into());
    flex.push(last);
    let mut table = FlexTable::new("report-summary", host.flex_columns(), flex);
    table.zoom = host.zoom;
    table.caption = "Report summary";
    table.framed = false;
    out.push(&render(table)?);
    Ok((render(Stack { parts: vec![out] })?, drillable))
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::stack(parts=parts, wide=true) %}{% endcall %}"#,
    ext = "html"
)]
struct Stack {
    parts: Vec<Html>,
}

/// A group's value with its drill button on the same line.
#[derive(askama::Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::removable(label=text, button=button) %}{% endcall %}"#,
    ext = "html"
)]
struct DrillCell<'a> {
    text: &'a str,
    button: Html,
}

fn drill_cell(shown: &str, line: usize) -> Result<Html> {
    let id = format!("report-drill-{line}");
    let value = line.to_string();
    let label = format!("Show only {shown}");
    let button = render(IconButton {
        id: &id,
        kind: IconKind::Neutral,
        event: "drill",
        name: "line",
        value: &value,
        ..IconButton::new("filter", &label)
    })?;
    render(DrillCell {
        text: shown,
        button,
    })
}

pub(crate) fn agg_label(agg: &str) -> &'static str {
    match agg {
        "count" => "count",
        "sum" => "sum",
        "avg" => "average",
        "min" => "lowest",
        "max" => "highest",
        _ => "",
    }
}

/// The chart of a summary. One grouping: a category per group, a series per
/// measure (or the record count). Two: a category per first group and a
/// series per second group, of the first measure. None: the total as a
/// number.
pub(crate) fn figure(
    id: &str,
    title: &str,
    kind: ChartKind,
    groupings: &[(ColumnInfo, Option<&'static str>)],
    measures: &[(ColumnInfo, &'static str)],
    lines: &[(Vec<Value>, bool, i64, Vec<Value>)],
    total: &(i64, Vec<Value>),
) -> Chart {
    let measure_name = |i: usize| {
        measures.get(i).map_or_else(
            || "Records".to_owned(),
            |(c, agg)| format!("{} ({})", c.label, agg_label(agg)),
        )
    };
    let pick = |count: i64, values: &[Value]| -> Option<f64> {
        if measures.is_empty() {
            Some(count as f64)
        } else {
            values.first().and_then(number)
        }
    };
    let depth = groupings.len();
    let key = |i: usize, v: &Value| key_text(v, groupings.get(i).and_then(|g| g.1));
    let detail: Vec<_> = lines.iter().filter(|l| !l.1).collect();
    if depth == 0 || kind == ChartKind::Kpi {
        let series = vec![Series {
            name: measure_name(0),
            values: vec![pick(total.0, &total.1)],
        }];
        return Chart::new(id, title, ChartKind::Kpi, Vec::new(), series);
    }
    if depth == 1 {
        let categories: Vec<String> = detail
            .iter()
            .map(|l| l.0.first().map(|v| key(0, v)).unwrap_or_default())
            .collect();
        let series = if measures.is_empty() {
            vec![Series {
                name: measure_name(0),
                values: detail.iter().map(|l| Some(l.2 as f64)).collect(),
            }]
        } else {
            (0..measures.len())
                .map(|i| Series {
                    name: measure_name(i),
                    values: detail.iter().map(|l| l.3.get(i).and_then(number)).collect(),
                })
                .collect()
        };
        return Chart::new(id, title, kind, categories, series);
    }
    let mut categories: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for l in &detail {
        let (a, b) = (
            l.0.first().map(|v| key(0, v)).unwrap_or_default(),
            l.0.get(1).map(|v| key(1, v)).unwrap_or_default(),
        );
        if !categories.contains(&a) {
            categories.push(a);
        }
        if !names.contains(&b) {
            names.push(b);
        }
    }
    let series = names
        .iter()
        .map(|name| Series {
            name: if name.is_empty() {
                "(none)".into()
            } else {
                name.clone()
            },
            values: categories
                .iter()
                .map(|category| {
                    detail
                        .iter()
                        .find(|l| {
                            l.0.first().map(|v| key(0, v)).as_ref() == Some(category)
                                && l.0.get(1).map(|v| key(1, v)).as_ref() == Some(name)
                        })
                        .and_then(|l| pick(l.2, &l.3))
                })
                .collect(),
        })
        .collect();
    Chart::new(id, title, kind, categories, series)
}
