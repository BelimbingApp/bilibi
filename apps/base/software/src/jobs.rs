//! The job rows: what a server owner asked for and what the worker reported.
//! Every statement binds the operator tenant the request was made in.
use bilibi_database::{Change, Error, MutationTransaction, Result};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

/// The PostgreSQL channel the installer listens on; the payload is
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
pub fn install_steps(downloads: bool) -> Vec<Step> {
    let mut list: Vec<(&'static str, &'static str, bool)> = Vec::new();
    if downloads {
        list.push(("download", "Download the release from GitHub", false));
    }
    list.extend([
        ("fetch", "Check the release and copy it into place", false),
        (
            "backup",
            "Back up the database (the instance pauses from here)",
            true,
        ),
        ("migrate", "Update the database", false),
        ("switch", "Switch to the new program", false),
        ("verify", "Check that it works", false),
    ]);
    steps(&list)
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
/// The newest job that finished well (an install), which Undo
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
/// Backup evidence for the recent jobs, and how many backups the installer
/// keeps (as the newest backup step recorded it). A path records where the
/// installer wrote the file; it does not promise that retention has kept it.
pub async fn backups_json(
    client: &impl GenericClient,
    tenant: i64,
) -> Result<(Vec<Value>, Option<u64>)> {
    let rows = client.query(
        "SELECT j.id, j.kind, s.detail FROM (SELECT id, tenant_id, kind FROM base_software_jobs WHERE tenant_id=$1 ORDER BY id DESC LIMIT 10) j JOIN base_software_steps s ON s.tenant_id=j.tenant_id AND s.job_id=j.id WHERE s.tenant_id=$1 AND s.key='backup' AND s.state='done' ORDER BY j.id DESC",
        &[&tenant],
    ).await?;
    let mut keep = None;
    let list = rows
        .iter()
        .map(|row| {
            let detail: Option<Value> = row.get(2);
            let detail = detail.unwrap_or(Value::Null);
            keep = keep.or(detail["keep_backups"].as_u64());
            json!({"job": row.get::<_, i64>(0), "kind": row.get::<_, String>(1),
            "file": detail["backup"], "at": detail["backup_at"], "bytes": detail["bytes"]})
        })
        .collect();
    Ok((list, keep))
}
/// The one statement of the backup rules, for the page, the card and the
/// assistant. The number kept is the installer's, as its backup step recorded it.
pub fn backup_rule(keep: Option<u64>) -> String {
    let kept = keep.map_or_else(
        || "removes the oldest ones".to_owned(),
        |keep| format!("keeps only the newest {keep} and removes the older ones"),
    );
    format!("Bilibi backs up the database before every install and undo. After a successful install or undo it {kept}, so a listed file may no longer exist. Undo switches back to the previous program. Only if the install changed the database structure does Undo also restore its backup, which loses data entered since. Undo takes a new backup first.")
}
/// Hides where a backup file is from anyone who cannot manage software:
/// they see that the backup exists, when it was taken and its size. The
/// path goes from the `backup` fields and from every message that names it.
pub fn hide_backup_files(job: &mut Value, backups: &mut [Value]) {
    let mut paths = Vec::new();
    collect_backup_paths(job, &mut paths);
    for backup in backups.iter_mut() {
        if let Some(file) = backup["file"].as_str() {
            paths.push(file.to_owned());
        }
        backup["file"] = Value::Null;
    }
    paths.sort_unstable_by_key(|path| std::cmp::Reverse(path.len()));
    scrub_backup_paths(job, &paths);
}
const BACKUP_FILE_WORDS: &str = "a backup file on this server";
/// Replaces every path under a `backups/job-<n>` folder in a message, whether
/// or not the backup was recorded: a failure can name the folder or a file in it.
fn hide_backup_locations(text: &str) -> String {
    const MARKER: &str = "backups/job-";
    let opens = |c: char| c.is_whitespace() || "\"'(".contains(c);
    let closes = |c: char| c.is_whitespace() || "\"')`,;".contains(c);
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(MARKER) {
        let start = rest[..at].rfind(opens).map_or(0, |i| {
            i + rest[i..].chars().next().map_or(1, char::len_utf8)
        });
        let mut end = rest[at..].find(closes).map_or(rest.len(), |i| at + i);
        while end > at && rest[..end].ends_with(['.', ':']) {
            end -= 1;
        }
        out.push_str(&rest[..start]);
        out.push_str(BACKUP_FILE_WORDS);
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}
fn collect_backup_paths(value: &Value, paths: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if let Some(path) = map.get("backup").and_then(Value::as_str) {
                paths.push(path.to_owned());
            }
            map.values()
                .for_each(|item| collect_backup_paths(item, paths));
        }
        Value::Array(list) => list
            .iter()
            .for_each(|item| collect_backup_paths(item, paths)),
        _ => {}
    }
}
fn scrub_backup_paths(value: &mut Value, paths: &[String]) {
    match value {
        Value::String(text) => {
            for path in paths {
                *text = text.replace(path.as_str(), BACKUP_FILE_WORDS);
            }
            *text = hide_backup_locations(text);
        }
        Value::Object(map) => {
            map.remove("backup");
            map.values_mut()
                .for_each(|item| scrub_backup_paths(item, paths));
        }
        Value::Array(list) => list
            .iter_mut()
            .for_each(|item| scrub_backup_paths(item, paths)),
        _ => {}
    }
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
/// The release a job's first part made, when that part is done and no later
/// step has started: an install's copy. The job is then
/// parked for the pause. `None` for any other job or moment.
pub async fn parked_release(
    client: &impl GenericClient,
    tenant: i64,
    job: &Job,
) -> Result<Option<i64>> {
    let first = match job.kind.as_str() {
        "install" => "fetch",
        _ => return Ok(None),
    };
    let steps = steps_json(client, tenant, job.id).await?;
    let part = steps
        .iter()
        .find(|s| s["key"] == first && s["state"] == "done");
    let made = part.and(job.request["release"].as_i64());
    let later_started = steps
        .iter()
        .skip_while(|s| s["key"] != first)
        .skip(1)
        .any(|s| s["state"] != "pending");
    Ok(made.filter(|_| !later_started))
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
/// The add-ons switched on, as the status answer shows them.
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
/// The add-on ids switched on now.
pub async fn enabled_plugins(client: &impl GenericClient, tenant: i64) -> Result<Vec<String>> {
    let rows = client
        .query(
            "SELECT plugin_id FROM base_software_plugins WHERE tenant_id=$1 AND enabled ORDER BY plugin_id",
            &[&tenant],
        )
        .await?;
    Ok(rows.iter().map(|row| row.get(0)).collect())
}

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
                    "UPDATE base_software_steps SET state='skipped' WHERE tenant_id=$1 AND job_id=$2 AND state IN ('pending','running')",
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

/// Install jobs waiting for this program to download their release: queued,
/// with a `download` step still pending, as (tenant, job) pairs, oldest
/// first. The installer leaves such a job alone until the step is done.
pub async fn awaiting_download(client: &impl GenericClient) -> Result<Vec<(i64, Job)>> {
    let rows = client
        .query(
            &format!("SELECT tenant_id, {JOB_COLUMNS} FROM base_software_jobs WHERE state='queued' AND kind='install' AND EXISTS (SELECT 1 FROM base_software_steps s WHERE s.tenant_id=base_software_jobs.tenant_id AND s.job_id=base_software_jobs.id AND s.key='download' AND s.state IN ('pending','running')) ORDER BY id LIMIT 5"),
            &[],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| (row.get(0), Job::from_row_offset(row, 1)))
        .collect())
}
/// Reports progress on the `download` step: the program is the worker of
/// that one step, as the installer is of the others.
pub async fn download_step(
    client: &impl GenericClient,
    tenant: i64,
    job: i64,
    state: &str,
    detail: &Value,
) -> Result<()> {
    let sql = match state {
        "running" => "UPDATE base_software_steps SET state='running', started_at=coalesce(started_at, now()), detail=$3 WHERE tenant_id=$1 AND job_id=$2 AND key='download' AND EXISTS (SELECT 1 FROM base_software_jobs j WHERE j.tenant_id=$1 AND j.id=$2 AND j.state='queued')",
        _ => "UPDATE base_software_steps SET state=$4, finished_at=now(), detail=$3 WHERE tenant_id=$1 AND job_id=$2 AND key='download' AND EXISTS (SELECT 1 FROM base_software_jobs j WHERE j.tenant_id=$1 AND j.id=$2 AND j.state='queued')",
    };
    if state == "running" {
        client.execute(sql, &[&tenant, &job, detail]).await?;
    } else {
        client
            .execute(sql, &[&tenant, &job, detail, &state])
            .await?;
    }
    Ok(())
}
/// Marks the download done and wakes the installer, unless the job was
/// cancelled meanwhile: then nothing is marked and the caller discards
/// what it downloaded. Under the job's row lock, so a cancel lands before
/// or after, never between.
pub async fn download_done(
    client: &mut tokio_postgres::Client,
    tenant: i64,
    job: i64,
    detail: &Value,
) -> Result<bool> {
    let tx = client.transaction().await?;
    let queued: bool = tx
        .query_one(
            "SELECT state = 'queued' FROM base_software_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
            &[&tenant, &job],
        )
        .await?
        .get(0);
    if queued {
        download_step(&tx, tenant, job, "done", detail).await?;
        tx.execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &job.to_string()])
            .await?;
    }
    tx.commit().await?;
    Ok(queued)
}
/// Ends a job this program could not get the release for: it never left the
/// queue, so nothing changed on the instance. Recorded like a worker's
/// outcome, so `settle` puts it in the audit trail and tells the person.
pub async fn fail_queued(
    client: &impl GenericClient,
    tenant: i64,
    job: i64,
    worker: &str,
    message: &str,
) -> Result<()> {
    let result = json!({"migrated": false, "message": message});
    client
        .execute(
            "UPDATE base_software_steps SET state='skipped' WHERE tenant_id=$1 AND job_id=$2 AND state IN ('pending','running')",
            &[&tenant, &job],
        )
        .await?;
    client
        .execute(
            "UPDATE base_software_jobs SET state='failed', worker=$3, started_at=coalesce(started_at, now()), finished_at=now(), result=$4 WHERE tenant_id=$1 AND id=$2 AND state='queued'",
            &[&tenant, &job, &worker, &result],
        )
        .await?;
    Ok(())
}
