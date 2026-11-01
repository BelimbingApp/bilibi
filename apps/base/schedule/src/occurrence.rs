//! Durable occurrence claims, forbid-overlap leases and run history. The claim
//! itself lives in `Schedule::claim` (lib.rs) because it joins the Queue insert
//! in one audited transaction; this module owns the lifecycle SQL around it.
use crate::Registered;
use bilibi_database::Result;
use bilibi_queue::{self as store, State};
use jiff::Timestamp;
use std::{collections::BTreeMap, time::SystemTime};
use tokio_postgres::GenericClient;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Trigger {
    Scheduled,
    Manual,
}
impl Trigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Scheduled => "scheduled",
            Self::Manual => "manual",
        }
    }
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "scheduled" => Some(Self::Scheduled),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }
}
/// Why a claim was not made. All of these are expected outcomes, not failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Refusal {
    Unreviewed,
    Disabled,
    Suppressed,
    /// This exact intended instant was already claimed, by any process, ever.
    AlreadyClaimed,
    /// A forbid-overlap definition still holds its lease for an earlier occurrence.
    Overlap,
}
impl Refusal {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unreviewed => "unreviewed",
            Self::Disabled => "disabled",
            Self::Suppressed => "suppressed",
            Self::AlreadyClaimed => "already_claimed",
            Self::Overlap => "overlap",
        }
    }
}
/// Who asked for a manual run; carried on the run row, never looked up later.
#[derive(Clone, Debug)]
pub struct TriggeredBy {
    pub user_id: i64,
    pub name: Option<String>,
}

/// Redacted history row; no arguments ever cross this type.
#[derive(Clone, Debug)]
pub struct Run {
    pub id: i64,
    pub key: String,
    pub name: String,
    pub expression: Option<String>,
    pub trigger: Trigger,
    pub triggered_by_user_id: Option<i64>,
    pub triggered_by_name: Option<String>,
    pub status: String,
    pub started_at: Timestamp,
    pub finished_at: Option<Timestamp>,
    pub runtime_ms: Option<i32>,
    pub output_excerpt: Option<String>,
    pub occurrence_id: Option<i64>,
    pub job_id: Option<i64>,
    pub attempt: Option<i32>,
}

/// PostgreSQL keeps microseconds; a claim must store exactly what its job
/// metadata says so execution can find the row by equality.
pub fn micros(at: Timestamp) -> Timestamp {
    Timestamp::new(at.as_second(), (at.subsec_nanosecond() / 1000) * 1000).unwrap_or(at)
}
pub(crate) fn to_pg(at: Timestamp) -> SystemTime {
    SystemTime::from(at)
}
pub(crate) fn from_pg(at: SystemTime) -> Timestamp {
    Timestamp::try_from(at).unwrap_or(Timestamp::UNIX_EPOCH)
}

pub const RECONCILE_BATCH: i64 = 300;

/// Settles unfinished occurrences whose Queue job already reached a terminal
/// state (or vanished): `completed` is `succeeded`, anything else terminal is
/// `failed`, and the forbid-overlap lease is released. Runs before every claim
/// for the same key and on every poll, so a cancelled, discarded or pruned job
/// cannot hold a lease forever. Returns how many rows were settled.
pub async fn reconcile(client: &impl GenericClient, key: Option<&str>) -> Result<usize> {
    Ok(
        reconcile_occurrences(client, key).await?
            + reconcile_tenant_runs(client, key, None).await?,
    )
}
async fn reconcile_occurrences(client: &impl GenericClient, key: Option<&str>) -> Result<usize> {
    let rows = client
        .query(
            "SELECT id,tenant_id,job_id FROM base_schedule_occurrences WHERE finished_at IS NULL AND job_id IS NOT NULL AND ($1::text IS NULL OR key=$1) ORDER BY claimed_at,id LIMIT $2",
            &[&key, &RECONCILE_BATCH],
        )
        .await?;
    let mut by_tenant: BTreeMap<i64, Vec<(i64, i64)>> = BTreeMap::new();
    for row in rows {
        by_tenant
            .entry(row.get(1))
            .or_default()
            .push((row.get(0), row.get(2)));
    }
    let mut settled = 0;
    for (tenant, pairs) in by_tenant {
        let ids: Vec<i64> = pairs.iter().map(|(_, job)| *job).collect();
        let states: BTreeMap<i64, State> = store::states(client, tenant, &ids)
            .await?
            .into_iter()
            .map(|job| (job.id, job.state))
            .collect();
        for (occurrence, job) in pairs {
            let outcome = match states.get(&job) {
                Some(State::Completed) => "succeeded",
                Some(state) if state.terminal() => "failed",
                Some(_) => continue,
                None => "failed",
            };
            settled += finish(client, occurrence, outcome).await? as usize;
        }
    }
    Ok(settled)
}
/// The same settling for per-tenant runs: a run whose job reached a terminal
/// state without the owner's wrapper recording it (`actor_refused` after a
/// revoked grant or a suspended tenant, a discarded final attempt) becomes
/// `failed` with the Queue's bounded failure code, and a vanished job becomes
/// `failed` with `linked_job_missing`; a completed job is `succeeded`. Either
/// way the tenant's overlap lease is released. Never creates a replacement job.
pub async fn reconcile_tenant_runs(
    client: &impl GenericClient,
    key: Option<&str>,
    tenant: Option<i64>,
) -> Result<usize> {
    let rows = client
        .query(
            "SELECT id,tenant_id,job_id FROM base_schedule_tenant_runs WHERE finished_at IS NULL AND ($1::text IS NULL OR key=$1) AND ($2::bigint IS NULL OR tenant_id=$2) ORDER BY created_at,id LIMIT $3",
            &[&key, &tenant, &RECONCILE_BATCH],
        )
        .await?;
    let mut by_tenant: BTreeMap<i64, Vec<(i64, i64)>> = BTreeMap::new();
    for row in rows {
        by_tenant
            .entry(row.get(1))
            .or_default()
            .push((row.get(0), row.get(2)));
    }
    let mut settled = 0;
    for (tenant, pairs) in by_tenant {
        let ids: Vec<i64> = pairs.iter().map(|(_, job)| *job).collect();
        let states: BTreeMap<i64, store::JobStatus> = store::states(client, tenant, &ids)
            .await?
            .into_iter()
            .map(|job| (job.id, job))
            .collect();
        for (run, job) in pairs {
            let (status, code) = match states.get(&job) {
                Some(status) if status.state == State::Completed => ("succeeded", None),
                Some(status) if status.state.terminal() => (
                    "failed",
                    Some(
                        status
                            .failure_code
                            .clone()
                            .unwrap_or_else(|| "job_ended".into()),
                    ),
                ),
                Some(_) => continue,
                None => ("failed", Some("linked_job_missing".into())),
            };
            settled += client
                .execute(
                    "UPDATE base_schedule_tenant_runs SET status=$2,code=$3,finished_at=clock_timestamp(),overlap_key=NULL WHERE id=$1 AND finished_at IS NULL",
                    &[&run, &status, &code],
                )
                .await? as usize;
        }
    }
    Ok(settled)
}
pub(crate) async fn finish(client: &impl GenericClient, id: i64, state: &str) -> Result<bool> {
    Ok(client
        .execute(
            "UPDATE base_schedule_occurrences SET state=$2,finished_at=clock_timestamp(),overlap_key=NULL WHERE id=$1 AND finished_at IS NULL",
            &[&id, &state],
        )
        .await?
        == 1)
}
/// The scheduler's watermark: the latest intended instant ever claimed per key.
pub async fn latest_scheduled(
    client: &impl GenericClient,
    keys: &[&str],
) -> Result<BTreeMap<String, Timestamp>> {
    Ok(client
        .query(
            "SELECT key,max(intended_at) FROM base_schedule_occurrences WHERE trigger='scheduled' AND key=ANY($1) GROUP BY key",
            &[&keys],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), from_pg(row.get(1))))
        .collect())
}

/// An occurrence refused for overlap is history too. One `skipped` row per
/// intended instant: polling retries the same instant every interval while the
/// earlier run is still going, and that is not a new event each time.
pub async fn record_skipped(
    client: &impl GenericClient,
    item: &Registered,
    intended_at: Timestamp,
    trigger: Trigger,
    triggered_by: Option<&TriggeredBy>,
) -> Result<bool> {
    let expression = if trigger == Trigger::Scheduled {
        let row = client
            .query_opt(
                "SELECT * FROM base_schedule_timings WHERE key=$1",
                &[&item.key()],
            )
            .await?;
        Some(crate::timing::Timing::from_row(item, row.as_ref())?.expression)
    } else {
        None
    };
    Ok(client.execute(
        "INSERT INTO base_schedule_runs(key,name,expression,trigger,triggered_by_user_id,triggered_by_name,status,started_at,finished_at,runtime_ms,output_excerpt) \
         SELECT $1::text,$2::text,$3::text,$4::text,$5::bigint,$6::text,'skipped',$7::timestamptz,clock_timestamp(),0,'overlap' \
         WHERE NOT EXISTS (SELECT 1 FROM base_schedule_runs WHERE key=$1::text AND trigger=$4::text AND status='skipped' AND started_at=$7::timestamptz)",
        &[&item.key(), &item.definition().task_name, &expression, &trigger.as_str(), &triggered_by.map(|t| t.user_id), &triggered_by.and_then(|t| t.name.as_deref()), &to_pg(intended_at)],
    ).await? == 1)
}
pub(crate) async fn runs(
    client: &impl GenericClient,
    key: Option<&str>,
    limit: i64,
) -> Result<Vec<Run>> {
    Ok(client
        .query(
            "SELECT id,key,name,expression,trigger,triggered_by_user_id,triggered_by_name,status,started_at,finished_at,runtime_ms,output_excerpt,occurrence_id,job_id,attempt \
             FROM base_schedule_runs WHERE ($1::text IS NULL OR key=$1) ORDER BY started_at DESC,id DESC LIMIT $2",
            &[&key, &limit],
        )
        .await?
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
            started_at: from_pg(row.get(8)),
            finished_at: row.get::<_, Option<SystemTime>>(9).map(from_pg),
            runtime_ms: row.get(10),
            output_excerpt: row.get(11),
            occurrence_id: row.get(12),
            job_id: row.get(13),
            attempt: row.get(14),
        })
        .collect())
}
