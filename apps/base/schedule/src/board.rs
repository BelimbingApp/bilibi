//! The operator board's read model (Bilimbi `Schedule.Administration`):
//! filtered tasks, an exact database-filtered page of run history, diagnostics
//! and the history-retention setting. Commands stay in `lib.rs`; this module
//! only reads, except `set_keep_days`, which is an audited operator write.
//! Worker arguments and recorded output never cross these types.
use crate::{
    occurrence::{self, Run, Trigger},
    ReviewState, Schedule, Task, MANAGE, VIEW,
};
use bilibi_authz::{authorize_key, authorize_operator};
use bilibi_database::{Error, Result};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, tz::TimeZone, Timestamp};
use serde_json::json;
use std::{
    sync::Mutex,
    time::{Duration, SystemTime},
};
use tokio_postgres::GenericClient;

/// Global days of finished history to keep; zero disables pruning.
pub const KEEP_DAYS: &str = "schedule.history.keep_days";
pub const KEEP_DAYS_MIN: i64 = 0;
pub const KEEP_DAYS_MAX: i64 = 3650;
pub const KEEP_DAYS_DEFAULT: i64 = 90;
pub const TASK_STATUSES: [&str; 8] = [
    "never",
    "running",
    "succeeded",
    "failed",
    "skipped",
    "paused",
    "unreviewed",
    "disabled",
];
pub const RUN_STATUSES: [&str; 4] = ["running", "succeeded", "failed", "skipped"];
/// Every run row is a scheduler run; Bilimbi's `source` column has one value here.
pub const SOURCE: &str = "scheduler";

/// The one status a task row shows and filters by: a pause or a missing
/// review outranks the last run.
pub fn task_status(task: &Task) -> &str {
    if task.suppressed {
        "paused"
    } else {
        match task.review {
            ReviewState::Unreviewed => "unreviewed",
            ReviewState::Disabled => "disabled",
            ReviewState::Enabled => task.last_status.as_deref().unwrap_or("never"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskSort {
    Name,
    NextDue,
    LastRun,
}
/// Filters and orders the (bounded, in-memory) task list. Missing instants
/// sort last in either direction; ties fall back to the key.
pub fn filter_tasks(
    mut tasks: Vec<Task>,
    search: &str,
    status: Option<&str>,
    sort: TaskSort,
    descending: bool,
) -> Vec<Task> {
    let needle = search.trim().to_lowercase();
    tasks.retain(|task| {
        (needle.is_empty() || task.name.to_lowercase().contains(&needle))
            && status.is_none_or(|status| task_status(task) == status)
    });
    let instant = |task: &Task| match sort {
        TaskSort::Name => None,
        TaskSort::NextDue => task.next_due_at,
        TaskSort::LastRun => task.last_started_at,
    };
    tasks.sort_by(|a, b| {
        let order = match sort {
            TaskSort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
            _ => match (instant(a), instant(b)) {
                (Some(x), Some(y)) => x.cmp(&y),
                // A missing instant sorts last in either direction.
                (x, y) => return x.is_none().cmp(&y.is_none()).then_with(|| a.key.cmp(b.key)),
            },
        };
        if descending { order.reverse() } else { order }.then_with(|| a.key.cmp(b.key))
    });
    tasks
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunSort {
    StartedAt,
    Name,
    Source,
    Status,
}
/// A validated history query. Dates are calendar days of `started_at` in
/// `zone`, the zone the board displays the Started column in.
pub struct RunQuery {
    pub search: String,
    pub status: Option<String>,
    pub start: Option<Date>,
    pub end: Option<Date>,
    pub zone: TimeZone,
    pub sort: RunSort,
    pub descending: bool,
    pub page: u64,
    pub page_size: u64,
}
pub struct RunPage {
    pub entries: Vec<Run>,
    pub total: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Availability {
    Available,
    Unavailable,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DueWork {
    NoneDue,
    Due,
    Unknown,
}
/// Each fact is gathered independently; absence of rows is never health.
#[derive(Clone, Copy, Debug)]
pub struct Diagnostics {
    pub scheduler: Availability,
    pub queue: Availability,
    pub recorder: Availability,
    pub due_work: DueWork,
}

/// The local scheduler loop's last completed poll and its interval.
#[derive(Default)]
pub(crate) struct Heartbeat(Mutex<Option<(Timestamp, Duration)>>);
impl Heartbeat {
    fn alive(&self, now: Timestamp) -> bool {
        let Ok(beat) = self.0.lock() else {
            return false;
        };
        beat.is_some_and(|(at, interval)| {
            let allowed = (interval * 3).max(Duration::from_secs(60));
            now.duration_since(at).unsigned_abs() <= allowed
        })
    }
}

/// The start of a displayed day as an instant: a midnight skipped by a clock
/// change begins when clocks resume, a repeated one at its first pass.
fn day_start(date: Date, zone: &TimeZone) -> Result<SystemTime> {
    let zoned = date.to_zoned(zone.clone()).map_err(|_| Error::Invalid)?;
    Ok(SystemTime::from(zoned.timestamp()))
}
fn like(search: &str) -> Option<String> {
    let search = search.trim();
    (!search.is_empty()).then(|| {
        let escaped = search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        format!("%{escaped}%")
    })
}

impl Schedule {
    /// The scheduler loop reports each completed poll so the board can say
    /// whether this process's scheduler is running.
    pub fn beat(&self, at: Timestamp, interval: Duration) {
        if let Ok(mut beat) = self.heartbeat.0.lock() {
            *beat = Some((at, interval));
        }
    }

    /// Exact total and one page, filtered, ordered and paged in PostgreSQL.
    /// Needs `VIEW`. The sort is an enum, never a client identifier.
    pub async fn run_page(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        query: &RunQuery,
    ) -> Result<RunPage> {
        authorize_key(client, scope, VIEW).await?;
        if query.search.chars().count() > 255
            || query
                .status
                .as_deref()
                .is_some_and(|status| !RUN_STATUSES.contains(&status))
            || !(1..=100).contains(&query.page_size)
            || query.page == 0
            || matches!((query.start, query.end), (Some(start), Some(end)) if start > end)
        {
            return Err(Error::Invalid);
        }
        let start = query.start.map(|d| day_start(d, &query.zone)).transpose()?;
        let end = query
            .end
            .map(|d| d.tomorrow().map_err(|_| Error::Invalid))
            .transpose()?
            .map(|d| day_start(d, &query.zone))
            .transpose()?;
        let pattern = like(&query.search);
        let filter = "($1::text IS NULL OR name ILIKE $1 OR key ILIKE $1) AND ($2::text IS NULL OR status=$2) \
                      AND ($3::timestamptz IS NULL OR started_at>=$3) AND ($4::timestamptz IS NULL OR started_at<$4)";
        let total: i64 = client
            .query_one(
                &format!("SELECT count(*) FROM base_schedule_runs WHERE {filter}"),
                &[&pattern, &query.status, &start, &end],
            )
            .await?
            .get(0);
        let dir = if query.descending { "DESC" } else { "ASC" };
        let order = match query.sort {
            RunSort::StartedAt => format!("started_at {dir},id {dir}"),
            RunSort::Name => format!("name {dir},id {dir}"),
            RunSort::Status => format!("status {dir},id {dir}"),
            RunSort::Source => format!("id {dir}"),
        };
        let limit = query.page_size as i64;
        let offset = i64::try_from((query.page - 1).saturating_mul(query.page_size))
            .map_err(|_| Error::Invalid)?;
        let rows = client
            .query(
                &format!(
                    "SELECT id,key,name,expression,trigger,triggered_by_user_id,triggered_by_name,status,started_at,finished_at,runtime_ms \
                     FROM base_schedule_runs WHERE {filter} ORDER BY {order} LIMIT $5 OFFSET $6"
                ),
                &[&pattern, &query.status, &start, &end, &limit, &offset],
            )
            .await?;
        Ok(RunPage {
            entries: rows
                .into_iter()
                .map(|row| Run {
                    id: row.get(0),
                    key: row.get(1),
                    name: row.get(2),
                    expression: row.get(3),
                    trigger: Trigger::parse(row.get(4)).unwrap_or(Trigger::Scheduled),
                    triggered_by_user_id: row.get(5),
                    triggered_by_name: row.get(6),
                    status: row.get(7),
                    started_at: occurrence::from_pg(row.get(8)),
                    finished_at: row.get::<_, Option<SystemTime>>(9).map(occurrence::from_pg),
                    runtime_ms: row.get(10),
                    output_excerpt: None,
                    occurrence_id: None,
                    job_id: None,
                    attempt: None,
                })
                .collect(),
            total: total.max(0) as u64,
        })
    }

    /// Scheduler, Queue, recorder and due-work evidence, each independently;
    /// a failed read is `Unavailable`/`Unknown`, never a healthy default.
    /// `tasks` is the caller's already authorized task read, if it succeeded.
    pub async fn diagnostics(
        &self,
        client: &(impl GenericClient + Sync),
        tasks: Option<&[Task]>,
        now: Timestamp,
    ) -> Diagnostics {
        let probe = |sql: &'static str| async move {
            match client.query_opt(sql, &[]).await {
                Ok(_) => Availability::Available,
                Err(_) => Availability::Unavailable,
            }
        };
        let queue = probe("SELECT 1 FROM base_queue_jobs LIMIT 1").await;
        let recorder = probe("SELECT 1 FROM base_schedule_runs LIMIT 1").await;
        let due_work = match tasks {
            Some(tasks) => self.due_work(client, tasks, now).await,
            None => DueWork::Unknown,
        };
        Diagnostics {
            scheduler: if self.heartbeat.alive(now) {
                Availability::Available
            } else {
                Availability::Unavailable
            },
            queue,
            recorder,
            due_work,
        }
    }
    async fn due_work(
        &self,
        client: &(impl GenericClient + Sync),
        tasks: &[Task],
        now: Timestamp,
    ) -> DueWork {
        let keys: Vec<&str> = tasks.iter().map(|task| task.key).collect();
        let Ok(latest) = occurrence::latest_scheduled(client, &keys).await else {
            return DueWork::Unknown;
        };
        let mut due = false;
        for task in tasks
            .iter()
            .filter(|task| task.review == ReviewState::Enabled && !task.suppressed)
        {
            let Ok(timing) = self.timing(client, task.key).await else {
                return DueWork::Unknown;
            };
            let Ok(previous) = timing.previous(now) else {
                return DueWork::Unknown;
            };
            if previous.is_some_and(|intended| latest.get(task.key).is_none_or(|at| *at < intended))
            {
                due = true;
                break;
            }
        }
        if due {
            DueWork::Due
        } else {
            DueWork::NoneDue
        }
    }
}

/// The stored retention, read without a session (the pruner has none). A
/// stored value outside the definition's bounds is an error, not a default.
pub async fn keep_days(settings: &Settings, client: &(impl GenericClient + Sync)) -> Result<i64> {
    settings
        .get_global(client, KEEP_DAYS)
        .await?
        .as_i64()
        .filter(|days| (KEEP_DAYS_MIN..=KEEP_DAYS_MAX).contains(days))
        .ok_or(Error::Invalid)
}
/// The operator changes retention: operator identity plus `MANAGE`, written
/// with its Audit evidence in one transaction.
pub async fn set_keep_days(
    schedule: &Schedule,
    settings: &Settings,
    client: &mut tokio_postgres::Client,
    scope: &Scope,
    days: i64,
) -> Result<()> {
    authorize_operator(&*client, scope, MANAGE).await?;
    if !(KEEP_DAYS_MIN..=KEEP_DAYS_MAX).contains(&days) {
        return Err(Error::Invalid);
    }
    let mut tx = schedule
        .boundary
        .begin(client, scope.mutation_context("schedule.retention.changed"))
        .await?;
    settings
        .put(&mut tx, scope, Target::Global, KEEP_DAYS, json!(days))
        .await?;
    tx.commit().await
}

/// What one retention pass deleted.
#[derive(Clone, Copy, Debug, Default)]
pub struct Pruned {
    pub occurrences: u64,
    pub runs: u64,
}
/// Deletes finished occurrences and finished run rows older than `keep_days`
/// before `now`. Unfinished claims and each key's latest scheduled claim (the
/// scheduler's watermark) are never deleted. Zero keeps everything.
/// Housekeeping over operational evidence: no Audit row per deleted record.
pub async fn prune(client: &impl GenericClient, keep_days: i64, now: Timestamp) -> Result<Pruned> {
    if keep_days == 0 {
        return Ok(Pruned::default());
    }
    if !(1..=KEEP_DAYS_MAX).contains(&keep_days) {
        return Err(Error::Invalid);
    }
    let cutoff = SystemTime::from(now) - Duration::from_secs(keep_days as u64 * 86_400);
    let occurrences = client
        .execute(
            "DELETE FROM base_schedule_occurrences WHERE finished_at IS NOT NULL AND finished_at<$1 \
             AND id NOT IN (SELECT DISTINCT ON (key) id FROM base_schedule_occurrences WHERE trigger='scheduled' ORDER BY key,intended_at DESC,id DESC)",
            &[&cutoff],
        )
        .await?;
    let runs = client
        .execute(
            "DELETE FROM base_schedule_runs WHERE finished_at IS NOT NULL AND finished_at<$1",
            &[&cutoff],
        )
        .await?;
    Ok(Pruned { occurrences, runs })
}
