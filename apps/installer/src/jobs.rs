//! The job and step rows of `base/software`, read and written the way its
//! README's "How a worker plugs in" describes. Every statement binds the
//! tenant the job was queued in.
use serde_json::{json, Value};
use tokio_postgres::{Client, GenericClient};

/// The channel `base/software` notifies on; the payload is the job id.
pub const CHANNEL: &str = "bilibi_software_jobs";
const TIME: &str = "'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'";
const JOB_COLUMNS: &str = "id, tenant_id, kind, request, undo_of, result";

#[derive(Clone, Debug)]
pub struct Job {
    pub id: i64,
    pub tenant: i64,
    pub kind: String,
    pub request: Value,
    pub undo_of: Option<i64>,
    pub result: Option<Value>,
}
impl Job {
    fn from_row(row: &tokio_postgres::Row) -> Self {
        Self {
            id: row.get(0),
            tenant: row.get(1),
            kind: row.get(2),
            request: row.get(3),
            undo_of: row.get(4),
            result: row.get(5),
        }
    }
}

#[derive(Clone, Debug)]
pub struct Step {
    pub key: String,
    pub label: String,
    pub state: String,
    pub detail: Value,
}

pub async fn get(
    client: &impl GenericClient,
    tenant: i64,
    id: i64,
) -> Result<Option<Job>, tokio_postgres::Error> {
    let row = client
        .query_opt(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND id=$2"),
            &[&tenant, &id],
        )
        .await?;
    Ok(row.as_ref().map(Job::from_row))
}
/// The job this worker was running when it was last stopped, if any.
pub async fn mine_running(
    client: &impl GenericClient,
    worker: &str,
) -> Result<Option<Job>, tokio_postgres::Error> {
    let row = client
        .query_opt(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE state='running' AND worker=$1 ORDER BY id LIMIT 1"),
            &[&worker],
        )
        .await?;
    Ok(row.as_ref().map(Job::from_row))
}
/// The oldest job waiting that an installer carries out: an install or an
/// undo. A change (a build) waits for the builder.
pub async fn next_waiting(
    client: &impl GenericClient,
) -> Result<Option<Job>, tokio_postgres::Error> {
    let row = client
        .query_opt(
            &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE state='queued' AND kind IN ('install','undo') ORDER BY id LIMIT 1"),
            &[],
        )
        .await?;
    Ok(row.as_ref().map(Job::from_row))
}
/// Claims a waiting job. False means someone else has it, or it was cancelled.
pub async fn claim(
    client: &impl GenericClient,
    job: &Job,
    worker: &str,
) -> Result<bool, tokio_postgres::Error> {
    let changed = client
        .execute(
            "UPDATE base_software_jobs SET state='running', worker=$3, started_at=now() WHERE tenant_id=$1 AND id=$2 AND state='queued'",
            &[&job.tenant, &job.id, &worker],
        )
        .await?;
    Ok(changed == 1)
}
pub async fn steps(
    client: &impl GenericClient,
    job: &Job,
) -> Result<Vec<Step>, tokio_postgres::Error> {
    let rows = client
        .query(
            "SELECT key, label, state, detail FROM base_software_steps WHERE tenant_id=$1 AND job_id=$2 ORDER BY seq",
            &[&job.tenant, &job.id],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| Step {
            key: row.get(0),
            label: row.get(1),
            state: row.get(2),
            detail: row.get::<_, Option<Value>>(3).unwrap_or(Value::Null),
        })
        .collect())
}
/// Whether the owner asked the job to stop, and which switch mode it has
/// now (`auto` or `ask`).
pub async fn watch(
    client: &impl GenericClient,
    job: &Job,
) -> Result<(bool, String), tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT cancel_requested_at IS NOT NULL, switch FROM base_software_jobs WHERE tenant_id=$1 AND id=$2",
            &[&job.tenant, &job.id],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}
pub async fn step_set(
    client: &impl GenericClient,
    job: &Job,
    key: &str,
    state: &str,
    detail: &Value,
) -> Result<(), tokio_postgres::Error> {
    let sql = match state {
        "running" => "UPDATE base_software_steps SET state='running', started_at=now(), finished_at=NULL, detail=$4 WHERE tenant_id=$1 AND job_id=$2 AND key=$3",
        "pending" => "UPDATE base_software_steps SET detail=$4 WHERE tenant_id=$1 AND job_id=$2 AND key=$3",
        _ => "UPDATE base_software_steps SET state=$5, finished_at=now(), detail=$4 WHERE tenant_id=$1 AND job_id=$2 AND key=$3",
    };
    if state == "running" || state == "pending" {
        client
            .execute(sql, &[&job.tenant, &job.id, &key, detail])
            .await?;
    } else {
        client
            .execute(sql, &[&job.tenant, &job.id, &key, detail, &state])
            .await?;
    }
    Ok(())
}
/// The steps not reached are skipped once the job has failed or stopped.
pub async fn skip_pending(
    client: &impl GenericClient,
    job: &Job,
) -> Result<(), tokio_postgres::Error> {
    client
        .execute(
            "UPDATE base_software_steps SET state='skipped' WHERE tenant_id=$1 AND job_id=$2 AND state IN ('pending','running')",
            &[&job.tenant, &job.id],
        )
        .await?;
    Ok(())
}
/// The job's final state and result, then the wake-up that tells the app.
pub async fn finish(
    client: &impl GenericClient,
    job: &Job,
    state: &str,
    result: &Value,
) -> Result<(), tokio_postgres::Error> {
    client
        .execute(
            "UPDATE base_software_jobs SET state=$3, result=$4, finished_at=now() WHERE tenant_id=$1 AND id=$2",
            &[&job.tenant, &job.id, &state, result],
        )
        .await?;
    client
        .execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &job.id.to_string()])
        .await?;
    Ok(())
}
/// Starts the step that begins the pause, unless a cancel landed first. In
/// one transaction with the job row locked: a cancel either arrives before
/// this and stops the job here, or the app refuses it as too late.
pub async fn begin_pause(
    client: &mut Client,
    job: &Job,
    key: &str,
    detail: &Value,
) -> Result<bool, tokio_postgres::Error> {
    let tx = client.transaction().await?;
    let cancelled: bool = tx
        .query_one(
            "SELECT cancel_requested_at IS NOT NULL FROM base_software_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
            &[&job.tenant, &job.id],
        )
        .await?
        .get(0);
    if cancelled {
        skip_pending(&tx, job).await?;
        let result = json!({"message": "Stopped before the pause, as asked. Nothing changed on this instance."});
        finish(&tx, job, "cancelled", &result).await?;
    } else {
        step_set(&tx, job, key, "running", detail).await?;
    }
    tx.commit().await?;
    Ok(!cancelled)
}

/// How many migrations the database records; zero before the first one.
pub async fn ledger_count(client: &impl GenericClient) -> Result<i64, tokio_postgres::Error> {
    let row = client
        .query_one(
            "SELECT CASE WHEN to_regclass('bilibi_migrations') IS NULL THEN 0 ELSE (SELECT count(*) FROM bilibi_migrations) END",
            &[],
        )
        .await?;
    Ok(row.get(0))
}
pub async fn database_size(client: &impl GenericClient) -> Result<i64, tokio_postgres::Error> {
    let row = client
        .query_one("SELECT pg_database_size(current_database())", &[])
        .await?;
    Ok(row.get(0))
}
/// What was entered after an instant, from the audit trail: the number of
/// recorded changes and how they split by kind of record. Software jobs are
/// left out, because they are what asked for this.
pub async fn changes_since(
    client: &impl GenericClient,
    since: &str,
) -> Result<Value, tokio_postgres::Error> {
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
/// The sentence the person reads about what a restore loses.
pub fn loss_sentence(lost: &Value) -> String {
    let since = lost["since"].as_str().unwrap_or("the backup");
    let total = lost["changes"].as_i64().unwrap_or(0);
    if total == 0 {
        return format!("Nothing was entered after the backup at {since}, so nothing is lost.");
    }
    let kinds: Vec<String> = lost["by_kind"]
        .as_array()
        .map(|list| {
            list.iter()
                .take(5)
                .filter_map(|item| {
                    Some(format!(
                        "{} to {}",
                        item["changes"].as_i64()?,
                        item["kind"].as_str()?
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    format!(
        "Everything entered after the backup at {since} is lost: {total} change{} ({}).",
        if total == 1 { "" } else { "s" },
        kinds.join(", ")
    )
}
/// Now, as the UTC instant the job rows and results carry.
pub async fn now(client: &impl GenericClient) -> Result<String, tokio_postgres::Error> {
    let row = client
        .query_one(
            &format!("SELECT to_char(now() AT TIME ZONE 'UTC', {TIME})"),
            &[],
        )
        .await?;
    Ok(row.get(0))
}

/// The software history (jobs, steps, plug-ins) as rows of JSON, taken
/// before a restore so the restore cannot erase what it is part of.
pub struct History(Vec<(&'static str, Vec<Value>)>);
const HISTORY_TABLES: [&str; 3] = [
    "base_software_jobs",
    "base_software_steps",
    "base_software_plugins",
];
pub async fn history(client: &impl GenericClient) -> Result<History, tokio_postgres::Error> {
    let mut tables = Vec::new();
    for table in HISTORY_TABLES {
        let order = if table == "base_software_plugins" {
            "tenant_id, plugin_id"
        } else {
            "id"
        };
        let rows = client
            .query(
                &format!("SELECT row_to_json(t)::jsonb FROM {table} t ORDER BY {order}"),
                &[],
            )
            .await?;
        tables.push((
            table,
            rows.iter().map(|row| row.get::<_, Value>(0)).collect(),
        ));
    }
    Ok(History(tables))
}
/// Puts the history back after a restore, into whatever shape the restored
/// tables have (a column the restored release lacks is left out). Returns
/// the rows carried and the rows that did not fit, in plain words.
pub async fn restore_history(
    client: &impl GenericClient,
    history: &History,
) -> Result<(i64, Vec<String>), tokio_postgres::Error> {
    let mut carried = 0;
    let mut problems = Vec::new();
    let mut present = Vec::new();
    for (table, rows) in &history.0 {
        let exists: bool = client
            .query_one("SELECT to_regclass($1) IS NOT NULL", &[table])
            .await?
            .get(0);
        if exists {
            present.push(*table);
        } else {
            problems.push(format!(
                "{table} does not exist in the restored database; its {} rows were not carried",
                rows.len()
            ));
        }
    }
    // Plug-ins and steps point at jobs, so they are emptied first and refilled last.
    for table in present.iter().rev() {
        client.execute(&format!("DELETE FROM {table}"), &[]).await?;
    }
    for (table, rows) in &history.0 {
        if !present.contains(table) {
            continue;
        }
        let columns: Vec<String> = client
            .query(
                "SELECT column_name FROM information_schema.columns WHERE table_schema='public' AND table_name=$1",
                &[table],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        for row in rows {
            let kept: serde_json::Map<String, Value> = row
                .as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .filter(|(name, _)| columns.contains(name))
                        .map(|(name, value)| (name.clone(), value.clone()))
                        .collect()
                })
                .unwrap_or_default();
            let inserted = client
                .execute(
                    &format!("INSERT INTO {table} OVERRIDING SYSTEM VALUE SELECT * FROM jsonb_populate_record(NULL::{table}, $1)"),
                    &[&Value::Object(kept)],
                )
                .await;
            match inserted {
                Ok(_) => carried += 1,
                Err(error) => problems.push(format!(
                    "{table}: one row was not carried ({})",
                    short(&error)
                )),
            }
        }
        if *table != "base_software_plugins" {
            client
                .execute(
                    &format!("SELECT setval(pg_get_serial_sequence('{table}', 'id'), coalesce((SELECT max(id) FROM {table}), 0) + 1, false)"),
                    &[],
                )
                .await?;
        }
    }
    Ok((carried, problems))
}
/// A database error as one plain line, without the statement.
pub fn short(error: &tokio_postgres::Error) -> String {
    error
        .as_db_error()
        .map(|db| db.message().to_owned())
        .unwrap_or_else(|| error.to_string())
}
