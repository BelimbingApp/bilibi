//! The flexible table: a real HTML table whose columns a person adds, removes,
//! reorders and reads through a lens, in normal or compact rows. Counterpart of
//! Bilimbi `apps/base/ui/lib/ui/flex_table.ex` (data shapes and zoom steps) and
//! `components/flex_table.ex` (markup) at the pinned source; the markup is
//! `templates/flex_table.html` and the browser half `assets/flex_table.js`.
//!
//! The component is presentation only: it knows nothing about catalogs, links
//! or queries. A host hands it [`FlexColumn`]s and prepared [`FlexRow`]s and
//! handles the one event it raises, `event` (default `"grid"`), whose payload
//! carries an `op`: `add`, `add_typed`, `suggest`, `remove`, `move`, `lens`,
//! `since`, `zoom`, `zoom_preset`, `reset`, `sort`, `expand`, `collapse`.
//! `ListColumns` (built-in columns) is one such host; the Grid catalog is
//! another (G-series).
//!
//! Nothing stands above the table but the notch on its frame's top-left edge.
//! It opens table customization: the chips (drag to reorder, remove, choose a
//! lens), the add-a-column box, the zoom, the reset and, while a change-since
//! lens is worn, its date. The notch's `aria-expanded` is the one record of
//! open and is browser-owned (`data-live-ignore`), so a reply that redraws the
//! chips leaves the bar as the person had it.
//!
//! The zoom is the height of a row in pixels and takes only [`ZOOM_STEPS`]:
//! every one draws a visibly different table. Each press is its own intent and
//! the transport queues them, so three presses on "Taller rows" are three steps.
use crate::list_state::SortDir;
use crate::lists::{Column, SortView, TableEmpty};
use crate::Html;
use askama::Template;
use std::collections::{BTreeMap, BTreeSet};

/// The row heights a table takes, shortest first. A compact row holds one line
/// of small text (18 at its shortest); a normal row what a page draws at full
/// size (32 at its shortest). Nothing between 26 and 32 is offered: it would
/// look the same as its neighbour.
pub const ZOOM_STEPS: [u16; 6] = [18, 22, 26, 32, 36, 40];
const COMPACT_STEPS: [u16; 3] = [18, 22, 26];
/// The row height a table opens at: normal rows.
pub const DEFAULT_ZOOM: u16 = 32;

/// Whether rows are drawn compact (one line, small text) or normal.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum RowMode {
    Compact,
    #[default]
    Normal,
}

impl RowMode {
    /// The two named heights, in the order the bar offers them.
    pub const PRESETS: [RowMode; 2] = [RowMode::Compact, RowMode::Normal];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compact => "compact",
            Self::Normal => "normal",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Compact => "Compact",
            Self::Normal => "Normal",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Compact => "rows-compact",
            Self::Normal => "rows-normal",
        }
    }

    /// The row height the preset sets.
    pub fn zoom(self) -> u16 {
        match self {
            Self::Compact => 18,
            Self::Normal => DEFAULT_ZOOM,
        }
    }

    /// A `zoom_preset` op's `preset`; anything else is no preset.
    pub fn parse(preset: &str) -> Option<Self> {
        Self::PRESETS
            .into_iter()
            .find(|mode| mode.as_str() == preset)
    }
}

/// The offered row height nearest to `value`.
pub fn normalize_zoom(value: i64) -> u16 {
    *ZOOM_STEPS
        .iter()
        .min_by_key(|step| (i128::from(**step) - i128::from(value)).abs())
        .expect("zoom steps")
}

/// The offered row height nearest to the text of an integer; anything else is
/// the default.
pub fn parse_zoom(text: &str) -> u16 {
    text.parse::<i64>()
        .map(normalize_zoom)
        .unwrap_or(DEFAULT_ZOOM)
}

/// One step taller or shorter, staying on the last step at either end.
pub fn step_zoom(zoom: u16, taller: bool) -> u16 {
    let index = ZOOM_STEPS
        .iter()
        .position(|step| *step == normalize_zoom(zoom.into()))
        .expect("zoom step");
    ZOOM_STEPS[if taller {
        (index + 1).min(ZOOM_STEPS.len() - 1)
    } else {
        index.saturating_sub(1)
    }]
}

/// Whether a row this tall is drawn compact or normal.
pub fn row_mode(zoom: u16) -> RowMode {
    if COMPACT_STEPS.contains(&normalize_zoom(zoom.into())) {
        RowMode::Compact
    } else {
        RowMode::Normal
    }
}

/// The class that gives a cell its row height; every step has one, written out
/// so Tailwind sees it.
pub fn row_class(zoom: u16) -> &'static str {
    match normalize_zoom(zoom.into()) {
        18 => "h-4.5",
        22 => "h-5.5",
        26 => "h-6.5",
        32 => "h-8",
        36 => "h-9",
        _ => "h-10",
    }
}

/// What a column holds, for alignment of figures and dates.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColumnType {
    #[default]
    String,
    Integer,
    Float,
    Decimal,
    Boolean,
    Date,
    Datetime,
    Enum,
}

impl ColumnType {
    /// Figures and dates line up in tabular numerals.
    pub fn tabular(self) -> bool {
        matches!(
            self,
            Self::Integer | Self::Float | Self::Decimal | Self::Date | Self::Datetime
        )
    }
}

/// How a column's cells are drawn: from their [`Cell`], as a rollup that
/// expands in place, or by the host's own fragment ([`FlexRow::slot`]).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ColumnKind {
    #[default]
    Field,
    Rollup,
    Slot,
}

/// The visual encoding of a column. A lens never changes a value's type: a
/// timestamp stays typed through every lens.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Lens {
    #[default]
    Value,
    Bar,
    Band,
    Trend,
    Delta,
}

impl Lens {
    pub const ALL: [Lens; 5] = [Lens::Value, Lens::Bar, Lens::Band, Lens::Trend, Lens::Delta];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Bar => "bar",
            Self::Band => "band",
            Self::Trend => "trend",
            Self::Delta => "delta",
        }
    }

    /// The word a lens is called.
    pub fn label(self) -> &'static str {
        match self {
            Self::Value => "Value",
            Self::Bar => "Bar",
            Self::Band => "Colour band",
            Self::Trend => "Trend",
            Self::Delta => "Change since a date",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Value => "bars-3-bottom-left",
            Self::Bar => "chart-bar",
            Self::Band => "paint-brush",
            Self::Trend => "arrow-trending-up",
            Self::Delta => "calendar-days",
        }
    }

    /// A `lens` op's `lens`; anything else is no lens.
    pub fn parse(lens: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|candidate| candidate.as_str() == lens)
    }
}

/// How a band position is painted (`app.css` `.flex-table [data-scale]`).
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Scale {
    Sequential,
    Categorical,
    Binary,
}

impl Scale {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Sequential => "sequential",
            Self::Categorical => "categorical",
            Self::Binary => "binary",
        }
    }
}

/// Deterministic categorical position in 0..=1 over eight slots: Bilimbi's
/// `:erlang.phash2(text, 8) / 7`, so each category paints the source's band.
pub fn category(text: &str) -> f64 {
    f64::from(phash2(text.as_bytes()) % 8) / 7.0
}

/// `erlang:phash2/1` of a binary: Bob Jenkins' lookup2 block hash seeded with
/// erts' `HCONST_13`.
fn phash2(bytes: &[u8]) -> u32 {
    const GOLDEN: u32 = 0x9e37_79b9;
    fn mix([mut a, mut b, mut c]: [u32; 3]) -> [u32; 3] {
        a = a.wrapping_sub(b).wrapping_sub(c) ^ (c >> 13);
        b = b.wrapping_sub(c).wrapping_sub(a) ^ (a << 8);
        c = c.wrapping_sub(a).wrapping_sub(b) ^ (b >> 13);
        a = a.wrapping_sub(b).wrapping_sub(c) ^ (c >> 12);
        b = b.wrapping_sub(c).wrapping_sub(a) ^ (a << 16);
        c = c.wrapping_sub(a).wrapping_sub(b) ^ (b >> 5);
        a = a.wrapping_sub(b).wrapping_sub(c) ^ (c >> 3);
        b = b.wrapping_sub(c).wrapping_sub(a) ^ (a << 10);
        c = c.wrapping_sub(a).wrapping_sub(b) ^ (b >> 15);
        [a, b, c]
    }
    // Little-endian words of up to four bytes; a short tail word reads as
    // lookup2's fall-through switch adds it.
    let word = |chunk: &[u8]| {
        chunk
            .iter()
            .enumerate()
            .fold(0u32, |word, (i, byte)| word | u32::from(*byte) << (8 * i))
    };
    let mut state = [GOLDEN, GOLDEN, GOLDEN.wrapping_mul(13)];
    let (blocks, tail) = bytes.as_chunks::<12>();
    for block in blocks {
        for (lane, chunk) in state.iter_mut().zip(block.chunks(4)) {
            *lane = lane.wrapping_add(word(chunk));
        }
        state = mix(state);
    }
    // The tail fills a and b from its first eight bytes; its last three land
    // in c above the low byte, which holds the length.
    state[2] = state[2].wrapping_add(bytes.len() as u32);
    for (index, chunk) in tail.chunks(4).enumerate() {
        let added = if index == 2 {
            word(chunk) << 8
        } else {
            word(chunk)
        };
        state[index] = state[index].wrapping_add(added);
    }
    mix(state)[2]
}

/// One column. `id` is DOM-safe and unique in the table; `spec` is what the
/// host's handler gets back; `label` is the full path (the tooltip) and
/// `short_label` what the heading shows.
#[derive(Clone, Debug, PartialEq)]
pub struct FlexColumn {
    pub id: String,
    pub spec: String,
    pub label: String,
    pub short_label: String,
    pub ty: ColumnType,
    pub kind: ColumnKind,
    pub lens: Lens,
    pub lenses: Vec<Lens>,
    pub sortable: bool,
    /// The owner's list sort key when it differs from the column spec.
    /// Events still carry the spec; ListColumns maps it to this key.
    pub sort_key: Option<String>,
    pub removable: bool,
    pub align_right: bool,
    /// The sort button's id; defaults to `<table>-sort-<id>`.
    pub sort_id: Option<String>,
}

impl FlexColumn {
    /// A sortable, removable string field read through the value lens.
    /// Panics on an id that is not DOM-safe, as a programming error.
    pub fn new(id: &str, spec: &str, label: &str) -> Self {
        assert!(
            !id.is_empty()
                && id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'),
            "a flex column id must be a DOM-safe string"
        );
        Self {
            id: id.into(),
            spec: spec.into(),
            label: label.into(),
            short_label: label.into(),
            ty: ColumnType::String,
            kind: ColumnKind::Field,
            lens: Lens::Value,
            lenses: vec![Lens::Value],
            sortable: true,
            sort_key: None,
            removable: true,
            align_right: false,
            sort_id: None,
        }
    }

    pub fn short(mut self, short_label: &str) -> Self {
        self.short_label = short_label.into();
        self
    }

    pub fn typed(mut self, ty: ColumnType) -> Self {
        self.ty = ty;
        self
    }

    pub fn kind(mut self, kind: ColumnKind) -> Self {
        self.kind = kind;
        self
    }

    /// The lens worn and the lenses offered; the worn one is always offered.
    pub fn lenses(mut self, lens: Lens, lenses: &[Lens]) -> Self {
        self.lens = lens;
        self.lenses = lenses.to_vec();
        if !self.lenses.contains(&lens) {
            self.lenses.insert(0, lens);
        }
        self
    }

    pub fn unsortable(mut self) -> Self {
        self.sortable = false;
        self
    }

    pub fn fixed(mut self) -> Self {
        self.removable = false;
        self
    }

    pub fn right(mut self) -> Self {
        self.align_right = true;
        self
    }

    pub fn sort_id(mut self, sort_id: &str) -> Self {
        self.sort_id = Some(sort_id.into());
        self
    }

    /// Wears `lens` when the column offers it; reports whether it did.
    pub fn wear(&mut self, lens: Lens) -> bool {
        let offered = self.lenses.contains(&lens);
        if offered {
            self.lens = lens;
        }
        offered
    }

    pub fn lens_menu(&self) -> bool {
        self.lenses.len() > 1
    }

    pub fn lens_aria(&self) -> String {
        format!("Lens for {}: {}", self.label, self.lens.label())
    }

    pub fn lens_title(&self) -> String {
        format!("Lens: {}", self.lens.label())
    }

    pub fn band_lens(&self) -> bool {
        self.lens == Lens::Band
    }
}

/// A timestamp kept typed through every lens: the RFC 3339 instant and both
/// server-decidable texts, so the browser follows a clock-mode change by
/// copying a server string. Fill it from `bilibi_datetime::Rendered`; never
/// preformat an instant into [`Cell::text`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Instant {
    pub iso: String,
    /// The text for the display mode (the UTC text in `local` mode).
    pub text: String,
    pub company: String,
    pub utc: String,
    /// `company`, `local` or `utc`.
    pub mode: String,
    /// `datetime`, `date` or `time`.
    pub format: &'static str,
    /// `minute` or `second`.
    pub precision: &'static str,
}

impl Instant {
    pub fn new(iso: &str, text: &str, company: &str, utc: &str, mode: &str) -> Self {
        Self {
            iso: iso.into(),
            text: text.into(),
            company: company.into(),
            utc: utc.into(),
            mode: mode.into(),
            format: "datetime",
            precision: "minute",
        }
    }
}

/// What a cell's value is, beyond its text. A calendar date is zone-free.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum Typed {
    #[default]
    Text,
    Instant(Instant),
    /// `iso` is `YYYY-MM-DD`; `text` its `dd/mm/yyyy` reading.
    Date {
        iso: String,
        text: String,
    },
}

/// One prepared cell: the text to show, the typed value, its 0..1 position in
/// the column's range (`n`), the band that position falls in and how bands
/// are painted, and the monthly values a trend lens draws.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Cell {
    pub text: String,
    pub typed: Typed,
    pub n: Option<f64>,
    pub band: Option<u8>,
    pub scale: Option<Scale>,
    pub series: Vec<f64>,
}

impl Cell {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    pub fn instant(instant: Instant) -> Self {
        Self {
            text: instant.text.clone(),
            typed: Typed::Instant(instant),
            ..Self::default()
        }
    }

    pub fn date(iso: &str, text: &str) -> Self {
        Self {
            text: text.into(),
            typed: Typed::Date {
                iso: iso.into(),
                text: text.into(),
            },
            ..Self::default()
        }
    }

    /// Places the cell in its column's range.
    pub fn at(mut self, n: f64, band: u8, scale: Scale) -> Self {
        self.n = Some(n);
        self.band = Some(band);
        self.scale = Some(scale);
        self
    }

    pub fn series(mut self, series: Vec<f64>) -> Self {
        self.series = series;
        self
    }

    /// The bar width the hook applies, as a percentage.
    pub fn bar_width(&self) -> Option<i64> {
        self.n.map(|n| (n * 100.0).round() as i64)
    }

    /// The points of a sparkline in a 60 by 16 box, for two values or more.
    pub fn sparkline(&self) -> Option<String> {
        sparkline(&self.series)
    }
}

/// The trend lens's small line, drawn by the shared chart module.
pub use crate::chart::sparkline;

/// A value in an expanded rollup's inner table.
#[derive(Clone, Debug, PartialEq)]
pub enum Plain {
    Missing,
    Text(String),
    Bool(bool),
    Instant(Instant),
}

impl Plain {
    /// The word for a value that is not a timestamp.
    pub fn word(&self) -> &str {
        match self {
            Self::Missing => "—",
            Self::Text(text) => text,
            Self::Bool(true) => "Yes",
            Self::Bool(false) => "No",
            Self::Instant(instant) => &instant.text,
        }
    }

    pub fn instant(&self) -> Option<&Instant> {
        match self {
            Self::Instant(instant) => Some(instant),
            _ => None,
        }
    }
}

/// One line a rollup collapsed, keyed by its target's field names.
pub type ExpansionRow = BTreeMap<String, Plain>;

/// One row. `key` identifies it to the host (`expand`/`collapse` carry it);
/// its DOM id is `<table>-row-<key_id(key)>` unless `dom_id` names one.
#[derive(Clone, Debug, Default)]
pub struct FlexRow {
    pub key: String,
    pub dom_id: Option<String>,
    pub cells: BTreeMap<String, Cell>,
    /// The host's own fragment for a column, by column id: a list keeps its
    /// links and badges while gaining walked columns beside them.
    pub slots: BTreeMap<String, Html>,
    pub actions: Html,
    /// Rollups opened in place, by column id, with the lines they collapsed.
    pub expanded: BTreeMap<String, Vec<ExpansionRow>>,
}

impl FlexRow {
    pub fn new(key: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            ..Self::default()
        }
    }

    pub fn cell(mut self, column_id: &str, cell: Cell) -> Self {
        self.cells.insert(column_id.into(), cell);
        self
    }

    pub fn slot(mut self, column_id: &str, html: Html) -> Self {
        self.slots.insert(column_id.into(), html);
        self
    }
}

/// A DOM id fragment for a row key, safe for any key: every character outside
/// `[A-Za-z0-9_-]` becomes `_` and its UTF-8 bytes in upper-case hex.
pub fn key_id(key: &str) -> String {
    let mut id = String::with_capacity(key.len());
    for character in key.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            id.push(character);
        } else {
            id.push('_');
            let mut bytes = [0; 4];
            for byte in character.encode_utf8(&mut bytes).bytes() {
                id.push_str(&format!("{byte:02X}"));
            }
        }
    }
    id
}

/// Rows ticked for a bulk action: a tick per row, one for the page in the
/// heading, and a bar above the table that counts them, offers every row the
/// filters match and carries the page's bulk actions. The selection lives on
/// the server; each tick raises `event` with an `op`: `row` (with its `key`),
/// `page`, `all` or `clear`. No Bilimbi counterpart (issue 133).
#[derive(Clone, Debug, Default)]
pub struct Selection {
    pub event: String,
    /// The keys of the ticked rows, on this page or another.
    pub selected: BTreeSet<String>,
    /// Every row the list's filters match is selected, not only `selected`.
    pub all_matching: bool,
    /// How many rows the filters match.
    pub matching: u64,
    /// What the rows are called, plural and lower case: "contacts".
    pub noun: String,
    /// The page's bulk actions, shown while something is selected.
    pub actions: Html,
}

impl Selection {
    /// How many rows a bulk action would take.
    pub fn count(&self) -> u64 {
        if self.all_matching {
            self.matching
        } else {
            self.selected.len() as u64
        }
    }
}

/// The planner's estimate for the statement that scales bars and bands over
/// the whole table; `heavy` shows the warning above it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cost {
    pub estimate: f64,
    pub heavy: bool,
}

/// The flexible table. Build with [`FlexTable::new`], set the public fields
/// the page needs, and render it like any other typed component.
#[derive(Template, Clone, Debug)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::flex_table(t=self) %}{% endcall %}"#,
    ext = "html"
)]
pub struct FlexTable<'a> {
    pub id: &'a str,
    pub columns: Vec<FlexColumn>,
    pub rows: Vec<FlexRow>,
    /// The height of a row, one of [`ZOOM_STEPS`]; it decides the mode.
    pub zoom: u16,
    /// The one event every operation raises, with an `op`.
    pub event: &'a str,
    /// `data-target` of a mounted panel; empty for the page itself.
    pub target: &'a str,
    pub sort_by: Option<&'a str>,
    pub sort_dir: Option<SortDir>,
    /// A list's own sort event; empty sends sorting through `event` with
    /// `op: sort`.
    pub sort_event: &'a str,
    /// `data-target` for `sort_event`; ignored when sorting through `event`.
    pub sort_target: &'a str,
    /// The columns the add-a-column box offers for what was typed.
    pub suggestions: Vec<FlexColumn>,
    pub add_query: &'a str,
    pub cost: Option<Cost>,
    /// The `YYYY-MM-DD` a change-since lens compares against.
    pub since: Option<&'a str>,
    /// Screen-reader caption naming the table.
    pub caption: &'a str,
    /// False inside a `p-0` card, so the card is the one frame around the table.
    pub framed: bool,
    /// Classes for the frame, such as padding a `p-0` card leaves out.
    pub class: &'a str,
    /// Renders the trailing actions column from [`FlexRow::actions`].
    pub actions: bool,
    /// Shown when there are no rows.
    pub empty: Option<TableEmpty>,
    /// Row ticks and the selection bar; `None` draws neither.
    pub selection: Option<Selection>,
}

impl<'a> FlexTable<'a> {
    pub fn new(id: &'a str, columns: Vec<FlexColumn>, rows: Vec<FlexRow>) -> Self {
        Self {
            id,
            columns,
            rows,
            zoom: DEFAULT_ZOOM,
            event: "grid",
            target: "",
            sort_by: None,
            sort_dir: None,
            sort_event: "",
            sort_target: "",
            suggestions: Vec::new(),
            add_query: "",
            cost: None,
            since: None,
            caption: "",
            framed: true,
            class: "",
            actions: false,
            empty: None,
            selection: None,
        }
    }

    pub fn sorted(mut self, sort_by: Option<&'a str>, sort_dir: Option<SortDir>) -> Self {
        self.sort_by = sort_by;
        self.sort_dir = sort_dir;
        self
    }

    pub fn mode(&self) -> RowMode {
        row_mode(self.zoom)
    }

    pub fn presets(&self) -> &'static [RowMode] {
        &RowMode::PRESETS
    }

    pub fn compact(&self) -> bool {
        self.mode() == RowMode::Compact
    }

    pub fn zoom(&self) -> u16 {
        normalize_zoom(self.zoom.into())
    }

    pub fn row_class(&self) -> &'static str {
        row_class(self.zoom)
    }

    pub fn shortest(&self) -> bool {
        self.zoom() <= ZOOM_STEPS[0]
    }

    pub fn tallest(&self) -> bool {
        self.zoom() >= ZOOM_STEPS[ZOOM_STEPS.len() - 1]
    }

    /// Whether a change-since lens is worn, which shows its date control.
    pub fn delta(&self) -> bool {
        self.columns.iter().any(|column| column.lens == Lens::Delta)
    }

    pub fn cost_text(&self, cost: &Cost) -> String {
        format!(
            "Heavy query: scaling these bars and colour bands reads the whole table, and the planner estimates a cost of {}. Fewer rollups under a bar or band lens would lighten it.",
            cost.estimate.trunc() as i64
        )
    }

    pub fn sort_view<'c>(&'c self, column: &'c FlexColumn) -> SortView<'c> {
        let own = self.sort_event.is_empty();
        SortView {
            table_id: self.id,
            sort_by: self
                .sort_by
                .filter(|key| *key == column.sort_key.as_deref().unwrap_or(&column.spec))
                .map(|_| column.spec.as_str()),
            sort_dir: self.sort_dir,
            event: if own { self.event } else { self.sort_event },
            target: if own { self.target } else { self.sort_target },
            op: if own { "sort" } else { "" },
        }
    }

    /// The sort button's id: the column's own, or `<table>-sort-<column id>`.
    pub fn sort_id(&self, column: &FlexColumn) -> String {
        column
            .sort_id
            .clone()
            .unwrap_or_else(|| format!("{}-sort-{}", self.id, column.id))
    }

    /// The column as the shared sort heading takes it; `sort_id` is
    /// [`Self::sort_id`].
    pub fn heading<'c>(&self, column: &'c FlexColumn, sort_id: &'c str) -> Column<'c> {
        Column {
            label: &column.short_label,
            sort: &column.spec,
            sort_id,
            align_right: column.align_right,
        }
    }

    pub fn aria_sort(&self, column: &FlexColumn) -> Option<&'static str> {
        if column.sortable {
            self.sort_view(column).aria_sort(&column.spec)
        } else {
            None
        }
    }

    pub fn colspan(&self) -> usize {
        self.columns.len() + usize::from(self.actions) + usize::from(self.selection.is_some())
    }

    /// Whether a row is ticked, by itself or with every matching row.
    pub fn ticked(&self, row: &FlexRow) -> bool {
        self.selection
            .as_ref()
            .is_some_and(|s| s.all_matching || s.selected.contains(&row.key))
    }

    /// The primary cell's text for the row tick's accessible name.
    pub fn selection_label<'r>(&self, row: &'r FlexRow) -> Option<&'r str> {
        let primary = self.columns.first()?;
        row.cells
            .get(&primary.id)
            .map(|cell| cell.text.as_str())
            .filter(|label| !label.is_empty())
    }

    /// The page tick's `aria-checked`:  `true` when every row here is
    /// ticked, `mixed` when some are.
    pub fn page_ticked(&self) -> &'static str {
        let ticked = self.rows.iter().filter(|row| self.ticked(row)).count();
        if ticked > 0 && ticked == self.rows.len() {
            "true"
        } else if ticked > 0 {
            "mixed"
        } else {
            "false"
        }
    }

    /// Whether the bar offers every row the filters match: the whole page
    /// is ticked and more rows match than it shows.
    pub fn offers_all(&self) -> bool {
        self.selection.as_ref().is_some_and(|s| {
            !s.all_matching && self.page_ticked() == "true" && s.matching > s.selected.len() as u64
        })
    }

    pub fn row_dom_id(&self, row: &FlexRow) -> String {
        row.dom_id
            .clone()
            .unwrap_or_else(|| format!("{}-row-{}", self.id, key_id(&row.key)))
    }

    pub fn opened_id(&self, row: &FlexRow, column_id: &str) -> String {
        format!("{}-{column_id}", self.row_dom_id(row))
    }

    pub fn cell_id(&self, row: &FlexRow, column: &FlexColumn) -> String {
        format!("{}-cell-{}-{}", self.id, key_id(&row.key), column.id)
    }

    pub fn expand_id(&self, row: &FlexRow, column: &FlexColumn) -> String {
        format!("{}-expand-{}-{}", self.id, key_id(&row.key), column.id)
    }

    pub fn lens_menu_id(&self, column: &FlexColumn) -> String {
        format!("{}-lens-{}", self.id, column.id)
    }

    /// The `<td>` class of a cell drawn from its value or the host's slot.
    pub fn cell_class(&self, column: &FlexColumn, slot: bool) -> String {
        let mut class = format!("text-ink {} {}", self.row_class(), self.density());
        if column.align_right {
            class.push_str(" text-right");
        }
        if !slot && column.ty.tabular() {
            class.push_str(" tabular-nums");
        }
        class
    }

    pub fn rollup_class(&self) -> String {
        format!(
            "text-ink tabular-nums {} {}",
            self.row_class(),
            self.density()
        )
    }

    /// A compact row is one line however long its text is.
    fn density(&self) -> &'static str {
        if self.compact() {
            "whitespace-nowrap px-1.5 py-0 leading-4.5"
        } else {
            "px-2 py-0.5"
        }
    }

    /// `data-band` of a cell worn through the band lens.
    pub fn band(&self, column: &FlexColumn, cell: &Option<&Cell>) -> Option<u8> {
        cell.filter(|_| column.band_lens())
            .and_then(|cell| cell.band)
    }

    /// `data-scale` of a banded cell.
    pub fn scale(&self, column: &FlexColumn, cell: &Option<&Cell>) -> Option<&'static str> {
        self.band(column, cell)?;
        cell.and_then(|cell| cell.scale).map(Scale::as_str)
    }

    pub fn cell<'r>(&self, row: &'r FlexRow, column: &FlexColumn) -> Option<&'r Cell> {
        row.cells.get(&column.id)
    }

    pub fn slot<'r>(&self, row: &'r FlexRow, column: &FlexColumn) -> Option<&'r Html> {
        row.slots.get(&column.id)
    }

    pub fn is_expanded(&self, row: &FlexRow, column: &FlexColumn) -> bool {
        row.expanded.contains_key(&column.id)
    }

    /// An expansion's headings: its first line's field names, sorted, with
    /// underscores spaced and the first letter capitalized.
    pub fn headings(&self, lines: &[ExpansionRow]) -> Vec<(String, String)> {
        lines
            .first()
            .map(|line| {
                line.keys()
                    .map(|key| {
                        let spaced = key.replace('_', " ").to_lowercase();
                        let mut chars = spaced.chars();
                        let label = chars
                            .next()
                            .map(|first| first.to_uppercase().chain(chars).collect())
                            .unwrap_or_default();
                        (key.clone(), label)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn has_empty(&self) -> bool {
        self.rows.is_empty() && self.empty.is_some()
    }
}
