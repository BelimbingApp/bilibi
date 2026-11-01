//! The specimen host for [`FlexTable`]: a fixed synthetic set of example
//! companies and the component's operations applied to plain state. There is
//! no catalog behind it, so the add box offers the three columns the set holds
//! back, and a rollup expands to the lines the set carries for that row.
//! Nothing is simulated: every gesture changes what the specimen shows.
//!
//! The component specimen page renders it statically and the `flex-reference`
//! example (apps/web/examples) runs it live. Categorical bands come from
//! [`flex_table::category`] (Erlang's `phash2`), so each region wears the
//! same colour everywhere.
use crate::flex_table::{
    Cell, ColumnKind, ColumnType, Cost, ExpansionRow, FlexColumn, FlexRow, FlexTable, Instant,
    Lens, Plain, RowMode, Scale, DEFAULT_ZOOM,
};
use crate::list_state::SortDir;
use crate::lists::{EmptyState, TableEmpty};
use crate::{flex_table, Html};
use std::collections::BTreeMap;

const ROWS: u32 = 60;
const REGIONS: [&str; 5] = ["North", "South", "East", "West", "Central"];
const STATUSES: [&str; 3] = ["active", "pending", "suspended"];
/// 2026-08-14 12:00:00 UTC, the newest `updated_at` in the set.
const UPDATED_BASE: i64 = 1_786_708_800;

fn column(id: &str, spec: &str, label: &str, short: &str, ty: ColumnType) -> FlexColumn {
    FlexColumn::new(id, spec, label).short(short).typed(ty)
}

const VALUE_BAND: [Lens; 2] = [Lens::Value, Lens::Band];
const VALUE_BAR_BAND: [Lens; 3] = [Lens::Value, Lens::Bar, Lens::Band];

/// The columns the specimen opens with.
pub fn columns() -> Vec<FlexColumn> {
    use ColumnKind::{Field, Rollup, Slot};
    use ColumnType::{Datetime, Decimal, Enum, Integer, String};
    vec![
        column("name", "name", "Name", "Name", String)
            .kind(Slot)
            .lenses(Lens::Value, &VALUE_BAND)
            .fixed(),
        column("code", "code", "Code", "Code", String)
            .kind(Field)
            .lenses(Lens::Value, &VALUE_BAND),
        column("status", "status", "Status", "Status", Enum)
            .kind(Field)
            .lenses(Lens::Band, &VALUE_BAND),
        column("region", "region", "Region", "Region", Enum)
            .kind(Field)
            .lenses(Lens::Value, &VALUE_BAND),
        column(
            "employees_count",
            "employees:count",
            "Employees › Count",
            "Count",
            Integer,
        )
        .kind(Rollup)
        .lenses(Lens::Bar, &VALUE_BAR_BAND)
        .right(),
        column(
            "orders-amount_sum",
            "orders.amount:sum",
            "Orders › Amount (Sum)",
            "Amount (Sum)",
            Decimal,
        )
        .kind(Rollup)
        .lenses(Lens::Band, &VALUE_BAR_BAND)
        .right(),
        column("updated_at", "updated_at", "Updated", "Updated", Datetime)
            .kind(Field)
            .lenses(Lens::Value, &VALUE_BAND)
            .right(),
    ]
}

/// The columns the add box can still offer.
pub fn extra() -> Vec<FlexColumn> {
    use ColumnKind::{Field, Rollup};
    use ColumnType::{Integer, String};
    vec![
        column(
            "users_count",
            "users:count",
            "Users › Count",
            "Count",
            Integer,
        )
        .kind(Rollup)
        .lenses(Lens::Value, &VALUE_BAR_BAND)
        .right(),
        column(
            "parent-name",
            "parent.name",
            "Parent company › Name",
            "Name",
            String,
        )
        .kind(Field)
        .lenses(Lens::Value, &VALUE_BAND),
        column(
            "addresses_count",
            "addresses:count",
            "Addresses › Count",
            "Count",
            Integer,
        )
        .kind(Rollup)
        .lenses(Lens::Value, &VALUE_BAR_BAND)
        .right(),
    ]
}

/// One value of the set.
#[derive(Clone, Debug, PartialEq)]
enum Value {
    Missing,
    Text(String),
    Int(i64),
    Float(f64),
    /// Seconds since the Unix epoch, UTC.
    At(i64),
}

impl Value {
    fn number(&self) -> Option<f64> {
        match self {
            Self::Int(n) | Self::At(n) => Some(*n as f64),
            Self::Float(n) => Some(*n),
            _ => None,
        }
    }

    fn text(&self) -> String {
        match self {
            Self::Missing => String::new(),
            Self::Text(text) => text.clone(),
            Self::Int(n) => n.to_string(),
            Self::Float(n) => format!("{n:.2}"),
            Self::At(seconds) => iso_date(seconds.div_euclid(86_400)),
        }
    }
}

fn values(id: u32) -> BTreeMap<&'static str, Value> {
    let id64 = i64::from(id);
    let at = UPDATED_BASE - (id64 * 13 % 400) * 86_400;
    BTreeMap::from([
        ("name", Value::Text(format!("Example Company {id}"))),
        ("code", Value::Text(format!("example-{id}"))),
        (
            "status",
            Value::Text(STATUSES[(id * 7 % 3) as usize].into()),
        ),
        ("region", Value::Text(REGIONS[(id * 3 % 5) as usize].into())),
        ("employees_count", Value::Int(id64 * 37 % 250)),
        (
            "orders-amount_sum",
            Value::Float((id64 * 7919 % 100_000) as f64 / 100.0),
        ),
        ("updated_at", Value::At(at)),
        ("users_count", Value::Int(id64 * 11 % 40)),
        (
            "parent-name",
            if id.is_multiple_of(4) {
                Value::Text(format!("Example Company {}", id / 4))
            } else {
                Value::Missing
            },
        ),
        ("addresses_count", Value::Int(id64 % 5)),
    ])
}

/// How the host draws an instant: the reader's display context in the app,
/// a fixed browser-local context on the static specimen.
pub type InstantRenderer<'a> = &'a dyn Fn(i64) -> Instant;

/// The local-mode rendering the specimen gets without a display
/// context: both texts are the UTC reading.
pub fn utc_instant(seconds: i64) -> Instant {
    let days = seconds.div_euclid(86_400);
    let clock = seconds.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    let text = format!(
        "{day:02}/{month:02}/{year}, {:02}:{:02} UTC",
        clock / 3600,
        clock % 3600 / 60
    );
    let iso = format!(
        "{}T{:02}:{:02}:{:02}Z",
        iso_date(days),
        clock / 3600,
        clock % 3600 / 60,
        clock % 60
    );
    Instant::new(&iso, &text, &text, &text, "local")
}

/// The proleptic Gregorian date of a day count from 1970-01-01.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}

fn iso_date(days: i64) -> String {
    let (year, month, day) = civil(days);
    format!("{year:04}-{month:02}-{day:02}")
}

/// The specimen's state: what a person arranged, sorted and opened.
#[derive(Clone, Debug)]
pub struct FlexSample {
    pub columns: Vec<FlexColumn>,
    pub sort_by: String,
    pub sort_dir: SortDir,
    pub zoom: u16,
    pub suggestions: Vec<FlexColumn>,
    pub add_query: String,
    /// Opened rollups: row id → column id → lines.
    pub expanded: BTreeMap<u32, BTreeMap<String, Vec<ExpansionRow>>>,
}

impl Default for FlexSample {
    fn default() -> Self {
        Self {
            columns: columns(),
            sort_by: "region".into(),
            sort_dir: SortDir::Asc,
            zoom: DEFAULT_ZOOM,
            suggestions: Vec::new(),
            add_query: String::new(),
            expanded: BTreeMap::new(),
        }
    }
}

impl FlexSample {
    pub fn mode(&self) -> RowMode {
        flex_table::row_mode(self.zoom)
    }

    /// The first `limit` rows of the set in the specimen's order, with cells
    /// for its columns, the name slot and the preview action.
    pub fn rows(&self, limit: usize, instant: InstantRenderer) -> Vec<FlexRow> {
        let all: Vec<FlexColumn> = columns().into_iter().chain(extra()).collect();
        let set: Vec<(u32, BTreeMap<&str, Value>)> =
            (1..=ROWS).map(|id| (id, values(id))).collect();
        let stats: BTreeMap<String, (f64, f64)> = all
            .iter()
            .filter_map(|column| {
                let numbers = set
                    .iter()
                    .filter_map(|(_, row)| row[column.id.as_str()].number());
                let (lo, hi) = numbers.fold(None, |range: Option<(f64, f64)>, n| {
                    Some(range.map_or((n, n), |(lo, hi)| (lo.min(n), hi.max(n))))
                })?;
                Some((column.id.clone(), (lo, hi)))
            })
            .collect();
        let sort_column = all
            .iter()
            .find(|column| column.spec == self.sort_by)
            .unwrap_or(&all[0]);
        let mut ordered: Vec<&(u32, BTreeMap<&str, Value>)> = set.iter().collect();
        ordered.sort_by(|a, b| order(&a.1[sort_column.id.as_str()], &b.1[sort_column.id.as_str()]));
        if self.sort_dir == SortDir::Desc {
            ordered.reverse();
        }
        ordered
            .into_iter()
            .take(limit)
            .map(|(id, row)| {
                let name = row["name"].text();
                let mut flex = FlexRow::new(id.to_string());
                for column in &self.columns {
                    let value = &row[column.id.as_str()];
                    flex.cells.insert(
                        column.id.clone(),
                        cell(value, column, stats.get(&column.id), instant),
                    );
                }
                flex.slots.insert(
                    "name".into(),
                    Html::raw(format!(
                        "<span class=\"font-medium text-ink-strong\">{}</span>",
                        Html::text(&name)
                    )),
                );
                flex.actions = preview(*id, &name);
                if let Some(opened) = self.expanded.get(id) {
                    flex.expanded = opened.clone();
                }
                flex
            })
            .collect()
    }

    /// The table for this state: its columns, `limit` rows, sort, zoom and
    /// add box. The caller sets the frame, caption, actions and cost.
    pub fn table<'a>(
        &'a self,
        id: &'a str,
        event: &'a str,
        limit: usize,
        instant: InstantRenderer,
    ) -> FlexTable<'a> {
        let mut table = FlexTable::new(id, self.columns.clone(), self.rows(limit, instant))
            .sorted(Some(&self.sort_by), Some(self.sort_dir));
        table.event = event;
        table.zoom = self.zoom;
        table.suggestions = self.suggestions.clone();
        table.add_query = &self.add_query;
        table.empty = Some(TableEmpty::State(EmptyState::missing(
            "No example companies",
            "The example set is empty.",
        )));
        table
    }

    /// Design Library framing: twelve rows with the preview action,
    /// a caption and the heavy-query notice.
    pub fn framed<'a>(
        &'a self,
        id: &'a str,
        event: &'a str,
        instant: InstantRenderer,
    ) -> FlexTable<'a> {
        let mut table = self.table(id, event, 12, instant);
        table.actions = true;
        table.caption = "Example companies as a flexible table";
        table.cost = Some(Cost {
            estimate: 32_000.0,
            heavy: true,
        });
        table
    }

    /// The plainest shape: four rows, no actions or caption, unframed inside a
    /// padless card that is its one frame.
    pub fn plain<'a>(
        &'a self,
        id: &'a str,
        event: &'a str,
        instant: InstantRenderer,
    ) -> FlexTable<'a> {
        let mut table = self.table(id, event, 4, instant);
        table.framed = false;
        for row in &mut table.rows {
            row.actions = Html::EMPTY;
            row.slots.clear();
        }
        table
    }

    /// Applies one component op from the event payload's string values.
    pub fn apply(&mut self, get: &dyn Fn(&str) -> Option<String>) {
        let spec = get("spec").unwrap_or_default();
        match get("op").as_deref() {
            Some("remove") => {
                if self.columns.iter().any(|c| c.spec == spec && c.removable) {
                    self.columns.retain(|c| !(c.spec == spec && c.removable));
                    if self.sort_by == spec {
                        self.sort_by = "name".into();
                    }
                }
            }
            Some("add") => self.add(&spec),
            Some("add_typed") => {
                if let Some(first) = self.suggest(&get("add").unwrap_or_default()).first() {
                    let spec = first.spec.clone();
                    self.add(&spec);
                }
            }
            Some("suggest") => {
                let text: String = get("add").unwrap_or_default().chars().take(120).collect();
                self.suggestions = if text.is_empty() {
                    Vec::new()
                } else {
                    self.suggest(&text)
                };
                self.add_query = text;
            }
            Some("move") => {
                let before = get("before");
                if let Some(index) = self.columns.iter().position(|c| c.spec == spec) {
                    let moving = self.columns.remove(index);
                    let at = before
                        .and_then(|before| self.columns.iter().position(|c| c.spec == before))
                        .unwrap_or(self.columns.len());
                    self.columns.insert(at, moving);
                }
            }
            Some("lens") => {
                if let Some(lens) = get("lens").as_deref().and_then(Lens::parse) {
                    for column in self.columns.iter_mut().filter(|c| c.spec == spec) {
                        column.wear(lens);
                    }
                }
            }
            Some("sort") => {
                let sort = get("sort").unwrap_or_default();
                if self.sort_by == sort && self.sort_dir == SortDir::Asc {
                    self.sort_dir = SortDir::Desc;
                } else if self.columns.iter().any(|c| c.spec == sort && c.sortable) {
                    self.sort_by = sort;
                    self.sort_dir = SortDir::Asc;
                }
            }
            Some("zoom") => match get("dir").as_deref() {
                Some("in") => self.zoom = flex_table::step_zoom(self.zoom, true),
                Some("out") => self.zoom = flex_table::step_zoom(self.zoom, false),
                _ => {}
            },
            Some("zoom_preset") => {
                if let Some(mode) = get("preset").as_deref().and_then(RowMode::parse) {
                    self.zoom = mode.zoom();
                }
            }
            Some("reset") => *self = Self::default(),
            Some("expand") => {
                let key = get("key").and_then(|key| key.parse::<u32>().ok());
                let rollup = self
                    .columns
                    .iter()
                    .find(|c| c.spec == spec && c.kind == ColumnKind::Rollup);
                if let (Some(id), Some(column)) = (key.filter(|id| (1..=ROWS).contains(id)), rollup)
                {
                    let column_id = column.id.clone();
                    let opened = lines(id, &column_id);
                    self.expanded
                        .entry(id)
                        .or_default()
                        .insert(column_id, opened);
                }
            }
            Some("collapse") => {
                let key = get("key").and_then(|key| key.parse::<u32>().ok());
                let column = self.columns.iter().find(|c| c.spec == spec);
                if let (Some(id), Some(column)) = (key, column) {
                    if let Some(opened) = self.expanded.get_mut(&id) {
                        opened.remove(&column.id);
                    }
                }
            }
            _ => {}
        }
    }

    fn add(&mut self, spec: &str) {
        if self.columns.iter().any(|c| c.spec == spec) {
            return;
        }
        if let Some(column) = extra().into_iter().find(|c| c.spec == spec) {
            self.columns.push(column);
            self.suggestions.clear();
            self.add_query.clear();
        }
    }

    fn suggest(&self, text: &str) -> Vec<FlexColumn> {
        let down = text.to_lowercase();
        extra()
            .into_iter()
            .filter(|c| !self.columns.iter().any(|shown| shown.spec == c.spec))
            .filter(|c| {
                down.is_empty()
                    || format!("{} {}", c.label, c.spec)
                        .to_lowercase()
                        .contains(&down)
            })
            .collect()
    }
}

/// Elixir term order for the set's values: numbers before text, a missing
/// value as the empty text; instants by their ISO reading.
fn order(a: &Value, b: &Value) -> std::cmp::Ordering {
    let key = |value: &Value| match value {
        Value::Int(n) => (0, *n as f64, String::new()),
        Value::Float(n) => (0, *n, String::new()),
        Value::At(n) => (1, 0.0, format!("{n:020}")),
        Value::Missing => (1, 0.0, String::new()),
        Value::Text(text) => (1, 0.0, text.clone()),
    };
    let (a, b) = (key(a), key(b));
    a.0.cmp(&b.0)
        .then(a.1.total_cmp(&b.1))
        .then_with(|| a.2.cmp(&b.2))
}

fn cell(
    value: &Value,
    column: &FlexColumn,
    stats: Option<&(f64, f64)>,
    instant: InstantRenderer,
) -> Cell {
    if *value == Value::Missing {
        return Cell::text("");
    }
    let base = match value {
        Value::At(seconds) => Cell::instant(instant(*seconds)),
        other => Cell::text(other.text()),
    };
    let (n, scale) = match (column.ty.tabular(), stats, value.number()) {
        (true, Some((lo, hi)), Some(v)) => {
            let span = hi - lo;
            (
                if span == 0.0 { 0.5 } else { (v - lo) / span },
                Scale::Sequential,
            )
        }
        _ => (flex_table::category(&value.text()), Scale::Categorical),
    };
    let band = if scale == Scale::Sequential {
        ((n * 5.0).trunc() as u8).min(4)
    } else {
        (n * 7.0).round() as u8
    };
    let mut cell = base.at(n, band, scale);
    cell.text = match value {
        Value::At(_) => value.text(),
        _ => cell.text,
    };
    cell
}

fn lines(id: u32, column_id: &str) -> Vec<ExpansionRow> {
    if column_id == "employees_count" {
        (1..=(id * 37 % 250).min(5))
            .map(|n| {
                ExpansionRow::from([
                    ("name".into(), Plain::Text(format!("Person {id}-{n}"))),
                    (
                        "status".into(),
                        Plain::Text(["active", "probation"][(n % 2) as usize].into()),
                    ),
                ])
            })
            .collect()
    } else {
        (1..=(id % 4 + 1).min(4))
            .map(|n| {
                ExpansionRow::from([
                    ("reference".into(), Plain::Text(format!("ORD-{id}-{n}"))),
                    (
                        "amount".into(),
                        Plain::Text((id * n * 977 % 20_000).to_string()),
                    ),
                ])
            })
            .collect()
    }
}

fn preview(id: u32, name: &str) -> Html {
    let label = format!("Preview {name}");
    let dom_id = format!("sample-flex-preview-{id}");
    let value = id.to_string();
    let button = crate::IconButton {
        id: &dom_id,
        event: "preview-company",
        name: "id",
        value: &value,
        ..crate::IconButton::new("view", &label)
    };
    Html::render(&button).unwrap_or_default()
}

/// The specimen page's flexible tables, rendered: the framed and plain
/// Design Library samples, and the framed sample in compact rows with two
/// rollups open and the customization bar's suggestions showing, and the
/// plain sample with every row of its page ticked for a bulk action, and
/// with every matching row selected but one unticked.
pub struct FlexSpecimens {
    pub framed: Html,
    pub plain: Html,
    pub compact: Html,
    pub selected: Html,
    pub excepted: Html,
}

impl FlexSpecimens {
    pub fn new() -> askama::Result<Self> {
        let sample = FlexSample::default();
        let mut compact = FlexSample::default();
        for op in [
            [("op", "zoom_preset"), ("preset", "compact")],
            [("op", "expand"), ("spec", "employees:count")],
            [("op", "expand"), ("spec", "orders.amount:sum")],
            [("op", "suggest"), ("add", "count")],
        ] {
            let mut values: BTreeMap<&str, &str> = op.into_iter().collect();
            values.entry("key").or_insert("3");
            compact.apply(&|key| values.get(key).map(|value| (*value).to_owned()));
        }
        let mut selected =
            sample.plain("sample-flex-selected", "sample-flex-selected", &utc_instant);
        selected.selection = Some(crate::Selection {
            event: "sample-flex-select".into(),
            selected: selected.rows.iter().map(|row| row.key.clone()).collect(),
            all_matching: false,
            excluded: Default::default(),
            matching: 240,
            noun: "companies".into(),
            actions: Html::render(&crate::Button {
                id: "sample-flex-selected-export",
                body: Html::text("Export CSV"),
                ..crate::Button::default()
            })?,
        });
        let mut excepted =
            sample.plain("sample-flex-excepted", "sample-flex-excepted", &utc_instant);
        let first = excepted.rows.first().map(|row| row.key.clone());
        excepted.selection = Some(crate::Selection {
            event: "sample-flex-select".into(),
            all_matching: true,
            excluded: first.into_iter().collect(),
            matching: 240,
            noun: "companies".into(),
            ..crate::Selection::default()
        });
        Ok(Self {
            selected: Html::render(&selected)?,
            excepted: Html::render(&excepted)?,
            framed: Html::render(&sample.framed("sample-flex-table", "sample-flex", &utc_instant))?,
            plain: Html::render(&sample.plain(
                "sample-flex-plain",
                "sample-flex-plain",
                &utc_instant,
            ))?,
            compact: Html::render(&compact.framed(
                "sample-flex-compact",
                "sample-flex-compact",
                &utc_instant,
            ))?,
        })
    }
}
