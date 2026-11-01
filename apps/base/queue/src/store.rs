//! Trusted transport lifecycle SQL, separate from audited owner business writes.
use bilibi_database::{Change, Db, Error, MutationTransaction, Result};
use bilibi_tenancy::Scope;
use serde_json::json;
use tokio_postgres::{GenericClient, Row};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum State {
    Available,
    Scheduled,
    Executing,
    Retryable,
    Completed,
    Cancelled,
    Discarded,
}
impl State {
    pub fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Discarded)
    }
    fn parse(state: &str) -> Result<Self> {
        Ok(match state {
            "available" => Self::Available,
            "scheduled" => Self::Scheduled,
            "executing" => Self::Executing,
            "retryable" => Self::Retryable,
            "completed" => Self::Completed,
            "cancelled" => Self::Cancelled,
            "discarded" => Self::Discarded,
            _ => return Err(Error::Internal),
        })
    }
}
/// Redacted facts only; no args, credentials, actors or lease tokens.
#[derive(Clone, Debug)]
pub struct JobStatus {
    pub id: i64,
    pub state: State,
    pub attempt: i32,
    pub max_attempts: i32,
    pub failure_code: Option<String>,
}
impl JobStatus {
    fn row(row: Row) -> Result<Self> {
        Ok(Self {
            id: row.get("id"),
            state: State::parse(row.get::<_, &str>("state"))?,
            attempt: row.get("attempt"),
            max_attempts: row.get("max_attempts"),
            failure_code: row.get("failure_code"),
        })
    }
}
/// Internal owner/scheduler adapter: tenant comes from its proven scope. This
/// lookup does not grant access; a UI/API adapter must authorize its own reads.
pub async fn lookup(client: &impl GenericClient, tenant: i64, id: i64) -> Result<JobStatus> {
    if tenant <= 0 || id <= 0 {
        return Err(Error::Invalid);
    }
    JobStatus::row(client.query_opt("SELECT id,state,attempt,max_attempts,failure_code FROM base_queue_jobs WHERE tenant_id=$1 AND id=$2", &[&tenant,&id]).await?.ok_or(Error::NotFound)?)
}
/// Bounded bulk terminal lookup for owner reconciliation; missing IDs are absent.
pub async fn states(
    client: &impl GenericClient,
    tenant: i64,
    ids: &[i64],
) -> Result<Vec<JobStatus>> {
    if tenant <= 0 || ids.len() > 1000 || ids.iter().any(|id| *id <= 0) {
        return Err(Error::Invalid);
    }
    client.query("SELECT id,state,attempt,max_attempts,failure_code FROM base_queue_jobs WHERE tenant_id=$1 AND id=ANY($2) ORDER BY id", &[&tenant,&ids]).await?.into_iter().map(JobStatus::row).collect()
}
/// The one-line queue status System Info shows: platform-wide backlog and
/// recovery counts only, no job, tenant or actor detail. It
/// counts `available` and `scheduled` jobs as pending.
pub async fn health_status(client: &impl GenericClient) -> Result<String> {
    let row = client
        .query_one(
            "SELECT count(*) FILTER (WHERE state IN ('available','scheduled')), count(*) FILTER (WHERE state='retryable'), count(*) FILTER (WHERE state='discarded') FROM base_queue_jobs",
            &[],
        )
        .await?;
    Ok(format!(
        "Available ({} pending, {} retryable, {} discarded)",
        row.get::<_, i64>(0),
        row.get::<_, i64>(1),
        row.get::<_, i64>(2)
    ))
}
fn actor_matches(tx: &MutationTransaction<'_>, scope: &Scope) -> bool {
    tx.context().tenant == scope.tenant()
        && tx.context().actor.kind == "user"
        && tx.context().actor.id == scope.user().to_string()
        && tx.context().impersonator.is_none()
}
/// Owner must authorize cancel/retry before calling. The row lock serializes
/// cancellation with completion and with Execution::begin business commits.
pub async fn cancel_for(tx: &mut MutationTransaction<'_>, scope: &Scope, id: i64) -> Result<State> {
    control(tx, scope, id, false).await
}
/// Explicitly retry a failed/cancelled job with a fresh attempt budget. Completed
/// jobs are never replayed. This is an owner command, not automatic backoff.
pub async fn retry_for(tx: &mut MutationTransaction<'_>, scope: &Scope, id: i64) -> Result<State> {
    control(tx, scope, id, true).await
}
async fn control(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    id: i64,
    retry: bool,
) -> Result<State> {
    if id <= 0 {
        return Err(Error::Invalid);
    }
    if !actor_matches(tx, scope) {
        return Err(Error::Forbidden);
    }
    let tenant = scope.tenant();
    tx.mutate(move |sql| Box::pin(async move {
        let row = sql.query_opt("SELECT state FROM base_queue_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE", &[&tenant,&id]).await?.ok_or(Error::NotFound)?;
        let old: String = row.get(0);
        let old_state = State::parse(&old)?;
        let state = if retry {
            if !matches!(old_state, State::Cancelled | State::Discarded | State::Retryable | State::Scheduled) { return Err(Error::Conflict); }
            sql.execute("UPDATE base_queue_jobs SET state='available',attempt=0,available_at=clock_timestamp(),updated_at=clock_timestamp(),cancelled_at=NULL,discarded_at=NULL,failure_code=NULL WHERE id=$1", &[&id]).await?;
            State::Available
        } else if old_state.terminal() {
            old_state
        } else {
            sql.execute("UPDATE base_queue_jobs SET state='cancelled',cancelled_at=clock_timestamp(),updated_at=clock_timestamp(),lease_token=NULL,lease_expires_at=NULL,failure_code='owner_cancelled' WHERE id=$1", &[&id]).await?;
            State::Cancelled
        };
        Ok((state,vec![Change {kind:"base/queue.job".into(),id,old:json!({"state":old}),new:json!({"state":format!("{state:?}").to_lowercase()})}]))
    })).await
}

pub(crate) struct Claim {
    pub row: Row,
    pub token: String,
}
/// Reaping and claiming are one transaction. SKIP LOCKED lets independent app
/// processes claim different jobs. An expired final attempt is discarded.
pub(crate) async fn claim(db: &Db, lease_seconds: i32) -> Result<Option<Claim>> {
    let mut client = db.get().await?;
    let tx = client.transaction().await?;
    tx.execute("WITH expired AS (SELECT id FROM base_queue_jobs WHERE state='executing' AND lease_expires_at<=clock_timestamp() ORDER BY lease_expires_at,id FOR UPDATE SKIP LOCKED LIMIT 100) UPDATE base_queue_jobs j SET state=CASE WHEN attempt>=max_attempts THEN 'discarded' ELSE 'retryable' END,available_at=clock_timestamp()+make_interval(secs=>LEAST(3600, power(2,LEAST(attempt,12))::integer)),discarded_at=CASE WHEN attempt>=max_attempts THEN clock_timestamp() ELSE NULL END,lease_token=NULL,lease_expires_at=NULL,failure_code='lease_expired',updated_at=clock_timestamp() FROM expired e WHERE j.id=e.id", &[]).await?;
    let token = uuid::Uuid::new_v4().to_string();
    let row = tx.query_opt("WITH candidate AS (SELECT id FROM base_queue_jobs WHERE state IN ('available','scheduled','retryable') AND available_at<=clock_timestamp() AND attempt<max_attempts ORDER BY available_at,id FOR UPDATE SKIP LOCKED LIMIT 1) UPDATE base_queue_jobs j SET state='executing',attempt=attempt+1,attempted_at=clock_timestamp(),updated_at=clock_timestamp(),lease_token=$1,lease_expires_at=clock_timestamp()+make_interval(secs=>$2::integer) FROM candidate c WHERE j.id=c.id RETURNING j.*", &[&token,&lease_seconds]).await?;
    tx.commit().await?;
    Ok(row.map(|row| Claim { row, token }))
}
pub(crate) async fn heartbeat(db: &Db, id: i64, token: &str, seconds: i32) -> Result<bool> {
    Ok(db.get().await?.execute("UPDATE base_queue_jobs SET lease_expires_at=clock_timestamp()+make_interval(secs=>$3::integer),updated_at=clock_timestamp() WHERE id=$1 AND state='executing' AND lease_token=$2 AND lease_expires_at>clock_timestamp()", &[&id,&token,&seconds]).await? == 1)
}
pub(crate) async fn finish(
    db: &Db,
    id: i64,
    token: &str,
    state: &str,
    code: Option<&str>,
) -> Result<bool> {
    Ok(db.get().await?.execute("UPDATE base_queue_jobs SET state=$3,failure_code=$4,lease_token=NULL,lease_expires_at=NULL,updated_at=clock_timestamp(),available_at=CASE WHEN $3='retryable' THEN clock_timestamp()+make_interval(secs=>LEAST(3600,power(2,LEAST(attempt,12))::integer)) ELSE available_at END,completed_at=CASE WHEN $3='completed' THEN clock_timestamp() ELSE NULL END,cancelled_at=CASE WHEN $3='cancelled' THEN clock_timestamp() ELSE NULL END,discarded_at=CASE WHEN $3='discarded' THEN clock_timestamp() ELSE NULL END WHERE id=$1 AND state='executing' AND lease_token=$2 AND lease_expires_at>clock_timestamp()", &[&id,&token,&state,&code]).await? == 1)
}
