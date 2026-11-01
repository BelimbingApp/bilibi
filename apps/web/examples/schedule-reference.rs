//! Running specimen of the shared calendar and status board, separate from
//! product routes. Run
//! with an isolated, migrated DATABASE_URL, APP_ORIGIN and BIND_ADDR; the
//! walkthrough fixture signs in (`manager@example.test` holds
//! `reference.appointments.update`, `viewer@example.test` holds only
//! `reference.appointments.view`).
//!
//! `/schedule-reference/calendar` shows twelve appointments (and a backlog of
//! seventy planned ones a month out) held in memory, with
//! a start, an end, a lane (person), a kind and a version. The page is the
//! owner: it reads only the visible range, and a drop, a resize and the Move
//! dialog all reach `Schedule::reschedule`, which rechecks the capability and the
//! record's version before it changes anything. A stale version or a missing
//! capability is answered with a message and the appointment stays where it was.
//! The owner prints one line per applied or refused move, so a forged event shows
//! as a refusal here.
//!
//! `/schedule-reference/board` shows the same appointments as cards in columns by
//! status (planned, confirmed, done), filtered by person through `ListState`. A
//! drop and the Move dialog reach `Schedule::set_status`, which rechecks the
//! capability, the record's version (shared with the calendar), that the card is
//! still in the column it was dragged from, and the status edges. The edges set
//! each card's `targets` for a person who may update; the operation decides again.
use bilibi_authz::{
    authorize_key, CapabilityDefinition, RoleDefinition, VocabularyContribution, VOCABULARY,
};
use bilibi_base::datetime::{Clock, DisplayPolicy};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{
    contributions::{Access, CompanyScope, Contribution, Method, Route, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_security::Scope;
use bilibi_ui::{
    board::{self, Board, BoardCard, BoardColumn, BoardMove, BoardState},
    calendar::{
        self, Calendar, CalendarItem, CalendarLane, CalendarMove, CalendarState, CalendarWhen,
        MAX_ITEMS,
    },
    choices,
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest, PageService,
    },
    Filter, FilterToolbar, Header, Html, ListSpec, ListState, StatusKind, ToolbarControl,
};
use jiff::{tz::TimeZone, Timestamp, ToSpan};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock, Mutex},
};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const MODULE: &str = "extensions/schedule_reference";
const PAGES: &str = "extensions/schedule_reference.pages";
const VIEW: &str = "reference.appointments.view";
const UPDATE: &str = "reference.appointments.update";
const PATH: &str = "/schedule-reference/calendar";
const BOARD_PATH: &str = "/schedule-reference/board";

macro_rules! provenance {
    () => {
        Provenance {
            module: MODULE,
            source: file!(),
            line: line!(),
        }
    };
}

inventory::submit! { Contribution::new(VOCABULARY, "schedule_reference.vocabulary", provenance!(), &VocabularyContribution {
    domains: &["reference"], verbs: &["view", "update"],
    capabilities: &[CapabilityDefinition::tenant(VIEW), CapabilityDefinition::tenant(UPDATE)],
    roles: &[
        RoleDefinition { code: "manager", name: "manager", description: "Extension of the fixture manager role", grant_all: false, capabilities: &[VIEW, UPDATE] },
        RoleDefinition { code: "viewer", name: "viewer", description: "Extension of the fixture viewer role", grant_all: false, capabilities: &[VIEW] },
    ],
    system_principals: &[],
}) }

/// The statuses in board order, and the statuses each may move to (the edges).
const STATUSES: [(&str, &str, &[&str]); 3] = [
    ("planned", "Planned", &["confirmed"]),
    ("confirmed", "Confirmed", &["planned", "done"]),
    ("done", "Done", &["confirmed"]),
];

const LANES: [(&str, &str); 3] = [("amira", "Amira"), ("bram", "Bram"), ("chen", "Chen")];

#[derive(Clone)]
struct Appointment {
    id: String,
    title: String,
    kind: &'static str,
    tone: u8,
    lane: &'static str,
    when: CalendarWhen,
    status: &'static str,
    version: u64,
}

/// The owner's records and the only code that changes them.
struct Schedule {
    records: Mutex<Vec<Appointment>>,
}

impl Schedule {
    fn seeded() -> Self {
        let monday = {
            let today = Timestamp::now().to_zoned(TimeZone::UTC).date();
            today
                .checked_sub(i64::from(today.weekday().to_monday_zero_offset()).days())
                .expect("a date near today")
        };
        let at = |day: i64, hour: i8, minute: i8| {
            TimeZone::UTC
                .to_timestamp(
                    monday
                        .checked_add(day.days())
                        .expect("a date near today")
                        .at(hour, minute, 0, 0),
                )
                .expect("a time near today")
        };
        let timed = |day: i64, from: (i8, i8), to: (i8, i8)| CalendarWhen::Timed {
            start: at(day, from.0, from.1),
            end: at(day, to.0, to.1),
        };
        let rows: [(&str, &str, u8, &str, CalendarWhen); 12] = [
            (
                "Weekly planning",
                "Meeting",
                1,
                "amira",
                timed(0, (9, 0), (10, 0)),
            ),
            (
                "Supplier call",
                "Call",
                2,
                "bram",
                timed(0, (9, 30), (10, 15)),
            ),
            (
                "Prepare quote",
                "Task",
                3,
                "amira",
                timed(1, (13, 0), (15, 0)),
            ),
            ("Site visit", "Visit", 4, "chen", timed(1, (8, 30), (12, 0))),
            (
                "Design review",
                "Meeting",
                1,
                "amira",
                timed(2, (11, 0), (12, 30)),
            ),
            (
                "Customer call",
                "Call",
                2,
                "chen",
                timed(2, (11, 30), (12, 0)),
            ),
            (
                "Warehouse stocktake",
                "Closure",
                5,
                "bram",
                CalendarWhen::AllDay {
                    first: monday.checked_add(3.days()).expect("a date near today"),
                    last: monday.checked_add(4.days()).expect("a date near today"),
                },
            ),
            ("Training", "Task", 3, "bram", timed(3, (14, 0), (16, 0))),
            ("Handover", "Meeting", 1, "chen", timed(4, (16, 0), (17, 0))),
            (
                "Overnight shift",
                "Shift",
                6,
                "bram",
                timed(4, (22, 0), (23, 59)),
            ),
            (
                "Planning offsite",
                "Visit",
                4,
                "amira",
                timed(7, (10, 0), (15, 30)),
            ),
            ("Review call", "Call", 2, "chen", timed(8, (9, 0), (9, 45))),
        ];
        let records = rows
            .into_iter()
            .enumerate()
            .map(|(index, (title, kind, tone, lane, when))| Appointment {
                id: format!("a{}", index + 1),
                title: title.to_owned(),
                kind,
                tone,
                lane,
                when,
                status: STATUSES[index % STATUSES.len()].0,
                version: 1,
            })
            .collect::<Vec<_>>();
        // A backlog far from today, so the board's "Show more" has more than a
        // page of planned appointments to show and the calendar stays readable.
        let backlog = (0..70i64).map(|n| Appointment {
            id: format!("b{}", n + 1),
            title: format!("Inspection {}", n + 1),
            kind: "Task",
            tone: 3,
            lane: LANES[(n % 3) as usize].0,
            when: timed(
                28 + n / 3,
                (9 + (n % 3) as i8 * 2, 0),
                (10 + (n % 3) as i8 * 2, 0),
            ),
            status: "planned",
            version: 1,
        });
        let records = records.into_iter().chain(backlog).collect();
        Self {
            records: Mutex::new(records),
        }
    }

    /// The appointments touching `[from, to)` for the lanes shown.
    fn read(&self, from: Timestamp, to: Timestamp, zone: &TimeZone) -> Vec<Appointment> {
        let first = from.to_zoned(zone.clone()).date();
        let last = to
            .to_zoned(zone.clone())
            .date()
            .checked_sub(1.day())
            .unwrap_or(first);
        self.records
            .lock()
            .expect("schedule lock")
            .iter()
            .filter(|record| match &record.when {
                CalendarWhen::Timed { start, end } => *start < to && *end >= from,
                CalendarWhen::AllDay { first: a, last: b } => *a <= last && *b >= first,
            })
            .take(MAX_ITEMS + 1)
            .cloned()
            .collect()
    }

    /// The owner's operation: capability, then version, then the change. Every
    /// way of moving an appointment ends here.
    async fn reschedule(
        &self,
        db: &Db,
        scope: &Scope,
        change: &CalendarMove,
    ) -> std::result::Result<String, String> {
        let client = db
            .get()
            .await
            .map_err(|_| "The schedule could not be reached.".to_owned())?;
        if authorize_key(&**client, scope, UPDATE).await.is_err() {
            println!(
                "schedule: refused {} for {}: no capability",
                change.id, scope.email
            );
            return Err("You do not have permission to move appointments.".to_owned());
        }
        let mut records = self.records.lock().expect("schedule lock");
        let record = records
            .iter_mut()
            .find(|record| record.id == change.id)
            .ok_or_else(|| "That appointment no longer exists.".to_owned())?;
        if record.version.to_string() != change.version {
            println!(
                "schedule: refused {} for {}: stale version",
                change.id, scope.email
            );
            return Err(format!(
                "{} was changed by someone else. It is shown as it now stands.",
                record.title
            ));
        }
        if std::mem::discriminant(&record.when) != std::mem::discriminant(&change.when) {
            return Err("That appointment cannot change between timed and all day.".to_owned());
        }
        record.when = change.when.clone();
        if let Some(lane) = &change.lane {
            record.lane = LANES
                .iter()
                .find(|(key, _)| key == lane)
                .map(|(key, _)| *key)
                .ok_or_else(|| "That person is not on the schedule.".to_owned())?;
        }
        record.version += 1;
        println!(
            "schedule: {} moved by {} to {:?} (version {})",
            record.id, scope.email, record.when, record.version
        );
        Ok(format!("Moved {}.", record.title))
    }
}

impl Schedule {
    /// The owner's status operation: capability, then version, then that the
    /// card is still where it was dragged from, then the edge. A drop and the
    /// Move dialog both end here.
    async fn set_status(
        &self,
        db: &Db,
        scope: &Scope,
        change: &BoardMove,
    ) -> std::result::Result<String, String> {
        let client = db
            .get()
            .await
            .map_err(|_| "The schedule could not be reached.".to_owned())?;
        if authorize_key(&**client, scope, UPDATE).await.is_err() {
            println!(
                "schedule: refused status of {} for {}: no capability",
                change.id, scope.email
            );
            return Err("You do not have permission to move appointments.".to_owned());
        }
        let mut records = self.records.lock().expect("schedule lock");
        let record = records
            .iter_mut()
            .find(|record| record.id == change.id)
            .ok_or_else(|| "That appointment no longer exists.".to_owned())?;
        if record.version.to_string() != change.version || record.status != change.from {
            println!(
                "schedule: refused status of {} for {}: stale",
                change.id, scope.email
            );
            return Err(format!(
                "{} was changed by someone else. It is shown as it now stands.",
                record.title
            ));
        }
        let allowed = STATUSES
            .iter()
            .find(|(key, _, _)| *key == record.status)
            .is_some_and(|(_, _, edges)| edges.contains(&change.to.as_str()));
        let to = STATUSES
            .iter()
            .find(|(key, _, _)| *key == change.to)
            .filter(|_| allowed)
            .ok_or_else(|| format!("{} cannot move to that column.", record.title))?;
        record.status = to.0;
        record.version += 1;
        println!(
            "schedule: {} moved by {} to status {} (version {})",
            record.id, scope.email, record.status, record.version
        );
        Ok(format!("Moved {} to {}.", record.title, to.1))
    }

    /// The appointments in `status` for the lane filter (`all` or a lane key):
    /// how many there are, and at most `limit` of them in start order.
    fn column(&self, status: &str, lane: &str, limit: usize) -> (u64, Vec<Appointment>) {
        let records = self.records.lock().expect("schedule lock");
        let mut matching: Vec<&Appointment> = records
            .iter()
            .filter(|record| record.status == status && (lane == "all" || record.lane == lane))
            .collect();
        matching.sort_by_key(|record| match &record.when {
            CalendarWhen::Timed { start, .. } => *start,
            CalendarWhen::AllDay { first, .. } => TimeZone::UTC
                .to_timestamp(first.at(0, 0, 0, 0))
                .unwrap_or(Timestamp::MIN),
        });
        (
            matching.len() as u64,
            matching.into_iter().take(limit).cloned().collect(),
        )
    }
}

struct Pages {
    db: Db,
    display: Arc<DisplayPolicy>,
    schedule: Arc<Schedule>,
}

struct CalendarPage {
    db: Db,
    display: Arc<DisplayPolicy>,
    schedule: Arc<Schedule>,
    query: HashMap<String, String>,
    browser_zone: Option<String>,
    moving: Option<String>,
    move_errors: Vec<String>,
    notice: Option<(StatusKind, String)>,
}

fn lanes() -> Vec<CalendarLane> {
    LANES
        .iter()
        .map(|(key, label)| CalendarLane::new(*key, *label))
        .collect()
}

impl CalendarPage {
    async fn body(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.db.get().await?;
        authorize_key(&**client, scope, VIEW).await?;
        let can_update = authorize_key(&**client, scope, UPDATE).await.is_ok();
        let display = self
            .display
            .display(&**client, scope, None, "en-MY")
            .await?;
        let clock = Clock::resolve(&display, self.browser_zone.as_deref(), Timestamp::now());
        let offered = lanes();
        let state = CalendarState::parse(&self.query, clock.today, &offered);
        let zone = TimeZone::get(&clock.timezone).map_err(|_| Error::Internal)?;
        let (from, to) = state.range(&clock.timezone).ok_or(Error::Internal)?;
        let shown: Vec<String> = state
            .shown_lanes(&offered)
            .into_iter()
            .map(|lane| lane.key.clone())
            .collect();
        let mut calendar = Calendar::new(
            "schedule-calendar",
            PATH,
            state.clone(),
            clock.timezone.clone(),
            clock.today,
        );
        calendar.lanes = offered;
        calendar.follows_browser_zone = display.mode == bilibi_base::datetime::Mode::Local;
        calendar.items = self
            .schedule
            .read(from, to, &zone)
            .into_iter()
            .filter(|record| {
                state.view != calendar::CalendarView::Shared
                    || shown.iter().any(|lane| lane == record.lane)
            })
            .map(|record| {
                let item = match &record.when {
                    CalendarWhen::Timed { start, end } => {
                        CalendarItem::timed(&record.id, record.title.clone(), *start, *end)
                    }
                    CalendarWhen::AllDay { first, last } => {
                        CalendarItem::all_day(&record.id, record.title.clone(), *first, *last)
                    }
                }
                .kind(record.kind, record.tone)
                .lane(record.lane);
                if can_update {
                    let item = item.movable(record.version.to_string());
                    match record.when {
                        CalendarWhen::Timed { .. } => item.resizable(),
                        CalendarWhen::AllDay { .. } => item,
                    }
                } else {
                    item
                }
            })
            .collect();
        calendar.notice = self.notice.clone();
        calendar.moving = self.moving.clone().filter(|_| can_update);
        calendar.move_errors = self.move_errors.clone();
        let mut html = Html::render(&Header {
            title: Html::text("Schedule"),
            subtitle:
                "Appointments by person. Drag to move, drag the bottom edge to change the length.",
            ..Header::default()
        })
        .map_err(|_| Error::Internal)?;
        html.push(&calendar.render().map_err(|_| Error::Internal)?);
        let html = Html::render(&bilibi_ui::Page {
            id: "schedule-reference",
            body: html,
            ..bilibi_ui::Page::default()
        })
        .map_err(|_| Error::Internal)?;
        Ok(LiveBody {
            html: html.as_str().to_owned(),
            query: state.to_params(),
            ..LiveBody::default()
        })
    }

    /// The zone moves are read in: the one the calendar draws.
    async fn zone(&self, scope: &Scope) -> Result<String> {
        let client = self.db.get().await?;
        let display = self
            .display
            .display(&**client, scope, None, "en-MY")
            .await?;
        Ok(Clock::resolve(&display, self.browser_zone.as_deref(), Timestamp::now()).timezone)
    }

    async fn handle(&mut self, scope: &Scope, event: &serde_json::Value) -> Result<()> {
        let get = |key: &str| event.get(key).and_then(|value| value.as_str());
        match get("type") {
            Some("browser_timezone") => {
                self.browser_zone = get("timezone")
                    .filter(|zone| bilibi_base::datetime::valid_timezone(zone))
                    .map(str::to_owned);
            }
            Some(calendar::MOVE_OPEN) => {
                let client = self.db.get().await?;
                if authorize_key(&**client, scope, UPDATE).await.is_ok() {
                    self.moving = calendar::item_id(event).map(str::to_owned);
                    self.move_errors.clear();
                    self.notice = None;
                } else {
                    self.notice = Some((
                        StatusKind::Error,
                        "You do not have permission to move appointments.".into(),
                    ));
                }
            }
            Some(calendar::MOVE_CANCEL) => {
                self.moving = None;
                self.move_errors.clear();
            }
            Some(name @ (calendar::MOVE | calendar::MOVE_SUBMIT)) => {
                let zone = self.zone(scope).await?;
                match CalendarMove::from_event(event, &zone) {
                    Err(sentence) if name == calendar::MOVE_SUBMIT => {
                        self.move_errors = vec![sentence];
                    }
                    Err(sentence) => self.notice = Some((StatusKind::Error, sentence)),
                    Ok(change) => {
                        match self.schedule.reschedule(&self.db, scope, &change).await {
                            Ok(message) => self.notice = Some((StatusKind::Success, message)),
                            Err(message) => self.notice = Some((StatusKind::Error, message)),
                        }
                        self.moving = None;
                        self.move_errors.clear();
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for CalendarPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.query = values.clone();
                    self.notice = None;
                }
                LiveInput::Event(event) => self.handle(request.scope, event).await?,
            }
            self.body(request.scope).await
        })
    }
}

static BOARD_SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        filters: BTreeMap::from([(
            "lane".into(),
            Filter::one_of(&["all", "amira", "bram", "chen"], "all"),
        )]),
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("schedule board spec")
});

struct BoardPage {
    db: Db,
    display: Arc<DisplayPolicy>,
    schedule: Arc<Schedule>,
    filters: ListState,
    board: BoardState,
    moving: Option<String>,
    move_errors: Vec<String>,
    notice: Option<(StatusKind, String)>,
}

impl BoardPage {
    fn parse(query: &HashMap<String, String>) -> Result<ListState> {
        ListState::parse(query, BOARD_SPEC.clone()).map_err(|_| Error::Internal)
    }

    async fn body(&mut self, scope: &Scope, query: &HashMap<String, String>) -> Result<LiveBody> {
        let client = self.db.get().await?;
        authorize_key(&**client, scope, VIEW).await?;
        let can_update = authorize_key(&**client, scope, UPDATE).await.is_ok();
        let display = self
            .display
            .display(&**client, scope, None, "en-MY")
            .await?;
        let clock = Clock::resolve(&display, None, Timestamp::now());
        let zone = TimeZone::get(&clock.timezone).map_err(|_| Error::Internal)?;
        let lane = self.filters.filters["lane"].clone();
        let mut columns = Vec::new();
        let mut cards = Vec::new();
        for (key, label, edges) in STATUSES {
            let (total, records) = self.schedule.column(key, &lane, self.board.limit(key));
            columns.push(BoardColumn::new(key, label, total));
            for record in records {
                let (when, date) = match &record.when {
                    CalendarWhen::Timed { start, .. } => {
                        let start = start.to_zoned(zone.clone());
                        (start.strftime("%a %-d %b %H:%M").to_string(), start.date())
                    }
                    CalendarWhen::AllDay { first, .. } => {
                        (format!("All day, {}", first.strftime("%a %-d %b")), *first)
                    }
                };
                let lane_label = LANES
                    .iter()
                    .find(|(key, _)| *key == record.lane)
                    .map(|(_, label)| *label)
                    .unwrap_or(record.lane);
                let card = BoardCard::new(&record.id, key, record.title.clone())
                    .fact(format!("{} · {lane_label}", record.kind))
                    .fact(when)
                    .href(format!("{PATH}?date={date}&view=week"));
                cards.push(if can_update {
                    card.movable(record.version.to_string(), edges.iter().copied())
                } else {
                    card
                });
            }
        }
        self.board.read_column(query, &columns);
        let mut board = Board::new("schedule-board", BOARD_PATH, self.board.clone(), columns);
        board.query = self.filters.to_params();
        board.cards = cards;
        board.notice = self.notice.clone();
        board.moving = self.moving.clone().filter(|_| can_update);
        board.move_errors = self.move_errors.clone();
        let form = self.filters.filters_form();
        let toolbar = Html::render(&FilterToolbar::new(
            "schedule-board-filters",
            vec![ToolbarControl::select(
                "schedule-board-lane",
                "lane",
                "Person",
                &form["lane"],
                choices(&[
                    ("Everyone", "all"),
                    ("Amira", "amira"),
                    ("Bram", "bram"),
                    ("Chen", "chen"),
                ]),
            )],
        ))
        .map_err(|_| Error::Internal)?;
        let mut html = Html::render(&Header {
            title: Html::text("Schedule board"),
            subtitle: "Appointments by status. Drag a card to another column.",
            ..Header::default()
        })
        .map_err(|_| Error::Internal)?;
        html.push(&toolbar);
        html.push(&board.render().map_err(|_| Error::Internal)?);
        let html = Html::render(&bilibi_ui::Page {
            id: "schedule-board-reference",
            body: html,
            ..bilibi_ui::Page::default()
        })
        .map_err(|_| Error::Internal)?;
        let mut params = self.filters.to_params();
        params.extend(self.board.to_params());
        Ok(LiveBody {
            html: html.as_str().to_owned(),
            query: params,
            ..LiveBody::default()
        })
    }

    async fn handle(&mut self, scope: &Scope, event: &serde_json::Value) -> Result<()> {
        let get = |key: &str| event.get(key).and_then(|value| value.as_str());
        match get("type") {
            Some("filters") => self.filters.apply_filters(
                &bilibi_ui::params::form_values_checked(
                    event,
                    FILTERS_FORM,
                    bilibi_ui::params::FIELD_LIMIT,
                )
                .map_err(Error::refused)?,
            ),
            Some(board::MORE) => {
                let column = board::column_key(event).ok_or(Error::Invalid)?;
                if STATUSES.iter().any(|(key, _, _)| *key == column) {
                    self.board.show_more(column);
                }
            }
            Some(board::MOVE_OPEN) => {
                let client = self.db.get().await?;
                if authorize_key(&**client, scope, UPDATE).await.is_ok() {
                    self.moving = board::card_id(event).map(str::to_owned);
                    self.move_errors.clear();
                    self.notice = None;
                } else {
                    self.notice = Some((
                        StatusKind::Error,
                        "You do not have permission to move appointments.".into(),
                    ));
                }
            }
            Some(board::MOVE_CANCEL) => {
                self.moving = None;
                self.move_errors.clear();
            }
            Some(name @ (board::MOVE | board::MOVE_SUBMIT)) => match BoardMove::from_event(event) {
                Err(sentence) if name == board::MOVE_SUBMIT => self.move_errors = vec![sentence],
                Err(sentence) => self.notice = Some((StatusKind::Error, sentence)),
                Ok(change) => {
                    match self.schedule.set_status(&self.db, scope, &change).await {
                        Ok(message) => self.notice = Some((StatusKind::Success, message)),
                        Err(message) => self.notice = Some((StatusKind::Error, message)),
                    }
                    self.moving = None;
                    self.move_errors.clear();
                }
            },
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}

/// The mounted board and the query it was last given.
struct BoardLive {
    page: BoardPage,
    query: HashMap<String, String>,
}

impl LiveState<Scope, Error> for BoardLive {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.query = values.clone();
                    self.page.filters = BoardPage::parse(values)?;
                    self.page.notice = None;
                }
                LiveInput::Event(event) => self.page.handle(request.scope, event).await?,
            }
            let query = self.query.clone();
            self.page.body(request.scope, &query).await
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let body = if request.operation == "board" {
                self.board(request.query)?
                    .page
                    .body(scope, request.query)
                    .await?
            } else {
                self.page(request.query).body(scope).await?
            };
            Ok(PageBody {
                html: body.html,
                live: true,
                title: None,
            })
        })
    }
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(if operation == "board" {
            "Schedule board"
        } else {
            "Schedule"
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = if request.operation == "board" {
                Box::new(self.board(request.query)?)
            } else {
                Box::new(self.page(request.query))
            };
            Ok(Some(state))
        })
    }
}

impl Pages {
    fn page(&self, query: &HashMap<String, String>) -> CalendarPage {
        CalendarPage {
            db: self.db.clone(),
            display: self.display.clone(),
            schedule: self.schedule.clone(),
            query: query.clone(),
            browser_zone: None,
            moving: None,
            move_errors: Vec::new(),
            notice: None,
        }
    }

    fn board(&self, query: &HashMap<String, String>) -> Result<BoardLive> {
        Ok(BoardLive {
            page: BoardPage {
                db: self.db.clone(),
                display: self.display.clone(),
                schedule: self.schedule.clone(),
                filters: BoardPage::parse(query)?,
                board: BoardState::default(),
                moving: None,
                move_errors: Vec::new(),
                notice: None,
            },
            query: query.clone(),
        })
    }
}

inventory::submit! {
    Module {id: MODULE, layer: Layer::Extensions,
        dependencies: &["base/ui", "base/database", "base/authz", "base/datetime"], migrations: &[], services: &[Service {
            id: PAGES,
            factory: |services| {
                Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Pages {
                    db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                    display: services.get("base/datetime.display")?,
                    schedule: Arc::new(Schedule::seeded()),
                }))))
            },
        }],
    }
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/schedule_reference.calendar", provenance!(), &Route {
        company_scope: CompanyScope::Tenant,
        method: Method::Get, path: PATH, service: PAGES, operation: "calendar",
        access: Access::Capability(VIEW), framed: true,
    })
}

inventory::submit! {
    Contribution::new(ROUTES, "extensions/schedule_reference.board", provenance!(), &Route {
        company_scope: CompanyScope::Tenant,
        method: Method::Get, path: BOARD_PATH, service: PAGES, operation: "board",
        access: Access::Capability(VIEW), framed: true,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let app = bilibi_web::App::new(db, &origin)?;
    // The fixture manager and viewer roles gain this example's capabilities
    // through their contributed extensions once the system roles are reconciled.
    app.services
        .get::<bilibi_authz::grants::Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("Schedule reference listening at {origin}{PATH} and {origin}{BOARD_PATH}");
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .map_err(|_| Error::Internal)
}
