//! A run drawn for the Result tab: a list report's page of rows in the
//! shared flex table (each row opening its record's page where Base Records
//! has a kind for it, and an optional totals line), a summary's chart and
//! its lines with subtotals and the total, or a matrix (one grouping down,
//! one across). A group, a total or a chart bar drills into the records
//! behind it: the page names a line it drew, never a value from the browser.
use super::render;
use crate::{ColumnInfo, RunResult};
use bilibi_database::{Error, Result};
use bilibi_ui::{
    BuiltinColumn, Chart, ChartKind, ColumnType, FlexRow, FlexTable, Html, IconButton, IconKind,
    ListColumns, PageCounts, Pagination, RecordLink, Series, TableEmpty,
};
use serde_json::Value;

/// One line of a summary as a run carries it: keys, subtotal flag, record
/// count, one value per measure.
type SummaryLine = (Vec<Value>, bool, i64, Vec<Value>);

/// Values across a matrix at most; more reads better as a summary.
pub(crate) const MAX_ACROSS: usize = 24;

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
/// The first and last day of the period a date bucket's key starts, as
/// `YYYY-MM-DD`, for the range filter a drill into it adds.
pub(crate) fn bucket_days(value: &Value, bucket: &str) -> Option<(String, String)> {
    use jiff::{civil::Date, ToSpan};
    let first: Date = cell(value).get(0..10)?.parse().ok()?;
    let last = match bucket {
        "day" => first,
        "week" => first.checked_add(6.days()).ok()?,
        "month" => first.last_of_month(),
        "quarter" => first
            .checked_add(3.months())
            .ok()?
            .checked_sub(1.day())
            .ok()?,
        "year" => first.last_of_year(),
        _ => return None,
    };
    Some((first.to_string(), last.to_string()))
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

/// One page of a list, as the result carries it.
pub(crate) struct RowsPage<'a> {
    pub rows: &'a [(Value, Vec<Value>)],
    pub pages: &'a [Option<String>],
    pub total: i64,
    pub page: usize,
    pub page_size: usize,
    pub totals: Option<&'a [Value]>,
}

/// A list report's page, with its totals line and pager. The first cell of
/// a row links to the record's page when Base Records names one for it.
pub(crate) fn rows(host: &ListColumns, list: &RowsPage<'_>, sizes: &[u64]) -> Result<Html> {
    let mut flex = Vec::with_capacity(list.rows.len() + 1);
    for (n, (_, cells)) in list.rows.iter().enumerate() {
        let mut row = FlexRow::new(n.to_string());
        for (i, value) in cells.iter().enumerate() {
            let text = cell(value);
            let html = match list.pages.get(n).and_then(Option::as_ref) {
                Some(href) if i == 0 => render(RecordLink {
                    workspace: false,
                    kind: "record",
                    record_id: "",
                    href,
                    class: "font-medium text-link hover:underline",
                    body: Html::text(if text.is_empty() { "(none)" } else { &text }),
                })?,
                _ => Html::text(&text),
            };
            row = row.slot(&format!("c{i}"), html);
        }
        flex.push(row);
    }
    // No rows, no totals line: the table says nothing matches instead.
    if let Some(totals) = list.totals.filter(|_| list.total > 0) {
        let mut last = FlexRow::new("total");
        for (i, value) in totals.iter().enumerate() {
            let text = if i == 0 && value.is_null() {
                "Total".to_owned()
            } else {
                cell(value)
            };
            last = last.slot(&format!("c{i}"), Html::text(&text));
        }
        last.dom_id = Some("report-rows-total".into());
        flex.push(last);
    }
    let mut table = FlexTable::new("report-rows", host.flex_columns(), flex);
    table.zoom = host.zoom;
    table.caption = "Report rows";
    table.framed = false;
    if list.total == 0 {
        table.empty = Some(TableEmpty::Content(Html::text(
            "No records match this report.",
        )));
    }
    let mut body = render(table)?;
    let counts = PageCounts::new(
        list.page as u64 + 1,
        list.page_size as u64,
        list.total.max(0) as u64,
    );
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
fn drillables(lines: &[SummaryLine]) -> Vec<Drillable> {
    lines
        .iter()
        .map(|(keys, subtotal, _, _)| Drillable {
            keys: keys.clone(),
            subtotal: *subtotal,
        })
        .collect()
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
            true,
        ))?);
    }
    let mut flex = Vec::new();
    for (n, (keys, subtotal, count, values)) in lines.iter().enumerate() {
        let mut row = FlexRow::new(n.to_string());
        for (i, key) in keys.iter().enumerate() {
            let placeholder = *subtotal && i + 1 == keys.len();
            let shown = if placeholder {
                "Subtotal".to_owned()
            } else {
                key_text(key, groupings[i].1)
            };
            let shown = if shown.is_empty() {
                "(none)".to_owned()
            } else {
                shown
            };
            // The innermost key drills the whole line; an outer one only on
            // its subtotal, so one group is one control.
            let html =
                if !placeholder && (i + 1 == keys.len() || (*subtotal && i + 2 == keys.len())) {
                    drill_cell(&shown, &n.to_string())?
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
        flex.push(row);
    }
    let mut last = FlexRow::new("total");
    let label = if groupings.is_empty() {
        None
    } else {
        Some("Total")
    };
    // The total opens every record behind it; with no grouping its count
    // carries the control, so a one-number summary drills too.
    let count = total.0.to_string();
    last = match label {
        Some(label) => last
            .slot("g0", drill_cell(label, "total")?)
            .slot("rows", Html::text(&count)),
        None => last.slot("rows", drill_cell(&count, "total")?),
    };
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
    Ok((render(Stack { parts: vec![out] })?, drillables(lines)))
}

/// What a matrix lays out: the values down (first grouping) and across
/// (second), in the order the read returned them.
struct Axes {
    down: Vec<Value>,
    across: Vec<Value>,
}
fn axes(lines: &[SummaryLine]) -> Axes {
    let mut axes = Axes {
        down: Vec::new(),
        across: Vec::new(),
    };
    for (keys, subtotal, _, _) in lines {
        if *subtotal || keys.len() != 2 {
            continue;
        }
        if !axes.down.contains(&keys[0]) {
            axes.down.push(keys[0].clone());
        }
        if !axes.across.contains(&keys[1]) {
            axes.across.push(keys[1].clone());
        }
    }
    axes
}

/// The table columns of a matrix: the down grouping, one per value across,
/// then the row total. `None` when too many values run across.
pub(crate) fn matrix_columns(result: &RunResult) -> Option<Vec<BuiltinColumn>> {
    let RunResult::Summary {
        groupings, lines, ..
    } = result
    else {
        return None;
    };
    let (down, across) = (groupings.first()?, groupings.get(1)?);
    let axes = axes(lines);
    if axes.across.len() > MAX_ACROSS {
        return None;
    }
    let mut columns = vec![builtin("down", &down.0.label, "string")];
    columns.extend(axes.across.iter().enumerate().map(|(i, value)| {
        let label = key_text(value, across.1);
        builtin(
            &format!("a{i}"),
            if label.is_empty() { "(none)" } else { &label },
            "decimal",
        )
    }));
    columns.push(builtin("total", "Total", "decimal"));
    Some(columns)
}

/// A matrix laid out once, for the page and the file alike: a row per value
/// down, a column per value across, the measure (or the record count) in
/// each cell, a row total, column totals where the measure adds up across
/// rows (count, sum, lowest, highest; blank otherwise) and the grand total.
pub(crate) struct Pivot {
    pub down: Vec<Value>,
    pub across: Vec<Value>,
    /// Per row down: one cell per value across.
    pub cells: Vec<Vec<Option<Value>>>,
    /// Per row down: its subtotal line (the row total), the drill target.
    pub subtotals: Vec<Option<usize>>,
    pub row_totals: Vec<Option<Value>>,
    pub column_totals: Vec<String>,
    pub grand: Value,
}

pub(crate) fn pivot(
    measures: &[(ColumnInfo, &'static str)],
    lines: &[SummaryLine],
    total: &(i64, Vec<Value>),
) -> Pivot {
    let agg = measures.first().map(|(_, agg)| *agg);
    let value = |count: i64, values: &[Value]| -> Value {
        if measures.is_empty() {
            Value::from(count)
        } else {
            values.first().cloned().unwrap_or(Value::Null)
        }
    };
    let Axes { down, across } = axes(lines);
    let mut sums: Vec<Option<f64>> = vec![None; across.len()];
    let (mut cells, mut subtotals, mut row_totals) = (Vec::new(), Vec::new(), Vec::new());
    for d in &down {
        let mut row = Vec::with_capacity(across.len());
        for (c, a) in across.iter().enumerate() {
            let shown = lines
                .iter()
                .find(|(keys, sub, _, _)| {
                    !*sub && keys.first() == Some(d) && keys.get(1) == Some(a)
                })
                .map(|(_, _, count, values)| value(*count, values));
            if let Some(v) = shown.as_ref().and_then(number) {
                let slot = &mut sums[c];
                *slot = Some(match (agg, *slot) {
                    (_, None) => v,
                    (Some("min"), Some(t)) => t.min(v),
                    (Some("max"), Some(t)) => t.max(v),
                    (_, Some(t)) => t + v,
                });
            }
            row.push(shown);
        }
        let subtotal = lines
            .iter()
            .position(|(keys, sub, _, _)| *sub && keys.first() == Some(d));
        row_totals.push(subtotal.map(|line| value(lines[line].2, &lines[line].3)));
        subtotals.push(subtotal);
        cells.push(row);
    }
    let adds_up = matches!(agg, None | Some("count" | "sum" | "min" | "max"));
    let column_totals = sums
        .iter()
        .map(|sum| match sum {
            Some(v) if adds_up => format_total(*v),
            _ => String::new(),
        })
        .collect();
    Pivot {
        down,
        across,
        cells,
        subtotals,
        row_totals,
        column_totals,
        grand: value(total.0, &total.1),
    }
}

/// A matrix on the page ([`Pivot`]). Each row's label drills into its
/// records.
pub(crate) fn matrix(
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
            true,
        ))?);
    }
    let pivot = pivot(measures, lines, total);
    let mut flex = Vec::new();
    for (r, down) in pivot.down.iter().enumerate() {
        let mut row = FlexRow::new(format!("down-{r}"));
        let label = key_text(down, groupings[0].1);
        let label = if label.is_empty() {
            "(none)".to_owned()
        } else {
            label
        };
        row = row.slot(
            "down",
            match pivot.subtotals[r] {
                Some(line) => drill_cell(&label, &line.to_string())?,
                None => Html::text(&label),
            },
        );
        for (c, shown) in pivot.cells[r].iter().enumerate() {
            row = row.slot(
                &format!("a{c}"),
                Html::text(&shown.as_ref().map(cell).unwrap_or_default()),
            );
        }
        if let Some(total) = &pivot.row_totals[r] {
            row = row.slot("total", Html::text(&cell(total)));
        }
        row.dom_id = Some(format!("report-down-{r}"));
        flex.push(row);
    }
    let mut last = FlexRow::new("total").slot("down", drill_cell("Total", "total")?);
    for (c, text) in pivot.column_totals.iter().enumerate() {
        last = last.slot(&format!("a{c}"), Html::text(text));
    }
    last = last.slot("total", Html::text(&cell(&pivot.grand)));
    last.dom_id = Some("report-line-total".into());
    flex.push(last);
    let mut table = FlexTable::new("report-matrix", host.flex_columns(), flex);
    table.zoom = host.zoom;
    table.caption = "Report matrix";
    table.framed = false;
    out.push(&render(table)?);
    Ok((render(Stack { parts: vec![out] })?, drillables(lines)))
}

fn format_total(v: f64) -> String {
    if v.fract() == 0.0 && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{v:.2}")
    }
}

#[derive(askama::Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::stack(parts=parts, wide=true) %}{% endcall %}"#,
    ext = "html"
)]
pub(crate) struct Stack {
    pub parts: Vec<Html>,
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

fn drill_cell(shown: &str, line: &str) -> Result<Html> {
    let id = format!("report-drill-{line}");
    let label = if line == "total" {
        "Show every record behind the total".to_owned()
    } else {
        format!("Show the records behind {shown}")
    };
    let button = render(IconButton {
        id: &id,
        kind: IconKind::Neutral,
        event: "drill",
        name: "line",
        value: line,
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
/// number. With `drill`, each bar or slice names the line it draws.
#[allow(clippy::too_many_arguments)]
pub(crate) fn figure(
    id: &str,
    title: &str,
    kind: ChartKind,
    groupings: &[(ColumnInfo, Option<&'static str>)],
    measures: &[(ColumnInfo, &'static str)],
    lines: &[SummaryLine],
    total: &(i64, Vec<Value>),
    drill: bool,
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
    // The detail lines with their position in `lines`, which a drill names.
    let detail: Vec<(usize, &SummaryLine)> =
        lines.iter().enumerate().filter(|(_, l)| !l.1).collect();
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
            .map(|(_, l)| l.0.first().map(|v| key(0, v)).unwrap_or_default())
            .collect();
        let series = if measures.is_empty() {
            vec![Series {
                name: measure_name(0),
                values: detail.iter().map(|(_, l)| Some(l.2 as f64)).collect(),
            }]
        } else {
            (0..measures.len())
                .map(|i| Series {
                    name: measure_name(i),
                    values: detail
                        .iter()
                        .map(|(_, l)| l.3.get(i).and_then(number))
                        .collect(),
                })
                .collect()
        };
        let chart = Chart::new(id, title, kind, categories, series);
        return if drill {
            chart.drill("drill", |_, category| {
                detail.get(category).map(|(n, _)| n.to_string())
            })
        } else {
            chart
        };
    }
    let mut categories: Vec<String> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    for (_, l) in &detail {
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
    let find = |category: &String, name: &String| {
        detail.iter().find(|(_, l)| {
            l.0.first().map(|v| key(0, v)).as_ref() == Some(category)
                && l.0.get(1).map(|v| key(1, v)).as_ref() == Some(name)
        })
    };
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
                .map(|category| find(category, name).and_then(|(_, l)| pick(l.2, &l.3)))
                .collect(),
        })
        .collect();
    let chart = Chart::new(id, title, kind, categories.clone(), series);
    if drill {
        chart.drill("drill", |s, c| {
            let (name, category) = (names.get(s)?, categories.get(c)?);
            find(category, name).map(|(n, _)| n.to_string())
        })
    } else {
        chart
    }
}
