//! The shared calendar (BelimbingProject/bilibi#142): day, week, month and
//! shared (one column per person or resource) views of records with a start
//! and an end. A Domain passes its records for the visible range and gets the
//! page fragment back; it owns no calendar markup, CSS or JavaScript.
//!
//! - **The component never writes.** A drop, a resize and the Move dialog all
//!   end in one [`CalendarMove`], decoded by [`CalendarMove::from_event`]; the
//!   owner passes it, with the record's `version`, to its own operation, which
//!   rechecks the capability and the version inside its mutation boundary and
//!   answers a refusal with a message ([`Calendar::notice`]). A refused or
//!   stale move changes nothing on the page: the browser never moves a record
//!   itself, so the record is still where the server last drew it.
//! - **Time.** Items are instants (UTC) or whole dates; the component draws
//!   them in `zone`, the viewer's display zone, and says which zone. A drop
//!   names a wall-clock date and minute in that zone, and [`CalendarMove`]
//!   resolves it with the zone's rules, so a move across a daylight-saving
//!   change keeps its wall-clock time.
//! - **Bounded.** The owner reads only [`CalendarState::range`]; past
//!   [`MAX_ITEMS`] the calendar says so instead of drawing.
//! - **No inline style.** Positions are `row-start-*` and `col-start-*`
//!   classes on a fixed grid of five-minute rows (`app.css` lists them with
//!   `@source inline`), colours are the `cat-1..8` data roles.
use crate::{
    Alert, Button, ButtonVariant, DayKind, Field, Form, Html, InputKind, Modal, ModalWidth,
    StatusKind, WEEKDAYS,
};
use askama::Template;
use jiff::{
    civil::{Date, DateTime, Time},
    tz::TimeZone,
    SignedDuration, Timestamp, ToSpan,
};
use std::collections::HashMap;

/// Items the calendar draws at most; past this it asks for a narrower filter.
pub const MAX_ITEMS: usize = 500;
/// Lanes (people or resources) a shared view shows at most.
pub const MAX_LANES: usize = 20;
/// The page event a drop or resize raises: `id`, `version`, `date`, `minute`,
/// `length` (see [`CalendarMove::from_event`]).
pub const MOVE: &str = "calendar_move";
/// The Move dialog's form event.
pub const MOVE_SUBMIT: &str = "calendar_move_submit";
/// A "Move…" action: `id` names the item whose dialog the owner opens.
pub const MOVE_OPEN: &str = "calendar_move_open";
/// The dialog's Cancel and Escape.
pub const MOVE_CANCEL: &str = "calendar_move_cancel";
/// The names of the events above, for an owner's `match`.
pub const EVENTS: [&str; 4] = [MOVE, MOVE_SUBMIT, MOVE_OPEN, MOVE_CANCEL];

/// The years offered, as the work calendar's pages do.
const YEARS: std::ops::RangeInclusive<i16> = 1900..=2999;
/// Minutes in one grid row.
const ROW_MINUTES: i64 = 5;
const ROWS: i64 = 24 * 60 / ROW_MINUTES;
/// Items side by side in one column at most; more share the last.
const MAX_SIDE_BY_SIDE: usize = 6;
/// Items a month cell lists before "+N more".
const CELL_ITEMS: usize = 3;
const MOVE_FORM: &str = "move";
const LONGEST_MOVE_DAYS: i64 = 366;

/// What the calendar shows at once.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CalendarView {
    Day,
    #[default]
    Week,
    Month,
    /// One day, a column for each lane.
    Shared,
}

impl CalendarView {
    pub const ALL: [CalendarView; 4] = [Self::Day, Self::Week, Self::Month, Self::Shared];

    pub fn name(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Shared => "shared",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Day => "Day",
            Self::Week => "Week",
            Self::Month => "Month",
            Self::Shared => "Shared",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|view| view.name() == name)
    }
}

/// One person or resource a shared view shows beside the others.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarLane {
    pub key: String,
    pub label: String,
}

impl CalendarLane {
    pub fn new(key: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            label: label.into(),
        }
    }
}

/// The part of the calendar that lives in the address, so a reload and a
/// shared link reopen the same view: `view`, `date` and, for a shared view,
/// `lanes` (comma-separated keys).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarState {
    pub view: CalendarView,
    /// Any day of the range shown.
    pub date: Date,
    /// The lanes the viewer chose, in the owner's order; empty is every lane
    /// the owner offers (up to [`MAX_LANES`]).
    pub lanes: Vec<String>,
}

impl CalendarState {
    /// Reads `view`, `date` and `lanes`. Anything unusable falls back to the
    /// week of `today`; a lane the owner does not offer is dropped.
    pub fn parse(query: &HashMap<String, String>, today: Date, offered: &[CalendarLane]) -> Self {
        let view = query
            .get("view")
            .and_then(|view| CalendarView::parse(view))
            .unwrap_or_default();
        let date = query
            .get("date")
            .and_then(|date| date.parse::<Date>().ok())
            .filter(|date| YEARS.contains(&date.year()))
            .unwrap_or(today);
        let wanted: Vec<&str> = query
            .get("lanes")
            .map(|lanes| lanes.split(',').collect())
            .unwrap_or_default();
        let lanes = offered
            .iter()
            .filter(|lane| wanted.contains(&lane.key.as_str()))
            .map(|lane| lane.key.clone())
            .take(MAX_LANES)
            .collect();
        Self { view, date, lanes }
    }

    /// The canonical query for [`LiveBody::query`](crate::page::LiveBody).
    pub fn to_params(&self) -> HashMap<String, String> {
        let mut params = HashMap::from([
            ("view".to_owned(), self.view.name().to_owned()),
            ("date".to_owned(), self.date.to_string()),
        ]);
        if !self.lanes.is_empty() {
            params.insert("lanes".to_owned(), self.lanes.join(","));
        }
        params
    }

    /// The lanes a shared view draws: the viewer's choice, or the owner's
    /// first [`MAX_LANES`] when none is chosen. Fetch items for exactly these.
    pub fn shown_lanes<'a>(&self, offered: &'a [CalendarLane]) -> Vec<&'a CalendarLane> {
        offered
            .iter()
            .filter(|lane| self.lanes.is_empty() || self.lanes.contains(&lane.key))
            .take(MAX_LANES)
            .collect()
    }

    /// Every date the view draws: the day, its Monday-first week, or the
    /// whole weeks that hold its month.
    pub fn dates(&self) -> Vec<Date> {
        let (first, count) = match self.view {
            CalendarView::Day | CalendarView::Shared => (self.date, 1),
            CalendarView::Week => (monday(self.date), 7),
            CalendarView::Month => {
                let first = monday(self.date.first_of_month());
                let last = self.date.last_of_month();
                let days = (last.since(first).map(|span| span.get_days()).unwrap_or(34)) + 1;
                (first, ((days + 6) / 7 * 7).min(42))
            }
        };
        (0..i64::from(count))
            .filter_map(|offset| first.checked_add(offset.days()).ok())
            .collect()
    }

    /// The half-open UTC range the owner reads, in `zone`: from the start of
    /// the first date shown to the start of the day after the last. `None`
    /// for a zone that is not an IANA name.
    pub fn range(&self, zone: &str) -> Option<(Timestamp, Timestamp)> {
        let zone = TimeZone::get(zone).ok()?;
        let dates = self.dates();
        let first = *dates.first()?;
        let after = dates.last()?.checked_add(1.day()).ok()?;
        Some((midnight(&zone, first)?, midnight(&zone, after)?))
    }

    /// The date the previous or next range is anchored on.
    pub fn step(&self, forward: bool) -> Date {
        let sign = if forward { 1 } else { -1 };
        let moved = match self.view {
            CalendarView::Day | CalendarView::Shared => self.date.checked_add((sign).days()),
            CalendarView::Week => self.date.checked_add((7 * sign).days()),
            CalendarView::Month => self.date.checked_add((sign).months()),
        };
        moved
            .ok()
            .filter(|date| YEARS.contains(&date.year()))
            .unwrap_or(self.date)
    }
}

fn monday(date: Date) -> Date {
    date.checked_sub(i64::from(date.weekday().to_monday_zero_offset()).days())
        .unwrap_or(date)
}

fn midnight(zone: &TimeZone, date: Date) -> Option<Timestamp> {
    zone.to_ambiguous_zoned(date.at(0, 0, 0, 0))
        .compatible()
        .ok()
        .map(|zoned| zoned.timestamp())
}

/// When an item happens.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CalendarWhen {
    /// Two instants; drawn in the display zone.
    Timed { start: Timestamp, end: Timestamp },
    /// Whole dates, first to last inclusive; no zone.
    AllDay { first: Date, last: Date },
}

/// One record on the calendar. `version` is the owner's own version token for
/// the record; it comes back with every move.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarItem {
    pub id: String,
    pub title: String,
    /// The kind in words ("Meeting"), so the colour is never the only signal.
    pub kind: String,
    pub when: CalendarWhen,
    /// `1..=8`: the `cat-*` data role the owner chose for the kind. Never a
    /// status colour.
    pub tone: u8,
    pub href: String,
    /// The lane (person or resource) the item belongs to.
    pub lane: Option<String>,
    pub movable: bool,
    pub resizable: bool,
    pub version: String,
}

impl CalendarItem {
    pub fn timed(
        id: impl Into<String>,
        title: impl Into<String>,
        start: Timestamp,
        end: Timestamp,
    ) -> Self {
        Self::new(id, title, CalendarWhen::Timed { start, end })
    }

    pub fn all_day(
        id: impl Into<String>,
        title: impl Into<String>,
        first: Date,
        last: Date,
    ) -> Self {
        Self::new(id, title, CalendarWhen::AllDay { first, last })
    }

    fn new(id: impl Into<String>, title: impl Into<String>, when: CalendarWhen) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            kind: String::new(),
            when,
            tone: 1,
            href: String::new(),
            lane: None,
            movable: false,
            resizable: false,
            version: String::new(),
        }
    }

    pub fn kind(mut self, kind: impl Into<String>, tone: u8) -> Self {
        self.kind = kind.into();
        self.tone = tone.clamp(1, 8);
        self
    }

    pub fn href(mut self, href: impl Into<String>) -> Self {
        self.href = href.into();
        self
    }

    pub fn lane(mut self, lane: impl Into<String>) -> Self {
        self.lane = Some(lane.into());
        self
    }

    /// The viewer may move it. Set it only when they hold the update
    /// capability: an item that is not movable has no drag handle and no
    /// Move action.
    pub fn movable(mut self, version: impl Into<String>) -> Self {
        self.movable = true;
        self.version = version.into();
        self
    }

    /// The viewer may also change its length (a timed, movable item).
    pub fn resizable(mut self) -> Self {
        self.resizable = true;
        self
    }
}

/// A month or week cell's mark from the owner (working, rest, off, holiday).
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DayMark {
    pub date: Date,
    pub kind: DayKind,
    /// What the mark says beyond its kind ("Merdeka Day"); may be empty.
    pub label: String,
}

/// The calendar to draw.
#[derive(Clone, Debug)]
pub struct Calendar {
    pub id: String,
    /// The page's path; the view, step and "+N more" links patch it.
    pub path: String,
    /// Other query values to keep in those links (the owner's filters).
    pub query: HashMap<String, String>,
    pub state: CalendarState,
    /// The IANA display zone every time is drawn in.
    pub zone: String,
    pub today: Date,
    /// Every lane the owner offers; a shared view draws
    /// [`CalendarState::shown_lanes`] of them.
    pub lanes: Vec<CalendarLane>,
    pub items: Vec<CalendarItem>,
    /// What a drag snaps to, in minutes: 5, 10, 15, 30 or 60 (15 default).
    pub snap_minutes: u32,
    pub day_marks: Vec<DayMark>,
    /// The result of the last move, announced politely; a refusal is an error.
    pub notice: Option<(StatusKind, String)>,
    /// The item whose Move dialog is open.
    pub moving: Option<String>,
    /// Errors of the open Move dialog's fields.
    pub move_errors: Vec<String>,
    /// The zone follows the viewer's browser (display mode `local`): the
    /// calendar then reports it as the `browser_timezone` event, which the
    /// owner reads (`bilibi_datetime::Clock::resolve`) to pick `zone`.
    pub follows_browser_zone: bool,
}

impl Calendar {
    pub fn new(
        id: impl Into<String>,
        path: impl Into<String>,
        state: CalendarState,
        zone: impl Into<String>,
        today: Date,
    ) -> Self {
        Self {
            id: id.into(),
            path: path.into(),
            query: HashMap::new(),
            state,
            zone: zone.into(),
            today,
            lanes: Vec::new(),
            items: Vec::new(),
            snap_minutes: 15,
            day_marks: Vec::new(),
            notice: None,
            moving: None,
            move_errors: Vec::new(),
            follows_browser_zone: false,
        }
    }

    /// Renders the calendar. An unknown `zone` is drawn in UTC and says so.
    pub fn render(&self) -> askama::Result<Html> {
        Html::render(&View::build(self)?)
    }
}

/// What a drop, a resize or the Move dialog asks the owner to do: set the
/// item to `when`, in `lane` when it moved to another one. The owner passes
/// `version` to its operation, which refuses a stale one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CalendarMove {
    pub id: String,
    pub version: String,
    pub when: CalendarWhen,
    pub lane: Option<String>,
}

impl CalendarMove {
    /// Decodes [`MOVE`] (a drop or resize: `id`, `version`, `date`, `minute`,
    /// `length`, optional `lane`; all-day items send `days` for `length`) or
    /// [`MOVE_SUBMIT`] (the dialog's `move[...]` fields) in `zone`. A wall
    /// clock time in a daylight-saving gap becomes the first valid instant, in
    /// a fold the earlier. The `Err` is a sentence for the person.
    pub fn from_event(payload: &serde_json::Value, zone: &str) -> Result<Self, String> {
        let invalid = || "Choose a valid date and time.".to_owned();
        let zone = TimeZone::get(zone).map_err(|_| invalid())?;
        let text = |key: &str| payload.get(key).and_then(|value| value.as_str());
        let number = |key: &str| {
            payload.get(key).and_then(|value| {
                value
                    .as_i64()
                    .or_else(|| value.as_str().and_then(|text| text.parse().ok()))
            })
        };
        if text("type") == Some(MOVE_SUBMIT) {
            let form = crate::params::form_values_checked(payload, MOVE_FORM, 64)
                .map_err(|_| invalid())?;
            let field = |key: &str| form.get(key).map(String::as_str).unwrap_or_default();
            let id = field("id").to_owned();
            let version = field("version").to_owned();
            if id.is_empty() {
                return Err(invalid());
            }
            let lane = Some(field("lane"))
                .filter(|lane| !lane.is_empty())
                .map(str::to_owned);
            let first: Date = field("start_date").parse().map_err(|_| invalid())?;
            if field("all_day") == "true" {
                let last: Date = field("end_date").parse().map_err(|_| invalid())?;
                return Self::all_day(id, version, first, last, lane);
            }
            let start = datetime(first, field("start_time"), &invalid)?;
            let end_date: Date = field("end_date").parse().map_err(|_| invalid())?;
            let end = datetime(end_date, field("end_time"), &invalid)?;
            return Self::timed(id, version, &zone, start, end, lane);
        }
        let id = text("id").filter(|id| !id.is_empty()).ok_or_else(invalid)?;
        let version = text("version").unwrap_or_default();
        let date: Date = text("date")
            .and_then(|date| date.parse().ok())
            .ok_or_else(invalid)?;
        let lane = text("lane")
            .filter(|lane| !lane.is_empty())
            .map(str::to_owned);
        if text("all_day") == Some("true") {
            let days = number("days").filter(|days| (1..=LONGEST_MOVE_DAYS).contains(days));
            let last = days
                .and_then(|days| date.checked_add((days - 1).days()).ok())
                .ok_or_else(invalid)?;
            return Self::all_day(id.to_owned(), version.to_owned(), date, last, lane);
        }
        let minute = number("minute").filter(|minute| (0..1440).contains(minute));
        let length =
            number("length").filter(|length| (0..=LONGEST_MOVE_DAYS * 1440).contains(length));
        let (minute, length) = minute.zip(length).ok_or_else(invalid)?;
        let start = date
            .at(0, 0, 0, 0)
            .checked_add(SignedDuration::from_mins(minute))
            .map_err(|_| invalid())?;
        let end = start
            .checked_add(SignedDuration::from_mins(length))
            .map_err(|_| invalid())?;
        Self::timed(id.to_owned(), version.to_owned(), &zone, start, end, lane)
    }

    fn all_day(
        id: String,
        version: String,
        first: Date,
        last: Date,
        lane: Option<String>,
    ) -> Result<Self, String> {
        if last < first {
            return Err("The end must not be before the start.".to_owned());
        }
        if !YEARS.contains(&first.year()) || !YEARS.contains(&last.year()) {
            return Err("Choose a valid date and time.".to_owned());
        }
        Ok(Self {
            id,
            version,
            when: CalendarWhen::AllDay { first, last },
            lane,
        })
    }

    fn timed(
        id: String,
        version: String,
        zone: &TimeZone,
        start: DateTime,
        end: DateTime,
        lane: Option<String>,
    ) -> Result<Self, String> {
        if end < start {
            return Err("The end must not be before the start.".to_owned());
        }
        if !YEARS.contains(&start.year()) || !YEARS.contains(&end.year()) {
            return Err("Choose a valid date and time.".to_owned());
        }
        let instant = |civil: DateTime| {
            zone.to_ambiguous_zoned(civil)
                .compatible()
                .map(|zoned| zoned.timestamp())
                .map_err(|_| "Choose a valid date and time.".to_owned())
        };
        Ok(Self {
            id,
            version,
            when: CalendarWhen::Timed {
                start: instant(start)?,
                end: instant(end)?,
            },
            lane,
        })
    }
}

fn datetime(date: Date, time: &str, invalid: &dyn Fn() -> String) -> Result<DateTime, String> {
    let time: Time = time.parse().map_err(|_| invalid())?;
    Ok(date.to_datetime(time))
}

/// The id of the item a [`MOVE_OPEN`] event names.
pub fn item_id(payload: &serde_json::Value) -> Option<&str> {
    payload
        .get("id")
        .and_then(|id| id.as_str())
        .filter(|id| !id.is_empty() && id.len() <= 128)
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

const BORDER: [&str; 8] = [
    "border-l-cat-1",
    "border-l-cat-2",
    "border-l-cat-3",
    "border-l-cat-4",
    "border-l-cat-5",
    "border-l-cat-6",
    "border-l-cat-7",
    "border-l-cat-8",
];

/// One item as the template draws it, wherever it appears.
struct ItemView {
    dom_id: String,
    id: String,
    title: String,
    kind: String,
    /// "09:00–10:30", "All day", or "Mon 3 Nov – Tue 4 Nov" for a span.
    time: String,
    /// The link's accessible name: time, title, kind.
    name: String,
    href: String,
    tone: &'static str,
    movable: bool,
    resizable: bool,
    version: String,
    all_day: bool,
    /// The wall-clock start and length a drop keeps: date, minute, minutes
    /// (days for an all-day item).
    start_date: String,
    start_minute: i64,
    length: i64,
    lane: String,
    /// The first part of the item is here (a continuation cannot be dragged).
    leads: bool,
    /// Grid classes of a segment in a time column.
    place: String,
    /// The segment keeps going into the next day.
    runs_on: bool,
}

struct ColumnView {
    dom_id: String,
    date: String,
    lane: String,
    heading: String,
    sub: String,
    mark_class: &'static str,
    mark: String,
    today: bool,
    columns: usize,
    all_day: Vec<ItemView>,
    segments: Vec<ItemView>,
}

struct CellView {
    dom_id: String,
    date: String,
    day: String,
    outside: bool,
    today: bool,
    mark_class: &'static str,
    mark: String,
    tab: i32,
    items: Vec<ItemView>,
    more: usize,
    more_href: String,
    description: String,
}

struct AgendaGroup {
    heading: String,
    items: Vec<ItemView>,
}

struct NavLink {
    href: String,
    label: &'static str,
}

struct ViewLink {
    href: String,
    label: &'static str,
    current: bool,
}

#[derive(Template)]
#[template(path = "calendar.html")]
struct View {
    id: String,
    view: &'static str,
    title: String,
    zone_note: String,
    snap: u32,
    views: Vec<ViewLink>,
    previous: NavLink,
    today: NavLink,
    next: NavLink,
    notice: Option<(StatusKind, String)>,
    follows_browser_zone: bool,
    too_many: bool,
    columns: Vec<ColumnView>,
    weekdays: [&'static str; 7],
    cells: Vec<CellView>,
    agenda: Vec<AgendaGroup>,
    empty: bool,
    shared_empty: bool,
    dialog: Option<Html>,
}

fn tz(zone: &str) -> (TimeZone, String) {
    match TimeZone::get(zone) {
        Ok(tz) => (tz, zone.to_owned()),
        Err(_) => (TimeZone::UTC, "UTC".to_owned()),
    }
}

fn clock(time: DateTime) -> String {
    time.strftime("%H:%M").to_string()
}

fn sanitized(text: &str) -> String {
    text.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// An item's span in wall-clock terms, or its dates.
enum Civil {
    Timed(DateTime, DateTime),
    AllDay(Date, Date),
}

impl Civil {
    fn of(item: &CalendarItem, zone: &TimeZone) -> Self {
        match &item.when {
            CalendarWhen::Timed { start, end } => {
                let start = start.to_zoned(zone.clone()).datetime();
                let end = end.to_zoned(zone.clone()).datetime().max(start);
                Self::Timed(start, end)
            }
            CalendarWhen::AllDay { first, last } => Self::AllDay(*first, (*last).max(*first)),
        }
    }

    /// Whether any part of the item falls on `date`.
    fn on(&self, date: Date) -> bool {
        match self {
            Self::Timed(start, end) => {
                let first = date.at(0, 0, 0, 0);
                let after = date.checked_add(1.day()).unwrap_or(date).at(0, 0, 0, 0);
                *start < after && (*end > first || *start >= first)
            }
            Self::AllDay(first, last) => *first <= date && date <= *last,
        }
    }

    fn sort_key(&self) -> (u8, DateTime) {
        match self {
            Self::AllDay(first, _) => (0, first.at(0, 0, 0, 0)),
            Self::Timed(start, _) => (1, *start),
        }
    }
}

impl View {
    fn build(calendar: &Calendar) -> askama::Result<Self> {
        let (zone, zone_name) = tz(&calendar.zone);
        let state = &calendar.state;
        let dates = state.dates();
        let snap = match calendar.snap_minutes {
            5 | 10 | 15 | 30 | 60 => calendar.snap_minutes,
            _ => 15,
        };
        let too_many = calendar.items.len() > MAX_ITEMS;
        let items: Vec<(&CalendarItem, Civil)> = if too_many {
            Vec::new()
        } else {
            let mut items: Vec<_> = calendar
                .items
                .iter()
                .map(|item| (item, Civil::of(item, &zone)))
                .collect();
            items.sort_by_key(|(item, civil)| (civil.sort_key(), item.id.clone()));
            items
        };
        let shown_lanes = state.shown_lanes(&calendar.lanes);
        let address = |view: CalendarView, date: Date| {
            let mut query = calendar.query.clone();
            query.extend(
                CalendarState {
                    view,
                    date,
                    lanes: state.lanes.clone(),
                }
                .to_params(),
            );
            crate::params::address(&calendar.path, &query)
        };
        let views = CalendarView::ALL
            .into_iter()
            .map(|view| ViewLink {
                href: address(view, state.date),
                label: view.label(),
                current: view == state.view,
            })
            .collect();
        let previous = NavLink {
            href: address(state.view, state.step(false)),
            label: match state.view {
                CalendarView::Day | CalendarView::Shared => "Previous day",
                CalendarView::Week => "Previous week",
                CalendarView::Month => "Previous month",
            },
        };
        let next = NavLink {
            href: address(state.view, state.step(true)),
            label: match state.view {
                CalendarView::Day | CalendarView::Shared => "Next day",
                CalendarView::Week => "Next week",
                CalendarView::Month => "Next month",
            },
        };
        let today = NavLink {
            href: address(state.view, calendar.today),
            label: "Today",
        };
        let mark = |date: Date| {
            calendar
                .day_marks
                .iter()
                .find(|mark| mark.date == date)
                .map(|mark| {
                    let word = if mark.label.is_empty() {
                        mark.kind.label().to_owned()
                    } else {
                        format!("{}: {}", mark.kind.label(), mark.label)
                    };
                    (mark.kind.class(), word)
                })
                .unwrap_or(("bg-surface", String::new()))
        };
        let item_view = |item: &CalendarItem, civil: &Civil, date: Date, suffix: &str| {
            Self::item(calendar, item, civil, date, suffix)
        };
        let mut columns = Vec::new();
        let mut cells = Vec::new();
        match state.view {
            CalendarView::Day | CalendarView::Week | CalendarView::Shared => {
                let headings: Vec<(Date, Option<&CalendarLane>)> =
                    if state.view == CalendarView::Shared {
                        shown_lanes
                            .iter()
                            .map(|lane| (state.date, Some(*lane)))
                            .collect()
                    } else {
                        dates.iter().map(|date| (*date, None)).collect()
                    };
                for (date, lane) in headings {
                    let lane_key = lane.map(|lane| lane.key.as_str());
                    let mut column = ColumnView {
                        dom_id: format!(
                            "{}-col-{}{}",
                            calendar.id,
                            date,
                            lane_key
                                .map(|key| format!("-{}", sanitized(key)))
                                .unwrap_or_default()
                        ),
                        date: date.to_string(),
                        lane: lane_key.unwrap_or_default().to_owned(),
                        heading: lane
                            .map(|lane| lane.label.clone())
                            .unwrap_or_else(|| date.strftime("%a %-d").to_string()),
                        sub: if lane.is_some() {
                            date.strftime("%a %-d %b").to_string()
                        } else {
                            date.strftime("%b").to_string()
                        },
                        mark_class: "bg-surface",
                        mark: String::new(),
                        today: date == calendar.today,
                        columns: 1,
                        all_day: Vec::new(),
                        segments: Vec::new(),
                    };
                    (column.mark_class, column.mark) = mark(date);
                    let mut occupied: Vec<(i64, i64, usize)> = Vec::new();
                    let mut placed = Vec::new();
                    for (item, civil) in &items {
                        if lane_key.is_some() && item.lane.as_deref() != lane_key {
                            continue;
                        }
                        if !civil.on(date) {
                            continue;
                        }
                        let mut view = item_view(item, civil, date, "");
                        match civil {
                            Civil::AllDay(..) => column.all_day.push(view),
                            Civil::Timed(start, end) => {
                                let day_start = date.at(0, 0, 0, 0);
                                let day_end =
                                    date.checked_add(1.day()).unwrap_or(date).at(0, 0, 0, 0);
                                let from = (*start).max(day_start);
                                let to = (*end).min(day_end);
                                let begin = from.duration_since(day_start).as_mins().clamp(0, 1440);
                                let finish = to.duration_since(day_start).as_mins().clamp(0, 1440);
                                view.leads = start.date() == date;
                                view.runs_on = *end > day_end;
                                let row = begin / ROW_MINUTES;
                                let rows =
                                    (((finish - begin) + ROW_MINUTES - 1) / ROW_MINUTES).max(1);
                                let rows = rows.min(ROWS - row).max(1);
                                // The first side-by-side slot that is free.
                                occupied.retain(|(_, until, _)| *until > begin);
                                let slot = (0..MAX_SIDE_BY_SIDE)
                                    .find(|slot| {
                                        !occupied.iter().any(|(_, _, taken)| taken == slot)
                                    })
                                    .unwrap_or(MAX_SIDE_BY_SIDE - 1);
                                occupied.push((begin, finish.max(begin + 1), slot));
                                placed.push((view, row + 1, rows, slot + 1));
                            }
                        }
                    }
                    let width = placed
                        .iter()
                        .map(|(_, _, _, slot)| *slot)
                        .max()
                        .unwrap_or(1);
                    column.columns = width;
                    for (mut view, row, rows, slot) in placed {
                        view.place = format!("row-start-{row} row-span-{rows} col-start-{slot}");
                        column.segments.push(view);
                    }
                    columns.push(column);
                }
            }
            CalendarView::Month => {
                for (index, date) in dates.iter().enumerate() {
                    let outside = date.month() != state.date.month();
                    let (mark_class, mark_text) = mark(*date);
                    let mut listed = Vec::new();
                    for (item, civil) in &items {
                        if civil.on(*date) {
                            listed.push(item_view(item, civil, *date, ""));
                        }
                    }
                    let more = listed.len().saturating_sub(CELL_ITEMS);
                    let count = listed.len();
                    listed.truncate(CELL_ITEMS);
                    let mut description = date.strftime("%A %-d %B %Y").to_string();
                    if *date == calendar.today {
                        description.push_str(", today");
                    }
                    if !mark_text.is_empty() {
                        description.push_str(&format!(", {mark_text}"));
                    }
                    description.push_str(&match count {
                        0 => ", nothing scheduled".to_owned(),
                        1 => ", 1 item".to_owned(),
                        n => format!(", {n} items"),
                    });
                    cells.push(CellView {
                        dom_id: format!("{}-cell-{}", calendar.id, date),
                        date: date.to_string(),
                        day: date.strftime("%-d").to_string(),
                        outside,
                        today: *date == calendar.today,
                        mark_class,
                        mark: mark_text,
                        tab: if *date == state.date || (index == 0 && state.date < dates[0]) {
                            0
                        } else {
                            -1
                        },
                        items: listed,
                        more,
                        more_href: address(CalendarView::Day, *date),
                        description,
                    });
                }
            }
        }
        // The agenda a phone shows in place of the grid: the same items, by
        // day (by lane when shared), each with its Move action.
        let mut agenda = Vec::new();
        if state.view == CalendarView::Shared {
            for lane in &shown_lanes {
                let listed: Vec<ItemView> = items
                    .iter()
                    .filter(|(item, civil)| {
                        item.lane.as_deref() == Some(lane.key.as_str()) && civil.on(state.date)
                    })
                    .map(|(item, civil)| item_view(item, civil, state.date, "-agenda"))
                    .collect();
                agenda.push(AgendaGroup {
                    heading: lane.label.clone(),
                    items: listed,
                });
            }
        } else {
            for date in &dates {
                let listed: Vec<ItemView> = items
                    .iter()
                    .filter(|(_, civil)| civil.on(*date))
                    .map(|(item, civil)| item_view(item, civil, *date, "-agenda"))
                    .collect();
                if !listed.is_empty() || state.view != CalendarView::Month {
                    agenda.push(AgendaGroup {
                        heading: date.strftime("%A %-d %B").to_string(),
                        items: listed,
                    });
                }
            }
        }
        let title = match state.view {
            CalendarView::Day | CalendarView::Shared => {
                state.date.strftime("%A %-d %B %Y").to_string()
            }
            CalendarView::Month => state.date.strftime("%B %Y").to_string(),
            CalendarView::Week => {
                let (first, last) = (dates[0], dates[dates.len() - 1]);
                if first.month() == last.month() {
                    format!("{} – {}", first.strftime("%-d"), last.strftime("%-d %B %Y"))
                } else if first.year() == last.year() {
                    format!(
                        "{} – {}",
                        first.strftime("%-d %b"),
                        last.strftime("%-d %b %Y")
                    )
                } else {
                    format!(
                        "{} – {}",
                        first.strftime("%-d %b %Y"),
                        last.strftime("%-d %b %Y")
                    )
                }
            }
        };
        let dialog = match &calendar.moving {
            Some(id) => calendar
                .items
                .iter()
                .find(|item| &item.id == id && item.movable)
                .map(|item| Self::dialog(calendar, item, &zone))
                .transpose()?,
            None => None,
        };
        let empty = !too_many && items.is_empty();
        let shared_empty = state.view == CalendarView::Shared && shown_lanes.is_empty();
        Ok(Self {
            id: calendar.id.clone(),
            view: state.view.name(),
            title,
            zone_note: format!("Times are shown in {zone_name}."),
            snap,
            views,
            previous,
            today,
            next,
            notice: calendar.notice.clone(),
            follows_browser_zone: calendar.follows_browser_zone,
            too_many,
            columns,
            weekdays: WEEKDAYS,
            cells,
            agenda,
            empty,
            shared_empty,
            dialog,
        })
    }

    /// `item` as it appears on `date`.
    fn item(
        calendar: &Calendar,
        item: &CalendarItem,
        civil: &Civil,
        date: Date,
        suffix: &str,
    ) -> ItemView {
        let (time, start_date, start_minute, length, all_day) = match civil {
            Civil::AllDay(first, last) => {
                let days = last.since(*first).map(|span| span.get_days()).unwrap_or(0) + 1;
                let time = if first == last {
                    "All day".to_owned()
                } else {
                    format!(
                        "All day, {} to {}",
                        first.strftime("%a %-d %b"),
                        last.strftime("%a %-d %b")
                    )
                };
                (time, first.to_string(), 0, i64::from(days), true)
            }
            Civil::Timed(start, end) => {
                let length = end.duration_since(*start).as_mins();
                let time = if start.date() == end.date() {
                    format!("{}–{}", clock(*start), clock(*end))
                } else {
                    format!(
                        "{} – {} {}",
                        clock(*start),
                        end.date().strftime("%a %-d %b"),
                        clock(*end)
                    )
                };
                let minute = start.time().hour() as i64 * 60 + start.time().minute() as i64;
                (time, start.date().to_string(), minute, length, false)
            }
        };
        let mut name = format!("{time}, {}", item.title);
        if !item.kind.is_empty() {
            name.push_str(&format!(", {}", item.kind));
        }
        ItemView {
            dom_id: format!(
                "{}-item-{}{}-{}",
                calendar.id,
                sanitized(&item.id),
                suffix,
                date
            ),
            id: item.id.clone(),
            title: item.title.clone(),
            kind: item.kind.clone(),
            time,
            name,
            href: item.href.clone(),
            tone: BORDER[usize::from(item.tone.clamp(1, 8)) - 1],
            movable: item.movable,
            resizable: item.movable && item.resizable && !all_day,
            version: item.version.clone(),
            all_day,
            start_date,
            start_minute,
            length,
            lane: item.lane.clone().unwrap_or_default(),
            leads: true,
            place: String::new(),
            runs_on: false,
        }
    }

    /// The Move dialog: the item's start and end in the display zone.
    fn dialog(calendar: &Calendar, item: &CalendarItem, zone: &TimeZone) -> askama::Result<Html> {
        let civil = Civil::of(item, zone);
        let field = |key: &str, label: &str, kind: InputKind, value: String| {
            Field::in_form(MOVE_FORM, key)
                .id(format!("{}-move-{key}", calendar.id))
                .label(label)
                .kind(kind)
                .value(value)
                .required()
        };
        let hidden = |key: &str, value: &str| {
            Field::in_form(MOVE_FORM, key)
                .id(format!("{}-move-{key}", calendar.id))
                .kind(InputKind::Hidden)
                .value(value)
        };
        let mut fields = vec![
            Html::render(&hidden("id", &item.id))?,
            Html::render(&hidden("version", &item.version))?,
        ];
        if let Some(lane) = &item.lane {
            fields.push(Html::render(&hidden("lane", lane))?);
        }
        match civil {
            Civil::AllDay(first, last) => {
                fields.push(Html::render(&hidden("all_day", "true"))?);
                fields.push(Html::render(&field(
                    "start_date",
                    "First day",
                    InputKind::Date,
                    first.to_string(),
                ))?);
                fields.push(Html::render(&field(
                    "end_date",
                    "Last day",
                    InputKind::Date,
                    last.to_string(),
                ))?);
            }
            Civil::Timed(start, end) => {
                fields.push(Html::render(&field(
                    "start_date",
                    "Start date",
                    InputKind::Date,
                    start.date().to_string(),
                ))?);
                fields.push(Html::render(&field(
                    "start_time",
                    "Start time",
                    InputKind::Time,
                    clock(start),
                ))?);
                fields.push(Html::render(&field(
                    "end_date",
                    "End date",
                    InputKind::Date,
                    end.date().to_string(),
                ))?);
                fields.push(Html::render(&field(
                    "end_time",
                    "End time",
                    InputKind::Time,
                    clock(end),
                ))?);
            }
        }
        let mut body = Html::EMPTY;
        if !calendar.move_errors.is_empty() {
            let mut sentences = Html::EMPTY;
            for error in &calendar.move_errors {
                sentences.push(&Html::text(error));
            }
            body.push(&Html::render(&Alert {
                kind: StatusKind::Error,
                id: &format!("{}-move-error", calendar.id),
                class: "mb-3",
                body: sentences,
            })?);
        }
        let mut grid = Html::raw("<div class=\"grid gap-x-3 sm:grid-cols-2\">");
        for field in &fields {
            grid.push(field);
        }
        grid.push(&Html::raw("</div>"));
        body.push(&grid);
        let actions = {
            let mut actions = Html::render(&Button {
                variant: ButtonVariant::Primary,
                id: &format!("{}-move-save", calendar.id),
                button_type: "submit",
                working: "Moving…",
                body: Html::text("Move"),
                ..Button::default()
            })?;
            actions.push(&Html::render(&Button {
                id: &format!("{}-move-cancel", calendar.id),
                button_type: "button",
                event: MOVE_CANCEL,
                body: Html::text("Cancel"),
                ..Button::default()
            })?);
            actions
        };
        let form = Html::render(&Form {
            id: &format!("{}-move-form", calendar.id),
            event: MOVE_SUBMIT,
            change: false,
            class: "",
            body,
            actions,
        })?;
        Html::render(&Modal {
            id: &format!("{}-move-dialog", calendar.id),
            title: &format!("Move {}", item.title),
            cancel_event: MOVE_CANCEL,
            width: ModalWidth::Narrow,
            description: "Times are in the zone the calendar shows.",
            role: "",
            flash: None,
            body: form,
        })
    }
}
