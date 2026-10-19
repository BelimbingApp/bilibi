//! The job rows: what a server owner asked for and what the worker reported.
//! Every statement binds the operator tenant the request was made in.
use bilibi_database::{Change, Error, MutationTransaction, Result};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

/// The PostgreSQL channel a builder or installer listens on; the payload is
/// the job id. Sent inside the commit, so it arrives only once the row is
/// visible.
pub const CHANNEL: &str = "bilibi_software_jobs";
/// The record kind every job write is audited under.
pub const RECORD: &str = "base/software.job";

const TIME: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'";
pub(crate) const JOB_COLUMNS: &str = "id, kind, state, request, review, result, undo_of, switch, requested_by, worker, cancel_requested_by,\
 to_char(requested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),\
 to_char(started_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),\
 to_char(finished_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'),\
 to_char(cancel_requested_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')";

/// One job as stored.
#[derive(Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub kind: String,
    pub state: String,
    pub request: Value,
    pub review: Value,
    pub result: Option<Value>,
    pub undo_of: Option<i64>,
    pub switch: String,
    pub requested_by: i64,
    pub worker: Option<String>,
    pub cancel_requested_by: Option<i64>,
    pub requested_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub cancel_requested_at: Option<String>,
}
impl Job {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self::from_row_offset(row, 0)
    }
    /// `JOB_COLUMNS` starting at column `at` (a worker query may select the
    /// tenant first).
    pub(crate) fn from_row_offset(row: &tokio_postgres::Row, at: usize) -> Self {
        Self {
            id: row.get(at),
            kind: row.get(at + 1),
            state: row.get(at + 2),
            request: row.get(at + 3),
            review: row.get(at + 4),
            result: row.get(at + 5),
            undo_of: row.get(at + 6),
            switch: row.get(at + 7),
            requested_by: row.get(at + 8),
            worker: row.get(at + 9),
            cancel_requested_by: row.get(at + 10),
            requested_at: row.get(at + 11),
            started_at: row.get(at + 12),
            finished_at: row.get(at + 13),
            cancel_requested_at: row.get(at + 14),
        }
    }
    pub fn active(&self) -> bool {
        matches!(self.state.as_str(), "queued" | "running")
    }
    /// The job as a record (for audit and the status answer), without steps.
    pub fn json(&self) -> Value {
        json!({
            "id": self.id, "kind": self.kind, "state": self.state, "request": self.request,
            "review": self.review, "result": self.result, "undo_of": self.undo_of, "switch": self.switch,
            "requested_by": self.requested_by, "requested_at": self.requested_at,
            "worker": self.worker, "started_at": self.started_at, "finished_at": self.finished_at,
            "cancel_requested_at": self.cancel_requested_at, "cancel_requested_by": self.cancel_requested_by,
        })
    }
}

/// One step of a job in the order it runs.
#[derive(Clone, Debug)]
pub struct Step {
    pub seq: i32,
    pub key: &'static str,
    pub label: &'static str,
    pub pauses: bool,
}
/// The steps a change (a build on Dev) goes through.
pub fn change_steps() -> Vec<Step> {
    steps(&[
        ("check", "Check the server", false),
        ("fetch", "Download the Platform and the Domains", false),
        (
            "build",
            "Build the new program (the instance keeps working)",
            false,
        ),
        (
            "backup",
            "Back up the database (the instance pauses from here)",
            true,
        ),
        ("migrate", "Update the database", false),
        ("switch", "Switch to the new program", false),
        ("verify", "Check that it works", false),
    ])
}
/// The steps an install of a tested release goes through.
pub fn install_steps() -> Vec<Step> {
    steps(&[
        ("fetch", "Copy the release and check it", false),
        (
            "backup",
            "Back up the database (the instance pauses from here)",
            true,
        ),
        ("migrate", "Update the database", false),
        ("switch", "Switch to the new program", false),
        ("verify", "Check that it works", false),
    ])
}
/// The steps an undo goes through; the restore only when a backup is restored.
pub fn undo_steps(restores_backup: bool) -> Vec<Step> {
    let mut list = vec![(
        "backup",
        "Back up the database (the instance pauses from here)",
        true,
    )];
    if restores_backup {
        list.push((
            "restore",
            "Restore the backup taken before the change",
            false,
        ));
    }
    list.push(("switch", "Switch back to the previous program", false));
    list.push(("verify", "Check that it works", false));
    steps(&list)
}
fn steps(list: &[(&'static str, &'static str, bool)]) -> Vec<Step> {
    list.iter()
        .enumerate()
        .map(|(index, (key, label, pauses))| Step {
            seq: index as i32 + 1,
            key,
            label,
            pauses: *pauses,
        })
        .collect()
}

pub async fn get(client: &impl GenericClient, tenant: i64, id: i64) -> Result<Option<Job>> {
    let row = client
        .query_opt(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND id=$2"),
            &[&tenant, &id],
        )
        .await?;
    Ok(row.as_ref().map(Job::from_row))
}
/// The job waiting or running, if any (at most one per server).
pub async fn active(client: &impl GenericClient, tenant: i64) -> Result<Option<Job>> {
    let row = client
        .query_opt(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND state IN ('queued','running') ORDER BY id DESC LIMIT 1"),
            &[&tenant],
        )
        .await?;
    Ok(row.as_ref().map(Job::from_row))
}
/// The newest jobs, newest first.
pub async fn recent(client: &impl GenericClient, tenant: i64, limit: i64) -> Result<Vec<Job>> {
    let rows = client
        .query(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 ORDER BY id DESC LIMIT $2"),
            &[&tenant, &limit],
        )
        .await?;
    Ok(rows.iter().map(Job::from_row).collect())
}
/// The newest job that finished well (a change or an install), which Undo
/// can take back.
pub async fn last_done(client: &impl GenericClient, tenant: i64) -> Result<Option<Job>> {
    let row = client
        .query_opt(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND state='done' ORDER BY id DESC LIMIT 1"),
            &[&tenant],
        )
        .await?;
    Ok(row.as_ref().map(Job::from_row))
}
/// The steps of a job as the status answer shows them.
pub async fn steps_json(client: &impl GenericClient, tenant: i64, job: i64) -> Result<Vec<Value>> {
    let rows = client
        .query(
            &format!("SELECT seq, key, label, pauses, state, detail, to_char(started_at AT TIME ZONE 'UTC', {TIME}), to_char(finished_at AT TIME ZONE 'UTC', {TIME}) FROM base_software_steps WHERE tenant_id=$1 AND job_id=$2 ORDER BY seq"),
            &[&tenant, &job],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "seq": row.get::<_, i32>(0), "key": row.get::<_, String>(1), "label": row.get::<_, String>(2),
                "pauses": row.get::<_, bool>(3), "state": row.get::<_, String>(4), "detail": with_loss(row.get::<_, Option<Value>>(5)),
                "started_at": row.get::<_, Option<String>>(6), "finished_at": row.get::<_, Option<String>>(7),
            })
        })
        .collect())
}
/// A step detail or result that carries the counts of what a restore lost
/// also carries the sentence that says it, from the one definition.
pub fn with_loss(value: Option<Value>) -> Option<Value> {
    value.map(|mut value| {
        if value["lost"].is_object() {
            value["loss"] = json!(crate::review::loss_sentence(&value["lost"]));
        }
        value
    })
}
/// Whether a running job has reached its pause: a cancel is then too late.
pub async fn past_pause(client: &impl GenericClient, tenant: i64, job: i64) -> Result<bool> {
    Ok(client
        .query_one(
            "SELECT EXISTS(SELECT 1 FROM base_software_steps WHERE tenant_id=$1 AND job_id=$2 AND pauses AND state<>'pending')",
            &[&tenant, &job],
        )
        .await?
        .get(0))
}
/// The release a running change built, when the build is done and no later
/// step has started: the job is parked for the installer. `None` otherwise.
pub async fn built_release(
    client: &impl GenericClient,
    tenant: i64,
    job: i64,
) -> Result<Option<i64>> {
    let steps = steps_json(client, tenant, job).await?;
    let built = steps
        .iter()
        .find(|s| s["key"] == "build" && s["state"] == "done")
        .and_then(|s| s["detail"]["release"].as_i64());
    let later_started = steps
        .iter()
        .skip_while(|s| s["key"] != "build")
        .skip(1)
        .any(|s| s["state"] != "pending");
    Ok(built.filter(|_| !later_started))
}
/// When the backup that Undo would restore was taken: the job's result
/// names it (`backup_at`); otherwise the backup step's start is it. `None`
/// when no time is known.
pub async fn backup_time(
    client: &impl GenericClient,
    tenant: i64,
    job: &Job,
) -> Result<Option<String>> {
    if let Some(at) = job.result.as_ref().and_then(|r| r["backup_at"].as_str()) {
        return Ok(Some(at.to_owned()));
    }
    Ok(client
        .query_opt(
            &format!("SELECT to_char(started_at AT TIME ZONE 'UTC', {TIME}) FROM base_software_steps WHERE tenant_id=$1 AND job_id=$2 AND key='backup' AND started_at IS NOT NULL"),
            &[&tenant, &job.id],
        )
        .await?
        .map(|row| row.get::<_, String>(0)))
}
/// The plug-ins switched on, as the status answer shows them.
pub async fn plugins_json(client: &impl GenericClient, tenant: i64) -> Result<Vec<Value>> {
    let rows = client
        .query(
            &format!("SELECT plugin_id, name, publisher, version, source, enabled, job_id, to_char(updated_at AT TIME ZONE 'UTC', {TIME}) FROM base_software_plugins WHERE tenant_id=$1 ORDER BY plugin_id"),
            &[&tenant],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<_, String>(0), "name": row.get::<_, String>(1), "publisher": row.get::<_, Option<String>>(2),
                "version": row.get::<_, Option<String>>(3), "source": row.get::<_, String>(4), "enabled": row.get::<_, bool>(5),
                "job_id": row.get::<_, Option<i64>>(6), "updated_at": row.get::<_, String>(7),
            })
        })
        .collect())
}
/// The plug-in ids switched on now.
pub async fn enabled_plugins(client: &impl GenericClient, tenant: i64) -> Result<Vec<String>> {
    let rows = client
        .query(
            "SELECT plugin_id FROM base_software_plugins WHERE tenant_id=$1 AND enabled ORDER BY plugin_id",
            &[&tenant],
        )
        .await?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

/// What a new job is made of.
pub struct NewJob {
    pub kind: &'static str,
    pub request: Value,
    pub review: Value,
    pub undo_of: Option<i64>,
    pub switch: String,
    pub steps: Vec<Step>,
}
/// Queues one job with its pending steps and wakes the worker. Serialized
/// per server: a second request while one waits or runs is refused
/// (`Error::Conflict`); the caller has already told the person which job.
pub async fn queue(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    user: i64,
    new: NewJob,
) -> Result<Job> {
    tx.mutate(move |sql| {
        Box::pin(async move {
            sql.execute(
                "SELECT pg_advisory_xact_lock(hashtextextended('base/software.jobs', 0))",
                &[],
            )
            .await?;
            let busy = sql
                .query_opt(
                    "SELECT id FROM base_software_jobs WHERE tenant_id=$1 AND state IN ('queued','running') LIMIT 1",
                    &[&tenant],
                )
                .await?;
            if busy.is_some() {
                return Err(Error::Conflict);
            }
            let row = sql
                .query_one(
                    &format!("INSERT INTO base_software_jobs(tenant_id, kind, request, review, undo_of, switch, requested_by) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING {JOB_COLUMNS}"),
                    &[&tenant, &new.kind, &new.request, &new.review, &new.undo_of, &new.switch, &user],
                )
                .await?;
            let job = Job::from_row(&row);
            for step in &new.steps {
                sql.execute(
                    "INSERT INTO base_software_steps(tenant_id, job_id, seq, key, label, pauses) VALUES($1,$2,$3,$4,$5,$6)",
                    &[&tenant, &job.id, &step.seq, &step.key, &step.label, &step.pauses],
                )
                .await?;
            }
            sql.execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &job.id.to_string()])
                .await?;
            // An empty `old` records the job as created (Audit's rule).
            let change = Change {
                kind: RECORD.into(),
                id: job.id,
                old: json!({}),
                new: job.json(),
            };
            Ok((job, vec![change]))
        })
    })
    .await
}
/// Jobs a worker finished whose outcome is not in the audit trail yet, as
/// (tenant, job) pairs, oldest first. The worker wrote them outside the
/// audited boundary; `settle` records each one.
pub async fn unsettled(client: &impl GenericClient) -> Result<Vec<(i64, Job)>> {
    let rows = client
        .query(
            &format!("SELECT tenant_id, {JOB_COLUMNS} FROM base_software_jobs WHERE worker IS NOT NULL AND finished_at IS NOT NULL AND settled_at IS NULL AND state IN ('done','failed','cancelled','interrupted') ORDER BY id LIMIT 20"),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| (row.get(0), Job::from_row_offset(row, 1)))
        .collect())
}
/// Records a worker's outcome as the audited change it is: from the job as
/// queued to the job as finished. Done once per job (`settled_at`).
pub async fn settle(tx: &mut MutationTransaction<'_>, tenant: i64, job: &Job) -> Result<bool> {
    let job = job.clone();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let marked = sql
                .execute(
                    "UPDATE base_software_jobs SET settled_at=now() WHERE tenant_id=$1 AND id=$2 AND settled_at IS NULL",
                    &[&tenant, &job.id],
                )
                .await?;
            if marked == 0 {
                return Ok((false, Vec::new()));
            }
            let mut queued = job.json();
            queued["state"] = json!("queued");
            queued["result"] = Value::Null;
            queued["worker"] = Value::Null;
            queued["started_at"] = Value::Null;
            queued["finished_at"] = Value::Null;
            let change = Change {
                kind: RECORD.into(),
                id: job.id,
                old: queued,
                new: job.json(),
            };
            Ok((true, vec![change]))
        })
    })
    .await
}
/// What was entered after an instant, from the audit trail across every
/// company on this server: what a restore of a backup taken then would
/// lose. Server-wide on purpose: one database holds every company, so the
/// loss is not one tenant's. Software jobs are left out, because they are
/// what asked.
pub async fn changes_since(client: &impl GenericClient, since: &str) -> Result<Value> {
    let rows = client
        .query(
            "SELECT auditable_type, count(*) FROM base_audit_mutations WHERE occurred_at > $1::text::timestamptz AND auditable_type NOT LIKE 'base/software.%' GROUP BY 1 ORDER BY 2 DESC, 1",
            &[&since],
        )
        .await?;
    let by_kind: Vec<Value> = rows
        .iter()
        .map(|row| json!({"kind": row.get::<_, String>(0), "changes": row.get::<_, i64>(1)}))
        .collect();
    let total: i64 = rows.iter().map(|row| row.get::<_, i64>(1)).sum();
    Ok(json!({"since": since, "changes": total, "by_kind": by_kind}))
}
/// Gives a parked job its go-ahead: `switch` turns `auto`, which is the
/// signal the installer waits for. The caller has checked the job is parked.
pub async fn go(tx: &mut MutationTransaction<'_>, tenant: i64, id: i64) -> Result<Job> {
    tx.mutate(move |sql| {
        Box::pin(async move {
            let before = sql
                .query_opt(
                    &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE"),
                    &[&tenant, &id],
                )
                .await?
                .map(|row| Job::from_row(&row))
                .ok_or(Error::NotFound)?;
            if before.state != "running"
                || before.switch != "ask"
                || before.cancel_requested_at.is_some()
            {
                return Err(Error::Conflict);
            }
            let row = sql
                .query_one(
                    &format!("UPDATE base_software_jobs SET switch='auto' WHERE tenant_id=$1 AND id=$2 RETURNING {JOB_COLUMNS}"),
                    &[&tenant, &id],
                )
                .await?;
            let after = Job::from_row(&row);
            sql.execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &id.to_string()])
                .await?;
            let change = Change {
                kind: RECORD.into(),
                id,
                old: before.json(),
                new: after.json(),
            };
            Ok((after, vec![change]))
        })
    })
    .await
}
/// Cancels a job: one still waiting is cancelled at once; one running is
/// asked to stop, and the worker stops before the pause. The caller has
/// checked the job can still be cancelled.
pub async fn cancel(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<Job> {
    tx.mutate(move |sql| {
        Box::pin(async move {
            let before = sql
                .query_opt(
                    &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE"),
                    &[&tenant, &id],
                )
                .await?
                .map(|row| Job::from_row(&row))
                .ok_or(Error::NotFound)?;
            let row = match before.state.as_str() {
                "queued" => {
                    sql.query_one(
                        &format!("UPDATE base_software_jobs SET state='cancelled', finished_at=now(), cancel_requested_at=now(), cancel_requested_by=$3 WHERE tenant_id=$1 AND id=$2 RETURNING {JOB_COLUMNS}"),
                        &[&tenant, &id, &user],
                    )
                    .await?
                }
                "running"
                    if before.cancel_requested_at.is_none()
                        && !past_pause(sql, tenant, id).await? =>
                {
                    sql.query_one(
                        &format!("UPDATE base_software_jobs SET cancel_requested_at=now(), cancel_requested_by=$3 WHERE tenant_id=$1 AND id=$2 RETURNING {JOB_COLUMNS}"),
                        &[&tenant, &id, &user],
                    )
                    .await?
                }
                _ => return Err(Error::Conflict),
            };
            let after = Job::from_row(&row);
            if after.state == "cancelled" {
                sql.execute(
                    "UPDATE base_software_steps SET state='skipped' WHERE tenant_id=$1 AND job_id=$2 AND state='pending'",
                    &[&tenant, &id],
                )
                .await?;
            }
            sql.execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &id.to_string()])
                .await?;
            let change = Change {
                kind: RECORD.into(),
                id,
                old: before.json(),
                new: after.json(),
            };
            Ok((after, vec![change]))
        })
    })
    .await
}
