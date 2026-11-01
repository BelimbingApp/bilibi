//! The only shape that reaches the history: stable identity, outcome, timings,
//! counts, a coarse size class and process facts. There is deliberately no field
//! that could carry a query string, SQL, an argument, an identifier or an error.
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    /// An HTTP request, keyed by its route template (`/companies/{id}`).
    Request,
    /// A live-page event, keyed by `event:` plus the page's route template.
    Event,
    /// One queue attempt, keyed by the worker id.
    Job,
    /// A periodic sample of this process, identity `process`.
    Runtime,
}
impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Event => "event",
            Self::Job => "job",
            Self::Runtime => "runtime",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Request, Self::Event, Self::Job, Self::Runtime]
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Outcome {
    Ok,
    Error,
    Cancelled,
    Discarded,
}
impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Cancelled => "cancelled",
            Self::Discarded => "discarded",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [Self::Ok, Self::Error, Self::Cancelled, Self::Discarded]
            .into_iter()
            .find(|outcome| outcome.as_str() == value)
    }
}

/// Response size, in four classes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SizeClass {
    Under1k,
    From1kTo10k,
    From10kTo100k,
    Over100k,
}
impl SizeClass {
    pub fn of(bytes: u64) -> Self {
        match bytes {
            0..1_024 => Self::Under1k,
            1_024..10_240 => Self::From1kTo10k,
            10_240..102_400 => Self::From10kTo100k,
            _ => Self::Over100k,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Under1k => "under_1k",
            Self::From1kTo10k => "1k_10k",
            Self::From10kTo100k => "10k_100k",
            Self::Over100k => "over_100k",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Under1k,
            Self::From1kTo10k,
            Self::From10kTo100k,
            Self::Over100k,
        ]
        .into_iter()
        .find(|class| class.as_str() == value)
    }
}

/// Facts about this process, each `None` where the platform cannot report it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Process {
    /// Resident set size.
    pub memory_bytes: Option<i64>,
    /// Tasks waiting in Tokio's global (injection) queue.
    pub run_queue: Option<i64>,
    /// Spawned Tokio tasks that have not finished.
    pub alive_tasks: Option<i64>,
    pub threads: Option<i64>,
    pub open_fds: Option<i64>,
    /// Cumulative user plus system CPU time of the process.
    pub cpu_ms: Option<i64>,
    /// Database pool connections checked out right now.
    pub pool_in_use: Option<i64>,
    /// Callers waiting for a pool connection right now.
    pub pool_waiting: Option<i64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    pub kind: Kind,
    pub identity: String,
    pub outcome: Outcome,
    pub duration_ms: i64,
    pub db_duration_ms: i64,
    pub db_count: i32,
    pub response_size_class: Option<SizeClass>,
    pub process: Process,
    pub observed_at: SystemTime,
}
impl Sample {
    pub fn runtime(process: Process) -> Self {
        Self {
            kind: Kind::Runtime,
            identity: "process".into(),
            outcome: Outcome::Ok,
            duration_ms: 0,
            db_duration_ms: 0,
            db_count: 0,
            response_size_class: None,
            process,
            observed_at: SystemTime::now(),
        }
    }
    /// The same rules the table's constraints enforce, checked before a row can
    /// share a batch with valid ones.
    pub fn valid(&self) -> bool {
        valid_identity(self.kind, &self.identity)
            && self.duration_ms >= 0
            && self.db_duration_ms >= 0
            && self.db_count >= 0
            && [
                self.process.memory_bytes,
                self.process.run_queue,
                self.process.alive_tasks,
                self.process.threads,
                self.process.open_fds,
                self.process.cpu_ms,
                self.process.pool_in_use,
                self.process.pool_waiting,
            ]
            .into_iter()
            .flatten()
            .all(|value| value >= 0)
    }
}

/// Route templates may carry axum `{param}` and `{*rest}` markers but never a
/// query string, so an identifier or secret in a URL cannot become an identity.
fn route_template(route: &str, limit: usize) -> bool {
    route.starts_with('/')
        && route.len() <= limit
        && route
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_/{}:.*-".contains(&b))
}
pub fn valid_identity(kind: Kind, identity: &str) -> bool {
    match kind {
        Kind::Request => route_template(identity, 255),
        Kind::Event => identity
            .strip_prefix("event:")
            .is_some_and(|route| route_template(route, 249)),
        Kind::Job => {
            let bytes = identity.as_bytes();
            !bytes.is_empty()
                && bytes.len() <= 128
                && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
                && bytes
                    .iter()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_/-".contains(b))
        }
        Kind::Runtime => identity == "process",
    }
}
