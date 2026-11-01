//! Bounded ingress and persistence. Observed work only ever calls `submit`,
//! which never awaits, never touches the database or settings and never fails:
//! a full queue, a slow database or a dead writer costs the sample, not the
//! business operation. A writer task batches rows into PostgreSQL on its own
//! connections with its own timeouts.
use crate::{
    runtime,
    sample::{Kind, Process, Sample},
};
use bilibi_database::{Db, Error, Result};
use bilibi_settings::Settings;
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, SystemTime},
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinSet,
    time::{interval, timeout, MissedTickBehavior},
};

pub const ENABLED: &str = "perf.enabled";
pub const MINIMUM_DURATION_MS: &str = "perf.minimum_duration_ms";
pub const SAMPLE_RATE: &str = "perf.sample_rate";
pub const SLOW_THRESHOLD_MS: &str = "perf.slow_threshold_ms";
pub const KEEP_DAYS: &str = "perf.history.keep_days";
pub const MAX_ROWS: &str = "perf.history.max_rows";

/// The reconciliation: the row cap is re-enforced after this
/// many accepted rows, bounding overshoot between scheduled retention runs.
const PRUNE_EVERY: usize = 100;
const PRUNE_TIMEOUT: Duration = Duration::from_secs(30);
const SETTINGS_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug)]
pub struct Config {
    /// Samples that may wait for the writer; further ones are dropped and counted.
    pub max_pending: usize,
    /// Rows per INSERT.
    pub batch: usize,
    /// Bound on one batch, including waiting for a pool connection.
    pub write_timeout: Duration,
    /// How often other processes' settings changes are noticed.
    pub policy_refresh: Duration,
    /// How often this process is sampled.
    pub runtime_interval: Duration,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            max_pending: 1_000,
            batch: 100,
            write_timeout: Duration::from_secs(5),
            policy_refresh: Duration::from_secs(10),
            runtime_interval: Duration::from_secs(60),
        }
    }
}
impl Config {
    fn validate(self) -> Result<Self> {
        if !(1..=100_000).contains(&self.max_pending)
            || !(1..=1_000).contains(&self.batch)
            || self.write_timeout < Duration::from_millis(100)
            || self.write_timeout > Duration::from_secs(60)
            || self.policy_refresh < Duration::from_secs(1)
            || self.runtime_interval < Duration::from_secs(1)
        {
            return Err(Error::Invalid);
        }
        Ok(self)
    }
}

/// What the ingress decides from settings. Read with atomics on the hot path and
/// refreshed in the background; until the first refresh it holds the defaults.
struct Policy {
    enabled: AtomicBool,
    minimum_ms: AtomicU64,
    rate_bits: AtomicU64,
}
impl Policy {
    fn rate(&self) -> f64 {
        f64::from_bits(self.rate_bits.load(Ordering::Relaxed))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Submit {
    Accepted,
    /// Not eligible: recording is off, the sample is below the minimum duration
    /// or outside the sample rate, or it failed validation.
    Ignored,
    /// The writer's queue was full; the sample is lost and counted.
    Dropped,
}

#[derive(Clone, Copy, Debug)]
pub struct Stats {
    pub pending: usize,
    pub max_pending: usize,
    /// Samples lost to a full queue or a failed write, since this process started.
    pub dropped: u64,
    pub persisted: u64,
    /// The writer task is running in this process.
    pub running: bool,
}

pub struct Recorder {
    sender: mpsc::Sender<Sample>,
    receiver: Mutex<Option<mpsc::Receiver<Sample>>>,
    policy: Policy,
    dropped: AtomicU64,
    persisted: AtomicU64,
    running: AtomicBool,
    sequence: AtomicU64,
    pub(crate) db: Db,
    pub(crate) settings: Arc<Settings>,
    config: Config,
}
impl Recorder {
    pub fn new(db: Db, settings: Arc<Settings>, config: Config) -> Result<Self> {
        let config = config.validate()?;
        let (sender, receiver) = mpsc::channel(config.max_pending);
        Ok(Self {
            sender,
            receiver: Mutex::new(Some(receiver)),
            policy: Policy {
                enabled: AtomicBool::new(true),
                minimum_ms: AtomicU64::new(100),
                rate_bits: AtomicU64::new(1.0f64.to_bits()),
            },
            dropped: AtomicU64::new(0),
            persisted: AtomicU64::new(0),
            running: AtomicBool::new(false),
            sequence: AtomicU64::new(0),
            db,
            settings,
            config,
        })
    }

    /// Never blocks and never fails. This is the whole business-path cost of
    /// recording: a few atomic loads and one `try_send`.
    pub fn submit(&self, sample: Sample) -> Submit {
        if !self.policy.enabled.load(Ordering::Relaxed)
            || !sample.valid()
            || (sample.kind != Kind::Runtime
                && u64::try_from(sample.duration_ms).unwrap_or(0)
                    < self.policy.minimum_ms.load(Ordering::Relaxed))
            || !self.chance(self.policy.rate())
        {
            return Submit::Ignored;
        }
        match self.sender.try_send(sample) {
            Ok(()) => Submit::Accepted,
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
                Submit::Dropped
            }
        }
    }
    /// Current facts about this process: what each runtime sample records.
    pub async fn process(&self) -> Process {
        runtime::snapshot(&self.db).await
    }
    /// Whether recording is currently on, as last read from settings.
    pub fn recording(&self) -> bool {
        self.policy.enabled.load(Ordering::Relaxed)
    }
    pub fn stats(&self) -> Stats {
        Stats {
            pending: self.sender.max_capacity() - self.sender.capacity(),
            max_pending: self.sender.max_capacity(),
            dropped: self.dropped.load(Ordering::Relaxed),
            persisted: self.persisted.load(Ordering::Relaxed),
            running: self.running.load(Ordering::Relaxed),
        }
    }
    /// A cheap uniform draw (splitmix64 over a counter): sampling needs spread,
    /// not secrecy, and the ingress takes no lock and pulls in no RNG.
    fn chance(&self, rate: f64) -> bool {
        if rate >= 1.0 {
            return true;
        }
        if rate <= 0.0 {
            return false;
        }
        let mut z = self
            .sequence
            .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
            .wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        ((z >> 11) as f64 / (1u64 << 53) as f64) < rate
    }

    /// Runs the writer and the background housekeeping (settings refresh and
    /// process sampling) until `shutdown` flips, then flushes what is queued.
    /// Call once per process; a second call is refused.
    pub async fn run(self: Arc<Self>, shutdown: watch::Receiver<bool>) -> Result<()> {
        let receiver = self
            .receiver
            .lock()
            .map_err(|_| Error::Internal)?
            .take()
            .ok_or(Error::Invalid)?;
        self.running.store(true, Ordering::Relaxed);
        let mut tasks = JoinSet::new();
        tasks.spawn(self.clone().write(receiver, shutdown.clone()));
        tasks.spawn(self.clone().housekeeping(shutdown));
        let mut failed = false;
        while let Some(result) = tasks.join_next().await {
            failed |= result.is_err();
        }
        self.running.store(false, Ordering::Relaxed);
        if failed {
            Err(Error::Internal)
        } else {
            Ok(())
        }
    }

    async fn write(
        self: Arc<Self>,
        mut receiver: mpsc::Receiver<Sample>,
        mut shutdown: watch::Receiver<bool>,
    ) {
        let mut buffer = Vec::with_capacity(self.config.batch);
        let mut since_prune = 0;
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                _ = shutdown.changed() => {}
                received = receiver.recv_many(&mut buffer, self.config.batch) => {
                    if received == 0 {
                        break;
                    }
                    self.flush(&mut buffer, &mut since_prune).await;
                }
            }
        }
        // Leave nothing queued behind a clean shutdown; the same timeouts apply.
        while let Ok(sample) = receiver.try_recv() {
            buffer.push(sample);
            if buffer.len() >= self.config.batch {
                self.flush(&mut buffer, &mut since_prune).await;
            }
        }
        self.flush(&mut buffer, &mut since_prune).await;
    }
    async fn flush(&self, buffer: &mut Vec<Sample>, since_prune: &mut usize) {
        if buffer.is_empty() {
            return;
        }
        let batch = std::mem::take(buffer);
        let count = batch.len() as u64;
        match timeout(self.config.write_timeout, self.insert(&batch)).await {
            Ok(Ok(())) => {
                self.persisted.fetch_add(count, Ordering::Relaxed);
                *since_prune += batch.len();
            }
            // A failed or timed out write drops the batch: history is
            // best-effort and must never queue behind a sick database.
            _ => {
                self.dropped.fetch_add(count, Ordering::Relaxed);
            }
        }
        if *since_prune >= PRUNE_EVERY {
            *since_prune = 0;
            let _ = timeout(PRUNE_TIMEOUT, self.prune()).await;
        }
    }
    async fn insert(&self, batch: &[Sample]) -> Result<()> {
        fn column<'a, T>(batch: &'a [Sample], read: impl Fn(&'a Sample) -> T) -> Vec<T> {
            batch.iter().map(read).collect()
        }
        let kinds = column(batch, |sample| sample.kind.as_str());
        let identities = column(batch, |sample| sample.identity.as_str());
        let outcomes = column(batch, |sample| sample.outcome.as_str());
        let durations = column(batch, |sample| sample.duration_ms);
        let db_durations = column(batch, |sample| sample.db_duration_ms);
        let db_counts = column(batch, |sample| sample.db_count);
        let sizes = column(batch, |sample| {
            sample.response_size_class.map(|class| class.as_str())
        });
        let memory = column(batch, |sample| sample.process.memory_bytes);
        let queues = column(batch, |sample| sample.process.run_queue);
        let tasks = column(batch, |sample| sample.process.alive_tasks);
        let threads = column(batch, |sample| sample.process.threads);
        let descriptors = column(batch, |sample| sample.process.open_fds);
        let cpu = column(batch, |sample| sample.process.cpu_ms);
        let in_use = column(batch, |sample| sample.process.pool_in_use);
        let waiting = column(batch, |sample| sample.process.pool_waiting);
        let observed = column(batch, |sample| sample.observed_at);
        let client = self.db.get().await?;
        client
            .execute(
                "INSERT INTO base_perf_samples(kind,identity,outcome,duration_ms,db_duration_ms,db_count,response_size_class,memory_bytes,run_queue,alive_tasks,threads,open_fds,cpu_ms,pool_in_use,pool_waiting,observed_at) \
                 SELECT * FROM UNNEST($1::text[],$2::text[],$3::text[],$4::bigint[],$5::bigint[],$6::int[],$7::text[],$8::bigint[],$9::bigint[],$10::bigint[],$11::bigint[],$12::bigint[],$13::bigint[],$14::bigint[],$15::bigint[],$16::timestamptz[])",
                &[&kinds, &identities, &outcomes, &durations, &db_durations, &db_counts, &sizes, &memory, &queues, &tasks, &threads, &descriptors, &cpu, &in_use, &waiting, &observed],
            )
            .await?;
        Ok(())
    }

    async fn housekeeping(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut policy = interval(self.config.policy_refresh);
        let mut process = interval(self.config.runtime_interval);
        for tick in [&mut policy, &mut process] {
            tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        }
        let mut changes = self.settings.subscribe();
        loop {
            if *shutdown.borrow() {
                return;
            }
            tokio::select! {
                _ = shutdown.changed() => {}
                _ = policy.tick() => self.refresh_policy().await,
                change = changes.recv() => match change {
                    // Same-process edits apply at once; other processes are
                    // noticed by the tick above. A lagged receiver just reloads.
                    Ok(change) if !change.key.starts_with("perf.") => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                        changes = self.settings.subscribe();
                        tokio::time::sleep(Duration::from_secs(1)).await;
                    }
                    _ => self.refresh_policy().await,
                },
                _ = process.tick() => {
                    let facts = self.process().await;
                    self.submit(Sample::runtime(facts));
                }
            }
        }
    }
    /// A failed read keeps the previous policy: a database blip must not switch
    /// recording off, nor on.
    async fn refresh_policy(&self) {
        let read = async {
            let client = self.db.get().await?;
            let setting = |key| self.settings.get_global(&**client, key);
            Ok::<_, Error>((
                setting(ENABLED).await?,
                setting(MINIMUM_DURATION_MS).await?,
                setting(SAMPLE_RATE).await?,
            ))
        };
        if let Ok(Ok((enabled, minimum, rate))) = timeout(SETTINGS_TIMEOUT, read).await {
            if let Some(enabled) = enabled.as_bool() {
                self.policy.enabled.store(enabled, Ordering::Relaxed);
            }
            if let Some(minimum) = minimum.as_u64() {
                self.policy.minimum_ms.store(minimum, Ordering::Relaxed);
            }
            if let Some(rate) = rate.as_f64().filter(|rate| (0.0..=1.0).contains(rate)) {
                self.policy
                    .rate_bits
                    .store(rate.to_bits(), Ordering::Relaxed);
            }
        }
    }

    /// Deletes history older than `perf.history.keep_days` and rows beyond
    /// `perf.history.max_rows`, newest kept. Idempotent, so concurrent pruning
    /// by several processes is safe. Returns the rows removed.
    pub async fn prune(&self) -> Result<u64> {
        let client = self.db.get().await?;
        let keep_days = self.settings.get_global(&**client, KEEP_DAYS).await?;
        let max_rows = self.settings.get_global(&**client, MAX_ROWS).await?;
        let (Some(keep_days), Some(max_rows)) = (
            keep_days.as_i64().filter(|days| *days > 0),
            max_rows.as_i64().filter(|rows| *rows > 0),
        ) else {
            return Err(Error::Invalid);
        };
        let cutoff = SystemTime::now()
            .checked_sub(Duration::from_secs(keep_days as u64 * 86_400))
            .ok_or(Error::Invalid)?;
        let expired = client
            .execute(
                "DELETE FROM base_perf_samples WHERE observed_at < $1",
                &[&cutoff],
            )
            .await?;
        let overflow = client
            .execute(
                "DELETE FROM base_perf_samples WHERE id IN (SELECT id FROM base_perf_samples ORDER BY observed_at DESC, id DESC OFFSET $1)",
                &[&max_rows],
            )
            .await?;
        Ok(expired + overflow)
    }
}
