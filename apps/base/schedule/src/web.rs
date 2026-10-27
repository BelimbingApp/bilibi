//! The `/system/schedule` operator board (Bilimbi `Schedule.Web.IndexLive`).
//!
//! Contributions supply defaults; operators can set a fixed clock time and zone.
//! The board reads tasks, run
//! history and diagnostics through the Schedule API and polls them on the host's
//! refresh. `can_execute`/`can_manage` only choose which controls to draw. Every
//! command re-asks Authz before it acts (`authorize_operator`, the same check
//! the Schedule API repeats inside its transaction), and a refusal flashes the
//! denial and redraws the controls instead of clearing the page.
//!
//! Enable, Pause and Disable confirm through the shared dialog. The request
//! holds the task whose consequence the dialog states and the confirmation
//! acts on that held task, never on a client-supplied key; a confirmation with
//! nothing held is a stale click and does nothing. Resume runs on click.
//!
//! History dates bound calendar days in the zone the Started column displays
//! in: the company, browser-local or UTC zone, named under the controls.
use crate::{
    board::{self, Availability, Diagnostics, DueWork, RunQuery, RunSort, TaskSort},
    phrase, timing, Claimed, Refusal, ReviewState, Run, Schedule, Task, Trigger, EXECUTE, MANAGE,
};
use askama::Template;
use bilibi_authz::{authorize_operator, effective_capabilities};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Mode, Precision};
use bilibi_locale::Locales;
use bilibi_settings::Settings;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    forms::{Field, InputKind},
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, ColumnType,
    ColumnsOutcome, CommitStates, CommitStatus, ConfirmDialog, ErrorSummary, Filter, FilterToolbar,
    FlashMap, FlexRow, FlexTable, Form, Html, InlineForm, ListColumns, ListSpec, ListState,
    PageCounts, Pagination, RowMode, SortDir, StatusKind, Tab, TableEmpty, Tabs, ToolbarControl,
    NO_FLASH,
};
use jiff::{civil::Date, tz::TimeZone, Timestamp};
use serde_json::Value;
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission for that action.";
const TABS: [(&str, &str); 3] = [
    ("tasks", "Tasks"),
    ("history", "History"),
    ("settings", "Settings"),
];

fn spec(
    sortable: &[(&str, SortDir)],
    default: &str,
    names: [&str; 4],
    size: &str,
    filters: Vec<(&str, Filter)>,
) -> Arc<ListSpec> {
    ListSpec {
        sortable: sortable
            .iter()
            .map(|(key, dir)| ((*key).into(), *dir))
            .collect(),
        default_sort: Some(default.into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: size.into(),
        param_names: bilibi_ui::list_state::ParamNames {
            search: names[0].into(),
            page: names[1].into(),
            sort_by: names[2].into(),
            sort_dir: names[3].into(),
        },
        filters: filters
            .into_iter()
            .map(|(key, filter)| (key.into(), filter))
            .collect(),
        ..ListSpec::default()
    }
    .shared()
    .expect("schedule board list spec")
}
/// Bilimbi's query names: tasks are not paged, so their page keys never reach the URL.
static TASKS: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    let mut statuses = vec![""];
    statuses.extend(board::TASK_STATUSES);
    spec(
        &[
            ("name", SortDir::Asc),
            ("next_due", SortDir::Asc),
            ("last_run", SortDir::Asc),
        ],
        "next_due",
        ["task_search", "task_page", "task_sort", "task_dir"],
        "task_page_size",
        vec![("task_status", Filter::one_of(&statuses, ""))],
    )
});
static RUNS: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    let mut statuses = vec![""];
    statuses.extend(board::RUN_STATUSES);
    spec(
        &[
            ("started_at", SortDir::Desc),
            ("name", SortDir::Asc),
            ("source", SortDir::Asc),
            ("status", SortDir::Asc),
        ],
        "started_at",
        ["run_search", "page", "run_sort", "run_dir"],
        "page_size",
        vec![
            ("run_status", Filter::one_of(&statuses, "")),
            ("start_date", Filter::string("")),
            ("end_date", Filter::string("")),
        ],
    )
});

/// The board's own columns (Bilimbi `@task_columns`/`@run_columns`): a sortable
/// one keeps the source's sort key and button id, so the heading raises the
/// same intent the plain table did.
fn builtin(id: &str, label: &str, kind: ColumnType, sort: Option<(&str, &str)>) -> BuiltinColumn {
    let mut column = BuiltinColumn::new(id, label);
    column.kind = kind;
    if let Some((key, table)) = sort {
        column = column.sortable(key);
        column.sort_id = Some(format!("{table}-sort-{key}"));
    }
    column
}
fn task_builtins() -> Vec<BuiltinColumn> {
    let sortable = |id, label, kind| builtin(id, label, kind, Some((id, "schedule-tasks")));
    vec![
        sortable("name", "Name", ColumnType::String),
        builtin("source", "Source", ColumnType::String, None),
        builtin("cron", "Schedule", ColumnType::String, None),
        sortable("next_due", "Next run", ColumnType::Datetime),
        builtin("status", "Status", ColumnType::String, None),
        sortable("last_run", "Last run", ColumnType::Datetime),
        builtin("result", "Result", ColumnType::String, None),
    ]
}
fn run_builtins() -> Vec<BuiltinColumn> {
    let sortable = |id, label, kind| builtin(id, label, kind, Some((id, "schedule-runs")));
    vec![
        sortable("started_at", "Started", ColumnType::Datetime),
        sortable("name", "Name", ColumnType::String),
        sortable("source", "Source", ColumnType::String),
        sortable("status", "Status", ColumnType::String),
        builtin("detail", "Detail", ColumnType::String, None),
    ]
}
fn columns(id: &str, builtins: Vec<BuiltinColumn>) -> Result<ListColumns> {
    ListColumns::mount(id, builtins).map_err(|_| Error::Internal)
}

struct Core {
    db: Db,
    schedule: Arc<Schedule>,
    settings: Arc<Settings>,
    display: Arc<DisplayPolicy>,
    locales: Arc<Locales>,
}
pub(crate) struct Pages(Arc<Core>);
impl Pages {
    pub fn new(
        db: Db,
        schedule: Arc<Schedule>,
        settings: Arc<Settings>,
        display: Arc<DisplayPolicy>,
        locales: Arc<Locales>,
    ) -> Self {
        Self(Arc::new(Core {
            db,
            schedule,
            settings,
            display,
            locales,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Command {
    Enable,
    Pause,
    Disable,
    ResetTime,
}
impl Command {
    fn parse(event: &str) -> Option<Self> {
        match event {
            "request_enable" | "enable" => Some(Self::Enable),
            "request_pause" | "pause" => Some(Self::Pause),
            "request_disable" | "disable" => Some(Self::Disable),
            "request_reset_time" | "reset_time" => Some(Self::ResetTime),
            _ => None,
        }
    }
    fn event(self) -> &'static str {
        match self {
            Self::Enable => "enable",
            Self::Pause => "pause",
            Self::Disable => "disable",
            Self::ResetTime => "reset_time",
        }
    }
    fn verb(self) -> &'static str {
        match self {
            Self::Enable => "Enable",
            Self::Pause => "Pause",
            Self::Disable => "Disable",
            Self::ResetTime => "Reset to code default",
        }
    }
    fn working(self) -> &'static str {
        match self {
            Self::Enable => "Enabling…",
            Self::Pause => "Pausing…",
            Self::Disable => "Disabling…",
            Self::ResetTime => "Resetting…",
        }
    }
    /// What the command sets running, or what it does to queued work, which
    /// resuming or re-enabling does not bring back.
    fn consequence(self, task: &Task) -> String {
        if self == Self::ResetTime {
            return format!("Task “{}” will use its code default time again.", task.name);
        }
        let done = match self {
            Self::Enable => "enabled",
            Self::Pause => "paused",
            Self::Disable => "disabled",
            Self::ResetTime => unreachable!(),
        };
        format!("Task “{}” will be {done}.", task.name)
    }
    fn detail(self, task: &Task) -> String {
        let print = &task.fingerprint[..12];
        match self {
            Self::Enable if task.suppressed => format!(
                "This version ({print}) is approved, but the task stays paused until you resume it. Any later change to the task needs approval again."
            ),
            Self::Enable => format!(
                "It will run on its schedule ({}, {}) using this version ({print}). Any later change to the task stops it until approved.",
                task.clock_expression, task.clock_timezone
            ),
            Self::Pause => "Runs already queued are cancelled. You can resume the task later.".into(),
            Self::Disable => "Runs already queued are cancelled, and it will not run again until it is reviewed and enabled.".into(),
            Self::ResetTime => format!("The default is {}. Runs already queued at the old time are cancelled. Its enabled and paused state stays the same.", phrase::show(task.expression,task.timezone,task.timezone,Timestamp::now()).words),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Loaded {
    Available,
    Unavailable,
    InvalidFilters,
}

struct TimeEditor {
    task: Task,
    zone: String,
    value: String,
}

struct Board {
    core: Arc<Core>,
    tab: &'static str,
    tasks: ListState,
    runs: ListState,
    /// Each table's column arrangement and row height, kept while mounted.
    task_columns: ListColumns,
    run_columns: ListColumns,
    /// Whether the rows are read anew for this render: the columns then forget
    /// an unfinished add box, as Bilimbi's `ListColumns.load/3` does.
    rows_changed: bool,
    browser_zone: Option<String>,
    /// A fresh server mount must obtain a report even if its DOM hook survives.
    timezone_request: String,
    pending: Option<(Command, Task)>,
    time_editor: Option<TimeEditor>,
    time_outcomes: CommitStates,
    viewer_zone: Option<String>,
    flash: Option<(StatusKind, String)>,
}
impl Board {
    fn new(core: Arc<Core>, query: &HashMap<String, String>) -> Result<Self> {
        let (tab, tasks, runs) = parse(query)?;
        Ok(Self {
            core,
            tab,
            tasks,
            runs,
            task_columns: columns("schedule-tasks", task_builtins())?
                .with_record_fields(task_fields::ALL.iter().copied()),
            run_columns: columns("schedule-runs", run_builtins())?
                .with_record_fields(run_fields::ALL.iter().copied()),
            rows_changed: true,
            browser_zone: None,
            timezone_request: uuid::Uuid::new_v4().to_string(),
            pending: None,
            time_editor: None,
            time_outcomes: CommitStates::default(),
            viewer_zone: None,
            flash: None,
        })
    }
}

fn parse(query: &HashMap<String, String>) -> Result<(&'static str, ListState, ListState)> {
    let tab = TABS
        .iter()
        .find(|(id, _)| query.get("tab").map(String::as_str) == Some(*id))
        .map_or("tasks", |(id, _)| *id);
    let tasks = ListState::parse(query, TASKS.clone()).map_err(|_| Error::Internal)?;
    let mut runs = ListState::parse(query, RUNS.clone()).map_err(|_| Error::Internal)?;
    normalize_dates(&mut runs);
    Ok((tab, tasks, runs))
}
/// A date the browser cannot have produced is dropped, as Bilimbi's `date_param`.
fn normalize_dates(runs: &mut ListState) {
    for key in ["start_date", "end_date"] {
        if let Some(value) = runs.filters.get_mut(key) {
            if date(value).is_none() {
                value.clear();
            }
        }
    }
}
fn date(value: &str) -> Option<Date> {
    (value.len() == 10).then(|| value.parse().ok()).flatten()
}

impl Board {
    fn query(&self) -> HashMap<String, String> {
        let mut query = self.tasks.to_params();
        query.remove("task_page");
        query.remove("task_page_size");
        query.extend(self.runs.to_params());
        query.insert("tab".into(), self.tab.into());
        query
    }
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    /// One command's outcome. A refusal is a flash, never a page-clearing error.
    fn outcome(&mut self, result: Result<()>, success: &str) -> Result<()> {
        match result {
            Ok(()) => self.flash(StatusKind::Success, success),
            Err(Error::Forbidden) => self.flash(StatusKind::Error, DENIED),
            Err(Error::Conflict) => self.flash(
                StatusKind::Error,
                "This schedule changed. Reopen the action and try again.",
            ),
            Err(Error::NotFound) => self.flash(
                StatusKind::Error,
                "That schedule definition is no longer installed.",
            ),
            Err(Error::Unauthorized) => return Err(Error::Unauthorized),
            Err(_) => self.flash(
                StatusKind::Error,
                "Schedule state is unavailable; no action was confirmed.",
            ),
        }
        Ok(())
    }
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        let client = self.core.db.get().await?;
        match authorize_operator(&**client, scope, capability).await {
            Ok(_) => Ok(true),
            Err(Error::Forbidden) => Ok(false),
            Err(error) => Err(error),
        }
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let key = get("key").unwrap_or_default().to_owned();
        let schedule = self.core.schedule.clone();
        match get("type").ok_or(Error::Invalid)? {
            "browser_timezone" => {
                self.browser_zone = Some(
                    get("timezone")
                        .filter(|zone| zone.len() <= 64 && bilibi_datetime::valid_timezone(zone))
                        .unwrap_or("Etc/UTC")
                        .to_owned(),
                );
            }
            "filter_tasks" => {
                let posted = params::form_values_checked(value, "task", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let mut mapped = HashMap::new();
                if let Some(search) = posted.get("search") {
                    mapped.insert("search".to_owned(), search.clone());
                }
                if let Some(status) = posted.get("status") {
                    mapped.insert("task_status".to_owned(), status.clone());
                }
                self.tasks.apply_filters(&mapped);
            }
            "sort_tasks" => {
                self.tasks.next_sort(get("sort").unwrap_or_default());
            }
            "grid_tasks" => match self.task_columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(column) => {
                    self.tasks.next_sort(&column);
                }
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "filter_runs" => {
                self.runs.apply_filters(
                    &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                );
                normalize_dates(&mut self.runs);
            }
            "sort_runs" => {
                self.runs.next_sort(get("sort").unwrap_or_default());
            }
            "grid_runs" => match self.run_columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(column) => {
                    self.runs.next_sort(&column);
                }
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "page" => self.runs.put_page(get("page")),
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| Some(kind.name()) == get("key"))
                {
                    self.flash = None;
                }
            }
            "edit_time" => {
                if !self.allowed(scope, MANAGE).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let Some(zone) = self.viewer_zone.clone() else {
                    return Ok(());
                };
                let client = self.core.db.get().await?;
                let task = schedule
                    .tasks(&**client, scope, Timestamp::now())
                    .await?
                    .into_iter()
                    .find(|task| task.key == key)
                    .ok_or(Error::NotFound)?;
                let Some(expression) = phrase::editable(
                    &task.clock_expression,
                    &task.clock_timezone,
                    &zone,
                    Timestamp::now(),
                ) else {
                    return Ok(());
                };
                let cron = crate::cron::Cron::parse(&expression).map_err(|_| Error::Invalid)?;
                let time = format!(
                    "{:02}:{:02}",
                    cron.hour_values()[0],
                    cron.minute_values()[0]
                );
                self.time_editor = Some(TimeEditor {
                    task,
                    zone,
                    value: time,
                });
            }
            "cancel_time" => self.time_editor = None,
            "save_time" => {
                if !self.allowed(scope, MANAGE).await? {
                    self.time_editor = None;
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let Some(editor) = &mut self.time_editor else {
                    return Ok(());
                };
                editor.value = params::form_values_checked(value, "timing", params::FIELD_LIMIT)
                    .map_err(Error::refused)?
                    .get("time")
                    .cloned()
                    .unwrap_or_default();
                let mut client = self.core.db.get().await?;
                let result = schedule
                    .set_time(
                        &mut client,
                        scope,
                        editor.task.key,
                        &editor.task.timing_version,
                        &editor.value,
                        &editor.zone,
                    )
                    .await;
                let status = match result {
                    Ok(()) => CommitStatus::Saved,
                    Err(Error::Invalid) => {
                        CommitStatus::Error("Time must be a valid hour and minute.".into())
                    }
                    Err(Error::Conflict) => CommitStatus::Error(
                        "This schedule changed. Cancel and reopen the time picker.".into(),
                    ),
                    Err(Error::Forbidden) => CommitStatus::Error(DENIED.into()),
                    Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                    Err(_) => CommitStatus::failure(),
                };
                let saved = status == CommitStatus::Saved;
                self.time_outcomes.put(editor.task.key, status);
                if saved {
                    self.time_editor = None;
                    self.flash(StatusKind::Success, "Task time saved.");
                }
            }
            "run_now" => {
                if !self.allowed(scope, EXECUTE).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let mut client = self.core.db.get().await?;
                let result = schedule
                    .run_now(&mut client, scope, &key, Timestamp::now())
                    .await;
                match result {
                    Ok(Ok(Claimed { .. })) => self.flash(StatusKind::Success, "Run queued."),
                    Ok(Err(refusal)) => self.flash(StatusKind::Error, refusal_message(refusal)),
                    Err(error) => self.outcome(Err(error), "")?,
                }
            }
            "resume" => {
                if !self.allowed(scope, MANAGE).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let mut client = self.core.db.get().await?;
                let result = schedule.resume(&mut client, scope, &key).await.map(drop);
                self.outcome(result, "Task resumed.")?;
            }
            "cancel_command" => self.pending = None,
            event @ ("request_enable" | "request_pause" | "request_disable"
            | "request_reset_time") => {
                let command = Command::parse(event).ok_or(Error::Invalid)?;
                if !self.allowed(scope, MANAGE).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let client = self.core.db.get().await?;
                match schedule.tasks(&**client, scope, Timestamp::now()).await {
                    Ok(tasks) => match tasks.into_iter().find(|task| task.key == key) {
                        Some(task) => {
                            self.flash = None;
                            self.pending = Some((command, task));
                        }
                        None => self.outcome(Err(Error::NotFound), "")?,
                    },
                    Err(error) => self.outcome(Err(error), "")?,
                }
            }
            event @ ("enable" | "pause" | "disable" | "reset_time") => {
                let command = Command::parse(event).ok_or(Error::Invalid)?;
                // The refusal comes first, so a revoked operator hears it even
                // from a forged event.
                if !self.allowed(scope, MANAGE).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let Some((held, task)) = self.pending.take() else {
                    return Ok(());
                };
                if held != command {
                    self.pending = Some((held, task));
                    return Ok(());
                }
                let mut client = self.core.db.get().await?;
                let (result, success) = match command {
                    Command::Enable => (
                        schedule.review(&mut client, scope, task.key, true).await,
                        "Task enabled.",
                    ),
                    Command::Disable => (
                        schedule.review(&mut client, scope, task.key, false).await,
                        "Task disabled.",
                    ),
                    Command::ResetTime => (
                        schedule
                            .reset_time(&mut client, scope, task.key, &task.timing_version)
                            .await,
                        "Task time reset to code default.",
                    ),
                    Command::Pause => (
                        schedule
                            .suppress(&mut client, scope, task.key)
                            .await
                            .map(drop),
                        "Task paused.",
                    ),
                };
                if result.is_ok() && command == Command::ResetTime {
                    if self
                        .time_editor
                        .as_ref()
                        .is_some_and(|editor| editor.task.key == task.key)
                    {
                        self.time_editor = None;
                    }
                    self.time_outcomes.put(task.key, CommitStatus::Saved);
                }
                self.outcome(result, success)?;
            }
            "save_retention" => {
                if !self.allowed(scope, MANAGE).await? {
                    self.flash(StatusKind::Error, DENIED);
                    return Ok(());
                }
                let days = params::form_values_checked(value, "retention", params::FIELD_LIMIT)
                    .map_err(Error::refused)?
                    .get("days")
                    .and_then(|days| days.trim().parse::<i64>().ok())
                    .filter(|days| (board::KEEP_DAYS_MIN..=board::KEEP_DAYS_MAX).contains(days));
                let Some(days) = days else {
                    self.flash(StatusKind::Error, retention_bounds());
                    return Ok(());
                };
                let mut client = self.core.db.get().await?;
                let result =
                    board::set_keep_days(&schedule, &self.core.settings, &mut client, scope, days)
                        .await;
                self.outcome(result, "Retention saved.")?;
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let core = self.core.clone();
        let client = core.db.get().await?;
        let now = Timestamp::now();
        let locale = core.locales.resolve(&**client, scope, None).await?.locale;
        let display = core.display.display(&**client, scope, None, locale).await?;
        let capabilities = effective_capabilities(&**client, scope).await?;
        let can_execute = capabilities.contains(EXECUTE);
        let can_manage = capabilities.contains(MANAGE);
        let all_tasks = match core.schedule.tasks(&**client, scope, now).await {
            Ok(tasks) => Some(tasks),
            Err(Error::Unauthorized) => return Err(Error::Unauthorized),
            Err(Error::Forbidden) => return Err(Error::Forbidden),
            Err(_) => None,
        };
        let diagnostics = core
            .schedule
            .diagnostics(&**client, all_tasks.as_deref(), now)
            .await;
        let zone_name = display.clock(self.browser_zone.as_deref(), now).timezone;
        self.viewer_zone =
            (display.mode != Mode::Local || self.browser_zone.is_some()).then(|| zone_name.clone());
        let mut body = Html::default();
        match self.tab {
            "tasks" => body.push(&self.tasks_panel(
                all_tasks.as_deref(),
                &display,
                (&zone_name, now),
                can_execute,
                can_manage,
            )?),
            "history" => {
                let zone = TimeZone::get(&zone_name).unwrap_or(TimeZone::UTC);
                body.push(
                    &self
                        .history_panel(&core, &client, scope, &display, &zone_name, zone)
                        .await?,
                )
            }
            _ => {
                let days = board::keep_days(&core.settings, &**client).await.ok();
                body.push(&retention_panel(days, can_manage)?);
            }
        }
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        // Each tab is the current address with its own `tab`, as Bilimbi's
        // `tab_path`, so a view can be linked and Back returns to it.
        let tab_paths: Vec<(String, String, &str, &str)> = TABS
            .iter()
            .map(|(id, label)| {
                let mut query = self.query();
                query.insert("tab".into(), (*id).into());
                (
                    format!("schedule-tab-{id}"),
                    params::address("/system/schedule", &query),
                    *id,
                    *label,
                )
            })
            .collect();
        let tabs = render(Tabs {
            id: "schedule-tabs",
            label: "Schedule views",
            class: "mt-5",
            tabs: tab_paths
                .iter()
                .map(|(dom, href, id, label)| Tab {
                    id: dom,
                    ..Tab::patch(href, label, self.tab == *id)
                })
                .collect(),
        })?;
        let dialog = match &self.pending {
            Some((command, task)) => {
                let (consequence, detail) = (command.consequence(task), command.detail(task));
                render(ConfirmDialog {
                    id: "schedule-command-confirm",
                    consequence: &consequence,
                    detail: &detail,
                    confirm: command.verb(),
                    working: command.working(),
                    confirm_event: command.event(),
                    cancel_event: "cancel_command",
                })?
            }
            None => Html::default(),
        };
        BoardView {
            flash,
            diagnostics: diagnostic_cards(&diagnostics),
            tabs,
            body,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn tasks_panel(
        &mut self,
        tasks: Option<&[Task]>,
        display: &DisplayContext,
        (viewer_zone, now): (&str, Timestamp),
        can_execute: bool,
        can_manage: bool,
    ) -> Result<Html> {
        let status = self.tasks.filters["task_status"].clone();
        let sort = match self.tasks.sort_by.as_deref() {
            Some("name") => TaskSort::Name,
            Some("last_run") => TaskSort::LastRun,
            _ => TaskSort::NextDue,
        };
        let shown = tasks.map(|tasks| {
            board::filter_tasks(
                tasks.to_vec(),
                &self.tasks.search,
                (!status.is_empty()).then_some(status.as_str()),
                sort,
                self.tasks.sort_dir == Some(SortDir::Desc),
            )
        });
        let search = Field::in_form("task", "search")
            .id("schedule-task-search")
            .kind(InputKind::Search)
            .label("Search task name")
            .label_class("sr-only")
            .wrapper_class("mb-0")
            .placeholder("Search task name…")
            .attr("maxlength", "255")
            .attr("data-debounce", "300")
            .autocomplete("off")
            .value(self.tasks.search.clone())
            .class("w-full rounded-md border border-line bg-surface px-3 py-1.5 text-sm text-ink focus:border-brand-strong focus:outline-none focus:ring-2 focus:ring-brand-strong/30");
        let select = Field::in_form("task", "status")
            .id("schedule-task-status")
            .kind(InputKind::Select)
            .label("Filter task status")
            .label_class("sr-only")
            .wrapper_class("mb-0")
            .value(status.clone())
            .options(choices(&[
                ("All task statuses", ""),
                ("Never run", "never"),
                ("Running", "running"),
                ("Succeeded", "succeeded"),
                ("Failed", "failed"),
                ("Skipped", "skipped"),
                ("Paused", "paused"),
                ("Unreviewed", "unreviewed"),
                ("Disabled", "disabled"),
            ]))
            .class("rounded-md border border-line bg-surface px-3 py-1.5 text-sm text-ink focus:border-brand-strong focus:outline-none");
        if self.rows_changed {
            self.task_columns
                .load(shown.as_deref().unwrap_or_default(), |task| {
                    task.key.to_owned()
                });
        }
        let compact = self.task_columns.mode() == RowMode::Compact;
        let mut rows = Vec::new();
        for task in shown.iter().flatten() {
            let dom = format!("schedule-task-{}", dom_key(task.key));
            let next = instant(display, format!("{dom}-next-due"), task.next_due_at);
            let last = instant(display, format!("{dom}-last-started"), task.last_started_at);
            let label = task_status_label(task);
            let result = status_label(task.last_status.as_deref().unwrap_or("never"));
            let overlap = format!(
                "{} overlap · {} misfires",
                task.overlap.as_str(),
                task.misfire.as_str()
            );
            let duration = duration(task.last_runtime_ms);
            let schedule = phrase::show(
                &task.clock_expression,
                &task.clock_timezone,
                viewer_zone,
                now,
            );
            let cells = [
                (
                    "name",
                    Cell::TaskName {
                        name: task.name,
                        key: task.key,
                    },
                ),
                (
                    "source",
                    Cell::Source {
                        owner: task.owner,
                        route: task.owner_route,
                    },
                ),
                (
                    "cron",
                    Cell::Schedule {
                        words: &schedule.words,
                        tooltip: &schedule.tooltip,
                    },
                ),
                ("next_due", Cell::Instant(&next)),
                (
                    "status",
                    Cell::Status {
                        kind: task_status_kind(task),
                        label: &label,
                        detail: &overlap,
                    },
                ),
                ("last_run", Cell::Instant(&last)),
                (
                    "result",
                    Cell::Result {
                        label: &result,
                        detail: &duration,
                    },
                ),
            ];
            let mut row = FlexRow::new(task.key);
            row.dom_id = Some(dom.clone());
            for (id, content) in cells {
                row = row.slot(id, cell(content, compact)?);
            }
            row = row
                .slot("overlap", Html::text(task.overlap.as_str()))
                .slot("misfire", Html::text(task.misfire.as_str()))
                .slot("last_runtime", Html::text(&duration));
            if can_manage
                && self.viewer_zone.is_some()
                && timing::fixed(task.expression)
                && phrase::editable(
                    &task.clock_expression,
                    &task.clock_timezone,
                    viewer_zone,
                    now,
                )
                .is_some()
            {
                row = row.slot(
                    "cron",
                    self.time_cell(task, &dom, &schedule.words, compact)?,
                );
            }
            row.actions = actions(task, &dom, can_execute, can_manage)?;
            rows.push(row);
        }
        let mut table = FlexTable::new("schedule-tasks", self.task_columns.flex_columns(), rows)
            .sorted(self.tasks.sort_by.as_deref(), self.tasks.sort_dir);
        table.zoom = self.task_columns.zoom;
        table.suggestions = self.task_columns.flex_suggestions();
        table.add_query = &self.task_columns.add_query;
        table.event = "grid_tasks";
        table.caption = "Scheduled tasks";
        table.framed = false;
        table.actions = true;
        if shown.as_ref().is_some_and(Vec::is_empty) {
            let text = if self.tasks.search.is_empty() && status.is_empty() {
                "Nothing is scheduled."
            } else {
                "No tasks match the current filters."
            };
            table.empty = Some(TableEmpty::Content(Html::text(text)));
        }
        let unavailable = if tasks.is_none() {
            render(Alert {
                kind: StatusKind::Error,
                id: "schedule-tasks-unavailable",
                class: "mt-4",
                body: Html::text("Scheduled tasks are unavailable."),
            })?
        } else {
            Html::default()
        };
        let card = render(Card {
            id: "schedule-tasks-card",
            class: "mt-4",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?;
        render(TasksPanel {
            timezone_request: if self.browser_zone.is_none() {
                self.timezone_request.clone()
            } else {
                String::new()
            },
            search,
            status: select,
            unavailable,
            card,
        })
    }

    fn time_cell(&self, task: &Task, dom: &str, words: &str, compact: bool) -> Result<Html> {
        let editor = self
            .time_editor
            .as_ref()
            .filter(|editor| editor.task.key == task.key);
        let mut form = Html::default();
        let status = self.time_outcomes.get(task.key);
        if let Some(editor) = editor {
            let field_id = format!("{dom}-time-input");
            let mut field = Field::in_form("timing", "time")
                .id(&field_id)
                .kind(InputKind::Time)
                .label("Time")
                .value(&editor.value)
                .required()
                .wrapper_class("mb-0")
                .attr("step", "60")
                .hint(format!("Time zone: {}", editor.zone));
            let mut errors = ErrorSummary::new(format!("{dom}-time-errors"));
            if let CommitStatus::Error(message) = &status {
                field.errors.push(message.clone());
                errors.push(&field_id, message);
            }
            let mut body = render(errors)?;
            body.push(&render(TimeField { field })?);
            let mut actions = render(Button {
                button_type: "submit",
                working: "Saving…",
                variant: ButtonVariant::Primary,
                body: Html::text("Save time"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                button_type: "button",
                event: "cancel_time",
                body: Html::text("Cancel"),
                ..Button::default()
            })?);
            form = render(Form {
                id: &format!("{dom}-time-form"),
                event: "save_time",
                body,
                actions,
                ..Form::default()
            })?;
        }
        render(InlineForm {
            compact,
            id: format!("{dom}-time"),
            key: task.key.into(),
            label: format!("time for {}", task.name),
            value: words.to_owned(),
            hint: if task.time_overridden {
                format!("Saved time zone: {}", task.clock_timezone)
            } else {
                String::new()
            },
            editing: editor.is_some(),
            open_event: "edit_time".into(),
            cancel_event: "cancel_time".into(),
            form,
            status,
        })
    }

    async fn history_panel(
        &mut self,
        core: &Core,
        client: &tokio_postgres::Client,
        scope: &Scope,
        display: &DisplayContext,
        zone_name: &str,
        zone: TimeZone,
    ) -> Result<Html> {
        let filters = self.runs.filters.clone();
        let start = date(&filters["start_date"]);
        let end = date(&filters["end_date"]);
        let sort = match self.runs.sort_by.as_deref() {
            Some("name") => RunSort::Name,
            Some("source") => RunSort::Source,
            Some("status") => RunSort::Status,
            _ => RunSort::StartedAt,
        };
        let status = filters["run_status"].clone();
        let mut query = RunQuery {
            search: self.runs.search.clone(),
            status: (!status.is_empty()).then(|| status.clone()),
            start,
            end,
            zone,
            sort,
            descending: self.runs.sort_dir == Some(SortDir::Desc),
            page: self.runs.page,
            page_size: self.runs.page_size,
        };
        let mut loaded = Loaded::Available;
        let mut page = match core.schedule.run_page(client, scope, &query).await {
            Ok(page) => Some(page),
            Err(Error::Invalid) => {
                loaded = Loaded::InvalidFilters;
                None
            }
            Err(Error::Forbidden) => return Err(Error::Forbidden),
            Err(_) => {
                loaded = Loaded::Unavailable;
                None
            }
        };
        // A page past the end (rows were pruned, or a stale URL) shows the last one.
        if let Some(found) = &page {
            let pages = found.total.div_ceil(self.runs.page_size);
            // Local-mode UTC bounds are provisional until the browser reports.
            // Do not destroy a bookmarked page that exists in the local day.
            if (display.mode != Mode::Local || self.browser_zone.is_some())
                && self.runs.clamp_to_last_page(pages, false)
            {
                query.page = self.runs.page;
                page = core.schedule.run_page(client, scope, &query).await.ok();
                if page.is_none() {
                    loaded = Loaded::Unavailable;
                }
            }
        }
        let form = self.runs.filters_form();
        let toolbar = render(FilterToolbar {
            event: "filter_runs",
            class: "mt-4",
            ..FilterToolbar::new(
                "schedule-history-filters",
                vec![
                    ToolbarControl::search(
                        "schedule-run-search",
                        "search",
                        "Search run name",
                        &form["search"],
                        "Search run name…",
                    ),
                    ToolbarControl::select(
                        "schedule-run-status",
                        "run_status",
                        "Filter run status",
                        &form["run_status"],
                        choices(&[
                            ("All run statuses", ""),
                            ("Running", "running"),
                            ("Succeeded", "succeeded"),
                            ("Failed", "failed"),
                            ("Skipped", "skipped"),
                        ]),
                    ),
                    ToolbarControl::date(
                        "schedule-run-start-date",
                        "start_date",
                        "Start date",
                        &form["start_date"],
                    )
                    .hint(zone_name),
                    ToolbarControl::date(
                        "schedule-run-end-date",
                        "end_date",
                        "End date",
                        &form["end_date"],
                    )
                    .hint(zone_name),
                ],
            )
        })?;
        let total = page.as_ref().map_or(0, |page| page.total);
        let entries: &[Run] = page.as_ref().map_or(&[], |page| page.entries.as_slice());
        if self.rows_changed {
            self.run_columns.load(entries, |run| run.id.to_string());
        }
        let compact = self.run_columns.mode() == RowMode::Compact;
        let mut rows = Vec::with_capacity(entries.len());
        for run in entries {
            let dom = format!("schedule-run-{}", run.id);
            let started = instant(display, format!("{dom}-started-at"), Some(run.started_at));
            let runtime = duration(run.runtime_ms);
            let status = status_label(&run.status);
            let trigger = trigger_label(run);
            let cells = [
                (
                    "started_at",
                    Cell::Started {
                        at: &started,
                        detail: &runtime,
                    },
                ),
                (
                    "name",
                    Cell::TaskName {
                        name: &run.name,
                        key: &run.key,
                    },
                ),
                ("source", Cell::RunSource { detail: &trigger }),
                (
                    "status",
                    Cell::Badge {
                        kind: status_kind(&run.status),
                        label: &status,
                    },
                ),
                ("detail", Cell::Detail(&status)),
            ];
            let mut row = FlexRow::new(run.id.to_string());
            row.dom_id = Some(dom);
            for (id, content) in cells {
                row = row.slot(id, cell(content, compact)?);
            }
            row = row
                .slot("runtime", Html::text(&runtime))
                .slot("trigger", Html::text(&trigger));
            rows.push(row);
        }
        let mut table = FlexTable::new("schedule-runs", self.run_columns.flex_columns(), rows)
            .sorted(self.runs.sort_by.as_deref(), self.runs.sort_dir);
        table.zoom = self.run_columns.zoom;
        table.suggestions = self.run_columns.flex_suggestions();
        table.add_query = &self.run_columns.add_query;
        table.event = "grid_runs";
        table.caption = "Schedule run history";
        table.framed = false;
        if loaded == Loaded::Available && total == 0 {
            let unfiltered = self.runs.search.is_empty()
                && status.is_empty()
                && start.is_none()
                && end.is_none();
            table.empty = Some(TableEmpty::Content(Html::text(if unfiltered {
                "No runs recorded yet."
            } else {
                "No runs match the current filters."
            })));
        }
        let alert = match loaded {
            Loaded::Available => Html::default(),
            Loaded::InvalidFilters => render(Alert {
                kind: StatusKind::Warning,
                id: "schedule-history-invalid",
                class: "mt-4",
                body: Html::text("The start date must not be after the end date."),
            })?,
            Loaded::Unavailable => render(Alert {
                kind: StatusKind::Error,
                id: "schedule-history-unavailable",
                class: "mt-4",
                body: Html::text("Run history is unavailable."),
            })?,
        };
        let card = render(Card {
            id: "schedule-runs-card",
            class: "mt-4",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?;
        let pagination = if loaded == Loaded::Available {
            render(Pagination {
                filters_event: "filter_runs",
                ..Pagination::new(
                    "schedule-history-pagination",
                    PageCounts::new(self.runs.page, self.runs.page_size, total),
                    &PAGE_SIZES,
                )
            })?
        } else {
            Html::default()
        };
        render(HistoryPanel {
            timezone_request: if self.browser_zone.is_none() {
                self.timezone_request.clone()
            } else {
                String::new()
            },
            toolbar,
            alert,
            card,
            pagination,
        })
    }
}

fn retention_bounds() -> String {
    format!(
        "Retention must be a whole number from {} to {}.",
        board::KEEP_DAYS_MIN,
        board::KEEP_DAYS_MAX
    )
}
fn retention_panel(days: Option<i64>, can_manage: bool) -> Result<Html> {
    let current = match days {
        Some(0) => "Keep all recorded runs".to_owned(),
        Some(1) => "1 day".to_owned(),
        Some(days) => format!("{days} days"),
        None => "Unavailable".to_owned(),
    };
    let field = Field::in_form("retention", "days")
        .id("schedule-retention-days")
        .kind(InputKind::Number)
        .label("Days")
        .wrapper_class("mb-0 w-40")
        .attr("min", &board::KEEP_DAYS_MIN.to_string())
        .attr("max", &board::KEEP_DAYS_MAX.to_string())
        .required()
        .value(days.map(|d| d.to_string()).unwrap_or_default());
    let save = render(Button {
        variant: ButtonVariant::Primary,
        id: "schedule-retention-save",
        button_type: "submit",
        working: "Saving…",
        body: Html::text("Save retention"),
        ..Button::default()
    })?;
    let body = render(RetentionBody {
        current,
        can_manage,
        field,
        save,
    })?;
    render(Card {
        id: "schedule-retention",
        title: "Schedule history retention",
        inner_class: "p-4",
        body,
        ..Card::default()
    })
    .and_then(|card| render(SettingsPanel { card }))
}

fn actions(task: &Task, dom: &str, can_execute: bool, can_manage: bool) -> Result<Html> {
    let enabled = task.review == ReviewState::Enabled;
    let mut buttons = Html::default();
    let mut add = |shown: bool, suffix: &str, event: &str, label: &str| -> Result<()> {
        if shown {
            let id = format!("{dom}-{suffix}");
            buttons.push(&render(Button {
                id: &id,
                button_type: "button",
                event,
                name: "key",
                value: task.key,
                body: Html::text(label),
                ..Button::default()
            })?);
        }
        Ok(())
    };
    add(
        can_execute && enabled && !task.suppressed,
        "run",
        "run_now",
        "Run now",
    )?;
    add(can_manage && !enabled, "enable", "request_enable", "Enable")?;
    add(
        can_manage && enabled && !task.suppressed,
        "pause",
        "request_pause",
        "Pause",
    )?;
    add(
        can_manage && enabled && task.suppressed,
        "resume",
        "resume",
        "Resume",
    )?;
    add(
        can_manage && enabled,
        "disable",
        "request_disable",
        "Disable",
    )?;
    add(
        can_manage && task.time_overridden,
        "reset-time",
        "request_reset_time",
        "Reset to code default",
    )?;
    render(Actions { buttons })
}

fn refusal_message(refusal: Refusal) -> &'static str {
    match refusal {
        Refusal::Disabled => "Enable this definition before queuing it.",
        Refusal::Suppressed => "Resume this task before queuing it.",
        Refusal::Unreviewed => "Review and enable this definition before queuing it.",
        Refusal::Overlap => "This task already has an active occurrence.",
        Refusal::AlreadyClaimed => "Schedule state is unavailable; no action was confirmed.",
    }
}
fn status_label(status: &str) -> String {
    let mut chars = status.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}
fn task_status_label(task: &Task) -> String {
    if task.suppressed {
        "Paused".into()
    } else {
        match task.review {
            ReviewState::Unreviewed => "Unreviewed".into(),
            ReviewState::Disabled => "Disabled".into(),
            ReviewState::Enabled => status_label(task.last_status.as_deref().unwrap_or("never")),
        }
    }
}
fn status_kind(status: &str) -> BadgeKind {
    match status {
        "succeeded" | "available" => BadgeKind::Success,
        "failed" | "unavailable" => BadgeKind::Danger,
        "running" | "unreviewed" | "unknown" => BadgeKind::Warning,
        _ => BadgeKind::Neutral,
    }
}
fn task_status_kind(task: &Task) -> BadgeKind {
    match (task.suppressed, task.review) {
        (true, _) | (_, ReviewState::Unreviewed) => BadgeKind::Warning,
        (_, ReviewState::Disabled) => BadgeKind::Neutral,
        _ => status_kind(task.last_status.as_deref().unwrap_or("never")),
    }
}
// Bilimbi schedule board facts: a cell's second line is hidden in compact rows,
// so each scalar it carries is also a column.
bilibi_ui::record_fields! { mod task_fields {
    OVERLAP: bilibi_ui::RecordField::new("overlap", "Overlap policy"),
    MISFIRE: bilibi_ui::RecordField::new("misfire", "Misfire policy"),
    LAST_RUNTIME: bilibi_ui::RecordField::new("last_runtime", "Last runtime"),
} }
bilibi_ui::record_fields! { mod run_fields {
    RUNTIME: bilibi_ui::RecordField::new("runtime", "Runtime"),
    TRIGGER: bilibi_ui::RecordField::new("trigger", "Trigger"),
} }

fn duration(milliseconds: Option<i32>) -> String {
    match milliseconds {
        None => "—".into(),
        Some(ms) if ms < 1_000 => format!("{ms} ms"),
        Some(ms) => format!("{:.1} s", f64::from(ms) / 1_000.0),
    }
}
fn trigger_label(run: &Run) -> String {
    match (
        run.trigger,
        &run.triggered_by_name,
        run.triggered_by_user_id,
    ) {
        (Trigger::Manual, Some(name), _) => format!("Run now by {name}"),
        (Trigger::Manual, None, Some(id)) => format!("Run now by user {id}"),
        (Trigger::Manual, None, None) => "Run now".into(),
        (Trigger::Scheduled, ..) => "Scheduled".into(),
    }
}
fn dom_key(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

struct DiagnosticCard {
    id: &'static str,
    label: &'static str,
    kind: BadgeKind,
    text: &'static str,
}
fn diagnostic_cards(d: &Diagnostics) -> Vec<DiagnosticCard> {
    let available = |a: Availability| match a {
        Availability::Available => (BadgeKind::Success, "Available"),
        Availability::Unavailable => (BadgeKind::Danger, "Unavailable"),
    };
    let due = match d.due_work {
        DueWork::NoneDue => (BadgeKind::Success, "No work currently due"),
        DueWork::Due => (BadgeKind::Danger, "Work is due"),
        DueWork::Unknown => (BadgeKind::Warning, "Unknown"),
    };
    [
        (
            "schedule-diagnostic-scheduler",
            "Scheduler",
            available(d.scheduler),
        ),
        ("schedule-diagnostic-queue", "Queue", available(d.queue)),
        (
            "schedule-diagnostic-recorder",
            "Run recorder",
            available(d.recorder),
        ),
        ("schedule-diagnostic-due-work", "Due work", due),
    ]
    .into_iter()
    .map(|(id, label, (kind, text))| DiagnosticCard {
        id,
        label,
        kind,
        text,
    })
    .collect()
}

/// One rendered instant, the Bilimbi `<.datetime>` contract: both
/// server-decidable texts travel with the element for the shell's mode control.
pub(crate) struct Instant {
    id: String,
    value: Option<(String, String, String, String, &'static str)>,
}
fn instant(display: &DisplayContext, id: String, at: Option<Timestamp>) -> Instant {
    Instant {
        id,
        value: at.map(|at| {
            let r = display.render(at, Format::DateTime, Precision::Minute);
            (r.iso, r.text, r.company, r.utc, r.mode.as_str())
        }),
    }
}

pub(crate) enum Cell<'a> {
    TaskName {
        name: &'a str,
        key: &'a str,
    },
    Source {
        owner: &'a str,
        route: Option<&'a str>,
    },
    Schedule {
        words: &'a str,
        tooltip: &'a str,
    },
    Instant(&'a Instant),
    Status {
        kind: BadgeKind,
        label: &'a str,
        detail: &'a str,
    },
    Result {
        label: &'a str,
        detail: &'a str,
    },
    Started {
        at: &'a Instant,
        detail: &'a str,
    },
    RunSource {
        detail: &'a str,
    },
    Badge {
        kind: BadgeKind,
        label: &'a str,
    },
    Detail(&'a str),
}
#[derive(Template)]
#[template(path = "schedule_cell.html")]
struct CellView<'a> {
    cell: Cell<'a>,
    source: &'a str,
    /// A compact row is one line: the second line a cell draws is left out.
    compact: bool,
}
fn cell(cell: Cell<'_>, compact: bool) -> Result<Html> {
    render(CellView {
        cell,
        source: board::SOURCE,
        compact,
    })
}
#[derive(Template)]
#[template(path = "schedule_actions.html")]
struct Actions {
    buttons: Html,
}
#[derive(Template)]
#[template(path = "schedule_time.html")]
struct TimeField {
    field: Field,
}
#[derive(Template)]
#[template(path = "schedule_tasks.html")]
struct TasksPanel {
    timezone_request: String,
    search: Field,
    status: Field,
    unavailable: Html,
    card: Html,
}
#[derive(Template)]
#[template(path = "schedule_history.html")]
struct HistoryPanel {
    timezone_request: String,
    toolbar: Html,
    alert: Html,
    card: Html,
    pagination: Html,
}
#[derive(Template)]
#[template(path = "schedule_retention.html")]
struct RetentionBody {
    current: String,
    can_manage: bool,
    field: Field,
    save: Html,
}
#[derive(Template)]
#[template(path = "schedule_settings.html")]
struct SettingsPanel {
    card: Html,
}
#[derive(Template)]
#[template(path = "schedule.html")]
struct BoardView<'a> {
    flash: FlashMap<'a>,
    diagnostics: Vec<DiagnosticCard>,
    tabs: Html,
    body: Html,
    dialog: Html,
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "board").then_some("Schedule")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "board" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut board = Board::new(self.0.clone(), request.query)?;
            Ok(PageBody {
                html: board.render(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "board" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(Board::new(self.0.clone(), request.query)?);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Board {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let before = self.flash.clone();
            // Every input reads the rows again except a column operation
            // that only changes what is on screen, which `event` says.
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    let (tab, tasks, runs) = parse(values)?;
                    (self.tab, self.tasks, self.runs) = (tab, tasks, runs);
                }
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.render(request.scope).await?;
            let notice = match &self.flash {
                Some((StatusKind::Success, message)) if self.flash != before => message.clone(),
                _ => String::new(),
            };
            Ok(LiveBody {
                html,
                query: self.query(),
                notice,
                ..LiveBody::default()
            })
        })
    }
}
