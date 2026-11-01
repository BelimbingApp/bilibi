//! A lens is how one column's values are shown: the raw value, a bar scaled to
//! the column's range, a colour band, a trend (the last twelve months of a
//! rollup as a sparkline), or the change since a date. The same column can be
//! switched between the lenses its type allows; the data does not change, the
//! reading does.
//!
//! [`cell`] prepares one projected value for every lens at once: its text, its
//! position on the column's range (0..1), the band that position falls in, a
//! trend's series and a delta's change, as the shared [`Cell`] the FlexTable
//! draws. Dates and timestamps stay typed through every lens: a date is a
//! zone-free `Typed::Date`, a timestamp a `Typed::Instant` carrying both server
//! clock texts from the request's [`DisplayContext`], never preformatted text.
use crate::{Agg, Column, ColumnKind, FieldType};
use bilibi_datetime::{DisplayContext, Format, Precision};
use bilibi_ui::flex_table::{self, Cell, ColumnType, FlexColumn, Instant, Scale};
use jiff::{civil, Timestamp};
use serde_json::Value;

pub use bilibi_ui::flex_table::Lens;

/// Above this planner estimate the page warns that scaling reads heavily.
pub const HEAVY_COST: f64 = 20_000.0;

/// A stats estimate as the FlexTable shows it; `None` when nothing was read.
pub fn cost(estimate: Option<f64>) -> Option<flex_table::Cost> {
    estimate.map(|estimate| flex_table::Cost {
        estimate,
        heavy: estimate > HEAVY_COST,
    })
}

/// Sequential bands a numeric position falls in.
pub const BANDS: u8 = 5;
/// Months a trend covers, oldest first, ending with the current one.
pub const TREND_MONTHS: usize = 12;

const SCALED: [Lens; 3] = [Lens::Value, Lens::Bar, Lens::Band];
const BANDED: [Lens; 2] = [Lens::Value, Lens::Band];

/// The lenses a column's values allow. A count or sum over a dated many-link
/// can show its trend and its change since a date; anything numeric scales to
/// a bar; everything bands.
pub fn available(column: &Column) -> &'static [Lens] {
    if trends(column) {
        &Lens::ALL
    } else if column.numeric() {
        &SCALED
    } else {
        &BANDED
    }
}

/// Whether the column is a count or sum over a dated many-link target: the
/// columns a trend and a change since a date are known for.
pub fn trends(column: &Column) -> bool {
    column.kind == ColumnKind::Rollup
        && matches!(column.agg, Some(Agg::Count | Agg::Sum))
        && column.time_field.is_some()
}

/// A lens name the column allows, falling back to the value lens.
pub fn normalize(name: &str, column: &Column) -> Lens {
    Lens::parse(name)
        .filter(|lens| available(column).contains(lens))
        .unwrap_or_default()
}

/// The smallest and largest value of a numeric column over every row the
/// scope may read ([`crate::stats`]), as projected JSON.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Range {
    pub min: Value,
    pub max: Value,
}

/// What a trend or delta lens adds to a value: the twelve monthly aggregates
/// and the aggregate as it stood before the comparison date.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras<'a> {
    pub series: Option<&'a [f64]>,
    pub before: Option<&'a Value>,
}

/// The FlexTable column for a resolved Grid column wearing `lens`. A walked
/// column never sorts (the page owns the order of its rows); a numeric one is
/// right-aligned.
pub fn flex_column(column: &Column, lens: Lens) -> FlexColumn {
    // Grid paths contain `.` link separators and `:` aggregate separators; the
    // shared table requires DOM-safe ids. Schema identifiers themselves are
    // underscore-delimited, so these substitutions keep path ids distinct.
    let id = column.id.replace(['.', ':'], "-");
    let mut flex = FlexColumn::new(&id, &column.spec, &column.label)
        .short(&column.short_label)
        .typed(column_type(column.ty))
        .kind(match column.kind {
            ColumnKind::Field => flex_table::ColumnKind::Field,
            ColumnKind::Rollup => flex_table::ColumnKind::Rollup,
        })
        .lenses(Lens::Value, available(column))
        .unsortable();
    flex.wear(lens);
    if column.numeric() {
        flex = flex.right();
    }
    flex
}

/// The FlexTable's alignment type for a field type.
pub fn column_type(ty: FieldType) -> ColumnType {
    match ty {
        FieldType::Integer => ColumnType::Integer,
        FieldType::Float => ColumnType::Float,
        FieldType::Decimal => ColumnType::Decimal,
        FieldType::String => ColumnType::String,
        FieldType::Boolean => ColumnType::Boolean,
        FieldType::Date => ColumnType::Date,
        FieldType::DateTime => ColumnType::Datetime,
        FieldType::Enum => ColumnType::Enum,
    }
}

/// Prepares one projected value: its text (typed for dates and timestamps),
/// its 0..1 position within `range`, and the band that position falls in.
/// Text and enum values get a categorical band from a stable hash, booleans a
/// binary one, so a colour band reads them too. Under the delta lens the text
/// states the change since the date; under the trend lens the cell carries the
/// monthly series.
pub fn cell(
    value: &Value,
    column: &Column,
    range: Option<&Range>,
    lens: Lens,
    extras: Extras<'_>,
    display: &DisplayContext,
) -> Cell {
    let mut cell = typed(value, column, display);
    let delta = delta(value, column, lens, extras);
    if let Some(delta) = delta.filter(|_| !cell.text.is_empty()) {
        cell.text = format!("{} ({})", cell.text, signed(delta));
    }
    if let Some((n, scale)) = position(value, column, range) {
        cell = cell.at(n, band(n, scale, value), scale);
    }
    if lens == Lens::Trend {
        if let Some(series) = extras.series {
            cell = cell.series(series.to_vec());
        }
    }
    cell
}

/// The text of a value as a cell shows it when it is not a date or timestamp.
/// Figures follow the column's type: decimals and floats with two places.
pub fn text(value: &Value, column: &Column) -> String {
    match value {
        Value::Null => String::new(),
        Value::Bool(true) => "Yes".into(),
        Value::Bool(false) => "No".into(),
        Value::Number(number) => match column.ty {
            FieldType::Decimal | FieldType::Float => {
                number.as_f64().map(two_places).unwrap_or_default()
            }
            _ => number.to_string(),
        },
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A projected date or timestamp keeps its type; everything else is text.
fn typed(value: &Value, column: &Column, display: &DisplayContext) -> Cell {
    let Value::String(raw) = value else {
        return Cell::text(text(value, column));
    };
    match column.ty {
        FieldType::Date => match raw.parse::<civil::Date>() {
            Ok(date) => Cell::date(raw, &display.render_date(date)),
            Err(_) => Cell::text(raw.clone()),
        },
        FieldType::DateTime => match timestamp(raw) {
            Some(at) => {
                let rendered = display.render(at, Format::DateTime, Precision::Minute);
                Cell::instant(Instant::new(
                    &rendered.iso,
                    &rendered.text,
                    &rendered.company,
                    &rendered.utc,
                    rendered.mode.as_str(),
                ))
            }
            None => Cell::text(raw.clone()),
        },
        _ => Cell::text(raw.clone()),
    }
}

/// PostgreSQL's JSON text of a `timestamptz` carries an offset; a `timestamp`
/// has none and is UTC by contract.
fn timestamp(raw: &str) -> Option<Timestamp> {
    raw.parse::<Timestamp>().ok().or_else(|| {
        raw.parse::<civil::DateTime>()
            .ok()
            .and_then(|at| jiff::tz::TimeZone::UTC.to_timestamp(at).ok())
    })
}

/// The change under the delta lens: a rollup's value now against the same
/// aggregate as of the date (nothing before it counts from zero).
fn delta(value: &Value, column: &Column, lens: Lens, extras: Extras<'_>) -> Option<f64> {
    if lens != Lens::Delta || column.kind != ColumnKind::Rollup {
        return None;
    }
    let now = number(value, column.ty)?;
    Some(
        match extras.before.and_then(|before| number(before, column.ty)) {
            Some(before) => now - before,
            None => now,
        },
    )
}

/// A change with its sign: whole changes as integers, others to two places,
/// a minus sign (U+2212) below zero and ± at zero.
fn signed(delta: f64) -> String {
    let rounded = (delta * 100.0).round() / 100.0;
    let sign = if rounded < 0.0 {
        "−"
    } else if rounded > 0.0 {
        "+"
    } else {
        "±"
    };
    if rounded == rounded.trunc() {
        format!("{sign}{}", rounded.abs() as i64)
    } else {
        format!("{sign}{:.2}", rounded.abs())
    }
}

fn two_places(value: f64) -> String {
    format!("{:.2}", (value * 100.0).round() / 100.0)
}

/// A value on the column's number line: figures as themselves, a date as its
/// day number, a timestamp as Unix seconds.
fn number(value: &Value, ty: FieldType) -> Option<f64> {
    match value {
        Value::Number(number) => number.as_f64(),
        Value::String(raw) if ty == FieldType::Date => {
            let date = raw.parse::<civil::Date>().ok()?;
            let days = civil::date(1970, 1, 1).until(date).ok()?.get_days();
            Some(f64::from(days))
        }
        Value::String(raw) if ty == FieldType::DateTime => {
            timestamp(raw).map(|at| at.as_second() as f64)
        }
        _ => None,
    }
}

fn position(value: &Value, column: &Column, range: Option<&Range>) -> Option<(f64, Scale)> {
    if value.is_null() {
        return None;
    }
    if column.numeric() {
        if let Some(range) = range {
            let v = number(value, column.ty)?;
            let lo = number(&range.min, column.ty)?;
            let hi = number(&range.max, column.ty)?;
            let n = if hi == lo {
                0.5
            } else {
                ((v - lo) / (hi - lo)).clamp(0.0, 1.0)
            };
            return Some((n, Scale::Sequential));
        }
    }
    if let Value::Bool(flag) = value {
        return Some((if *flag { 1.0 } else { 0.0 }, Scale::Binary));
    }
    Some((
        flex_table::category(&category_text(value, column)),
        Scale::Categorical,
    ))
}

/// The text a categorical colour is hashed from: Elixir's `to_string` of the
/// value held, so the same word wears the same band.
fn category_text(value: &Value, column: &Column) -> String {
    match (value, column.ty) {
        (Value::String(raw), FieldType::DateTime) => match raw.parse::<Timestamp>() {
            Ok(at) => at.strftime("%Y-%m-%d %H:%M:%SZ").to_string(),
            Err(_) => raw.replacen('T', " ", 1),
        },
        (Value::String(raw), _) => raw.clone(),
        (other, _) => other.to_string(),
    }
}

fn band(n: f64, scale: Scale, value: &Value) -> u8 {
    match scale {
        Scale::Binary => {
            if value.as_bool() == Some(true) {
                BANDS - 1
            } else {
                0
            }
        }
        Scale::Categorical => (n * 7.0).round() as u8,
        Scale::Sequential => ((n * f64::from(BANDS)).trunc() as u8).min(BANDS - 1),
    }
}
