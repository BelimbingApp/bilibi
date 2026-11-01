//! What Schedule adds to a scheduled job and how an owner's handler runs it.
//!
//! The claim puts bounded metadata under [`RESERVED_ARGUMENT`]. A scheduled
//! worker's `validate` must go through [`validate`] so the owner sees only its
//! own arguments, and its `Handler::perform` through [`Schedule::run`], which
//! re-checks the definition, review, suppression and the durable occurrence
//! under the per-key lock before the owner's code starts, then records the
//! attempt. Boot refuses a scheduled worker whose validator drops the metadata.
use crate::{
    definition::{valid_key, Reach},
    lock,
    occurrence::{self, Trigger},
    Eligibility, Schedule, RESERVED_ARGUMENT,
};
use bilibi_database::{Error, Result};
use bilibi_queue::{Execution, Outcome};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{future::Future, time::Instant};

/// Claim facts carried in the reserved argument; validated again at execution.
#[derive(Clone, Debug)]
pub struct Metadata {
    pub key: String,
    pub name: String,
    /// The cron the occurrence came from; `None` for a manual run.
    pub expression: Option<String>,
    pub fingerprint: String,
    pub intended_at: Timestamp,
    pub trigger: Trigger,
    pub triggered_by_user_id: Option<i64>,
    pub triggered_by_name: Option<String>,
}
impl Metadata {
    pub(crate) fn to_json(&self) -> Value {
        json!({
            "key": self.key, "name": self.name, "expression": self.expression,
            "fingerprint": self.fingerprint, "intended_at": self.intended_at.to_string(),
            "trigger": self.trigger.as_str(),
            "triggered_by_user_id": self.triggered_by_user_id, "triggered_by_name": self.triggered_by_name,
        })
    }
    pub fn parse(value: &Value) -> Result<Self> {
        let object = value.as_object().ok_or(Error::Invalid)?;
        let text = |name: &str, maximum: usize| -> Result<String> {
            let text = object
                .get(name)
                .and_then(Value::as_str)
                .ok_or(Error::Invalid)?;
            if text.is_empty() || text.chars().count() > maximum {
                return Err(Error::Invalid);
            }
            Ok(text.to_owned())
        };
        let key = text("key", 255)?;
        if !valid_key(&key) {
            return Err(Error::Invalid);
        }
        let fingerprint = text("fingerprint", 64)?;
        if fingerprint.len() != 64 || !fingerprint.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Invalid);
        }
        let expression = match object.get("expression") {
            None | Some(Value::Null) => None,
            Some(_) => Some(text("expression", 64)?),
        };
        let triggered_by_name = match object.get("triggered_by_name") {
            None | Some(Value::Null) => None,
            Some(_) => Some(text("triggered_by_name", 255)?),
        };
        let triggered_by_user_id = match object.get("triggered_by_user_id") {
            None | Some(Value::Null) => None,
            Some(id) => Some(id.as_i64().filter(|id| *id > 0).ok_or(Error::Invalid)?),
        };
        Ok(Self {
            name: text("name", 255)?,
            intended_at: occurrence::micros(
                text("intended_at", 64)?
                    .parse()
                    .map_err(|_| Error::Invalid)?,
            ),
            trigger: Trigger::parse(&text("trigger", 16)?).ok_or(Error::Invalid)?,
            key,
            expression,
            fingerprint,
            triggered_by_user_id,
            triggered_by_name,
        })
    }
}
/// Splits a job's arguments into the owner's part and Schedule's metadata.
pub fn split(args: Value) -> Result<(Value, Option<Metadata>)> {
    let mut object = match args {
        Value::Object(object) => object,
        _ => return Err(Error::Invalid),
    };
    let metadata = object
        .remove(RESERVED_ARGUMENT)
        .map(|value| Metadata::parse(&value))
        .transpose()?;
    Ok((Value::Object(object), metadata))
}
/// Use as a scheduled worker's `validate`:
/// `validate: |args| bilibi_schedule::worker::validate(args, mine)`.
/// The owner's validator sees only its own arguments and may normalise them;
/// the metadata is validated here and put back unchanged. Arguments without
/// metadata pass (boot validates the contributed arguments this way) but
/// [`Schedule::run`] refuses to execute them.
pub fn validate(args: Value, owner: fn(Value) -> Result<Value>) -> Result<Value> {
    let (business, metadata) = split(args)?;
    let mut normalized = match owner(business)? {
        Value::Object(object) => object,
        _ => return Err(Error::Invalid),
    };
    if let Some(metadata) = metadata {
        if normalized.contains_key(RESERVED_ARGUMENT) {
            return Err(Error::Invalid);
        }
        normalized.insert(RESERVED_ARGUMENT.into(), metadata.to_json());
    }
    Ok(Value::Object(normalized))
}
/// The metadata boot attaches to prove a worker's validator keeps it.
pub(crate) fn probe(key: &str, name: &str, fingerprint: &str) -> Value {
    Metadata {
        key: key.into(),
        name: name.into(),
        expression: None,
        fingerprint: fingerprint.into(),
        intended_at: Timestamp::UNIX_EPOCH,
        trigger: Trigger::Manual,
        triggered_by_user_id: None,
        triggered_by_name: None,
    }
    .to_json()
}

impl Schedule {
    /// Wrap the owner's handler body. Before it runs: the definition must still
    /// exist with this fingerprint, be enabled and unsuppressed, and the
    /// durable occurrence must be unfinished and bound to this job; otherwise
    /// the occurrence is failed and the job cancelled with a bounded code.
    /// Each attempt is a run row; the occurrence finishes when the outcome is
    /// terminal (success, cancel, or the last retry). Recording is best effort
    /// and never changes the owner's outcome.
    pub async fn run<F, Fut>(&self, execution: &Execution, args: Value, owner: F) -> Outcome
    where
        F: FnOnce(Value) -> Fut,
        Fut: Future<Output = Outcome>,
    {
        let (business, metadata) = match split(args) {
            Ok((business, Some(metadata))) => (business, metadata),
            Ok((_, None)) => return Outcome::Cancel("missing_schedule_metadata"),
            Err(_) => return Outcome::Cancel("invalid_schedule_metadata"),
        };
        // An each-tenant definition's job is one tenant's run: it binds to that
        // tenant's run row, not to the operator-tenant occurrence.
        let each_tenant = self
            .definitions
            .get(metadata.key.as_str())
            .is_some_and(|item| item.definition.reach == Reach::EachTenant);
        let (occurrence, tenant_run) = if each_tenant {
            match self.authorize_tenant_run(&metadata, execution).await {
                Ok(Ok((run, occurrence))) => (occurrence, Some(run)),
                Ok(Err(code)) => return Outcome::Cancel(code),
                Err(_) => return Outcome::Retry("schedule_state_unavailable"),
            }
        } else {
            match self.authorize_execution(&metadata, execution.job_id).await {
                Ok(Ok(id)) => (id, None),
                Ok(Err(code)) => return Outcome::Cancel(code),
                Err(_) => return Outcome::Retry("schedule_state_unavailable"),
            }
        };
        let tenant = tenant_run.map(|_| execution.tenant());
        let run = self
            .start_run(&metadata, occurrence, execution, tenant)
            .await;
        let started = Instant::now();
        let outcome = owner(business).await;
        self.finish_run(
            &metadata,
            (occurrence, tenant_run),
            run,
            &outcome,
            execution,
            started,
        )
        .await;
        outcome
    }
    /// Why the definition may not start work now, or `None`. Callers hold the
    /// per-key lock so a review or pause cannot slip in after the read.
    async fn start_refusal(
        &self,
        tx: &tokio_postgres::Transaction<'_>,
        metadata: &Metadata,
    ) -> Result<Option<&'static str>> {
        Ok(match self.definitions.get(metadata.key.as_str()) {
            None => Some("schedule_removed"),
            Some(_) if self.timing(tx, &metadata.key).await?.version != metadata.fingerprint => {
                Some("schedule_changed")
            }
            Some(_) => match self.eligibility(tx, &metadata.key).await? {
                Eligibility::Eligible => None,
                Eligibility::Unreviewed => Some("schedule_unreviewed"),
                Eligibility::Disabled => Some("schedule_disabled"),
                Eligibility::Suppressed => Some("schedule_suppressed"),
            },
        })
    }
    /// Installation runs and the each-tenant fan-out job: the operator-tenant
    /// occurrence must be unfinished and bound to this job.
    pub(crate) async fn authorize_execution(
        &self,
        metadata: &Metadata,
        job_id: i64,
    ) -> Result<std::result::Result<i64, &'static str>> {
        let mut client = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut client;
        let tx = client.transaction().await?;
        lock(&tx, &metadata.key).await?;
        let refusal = self.start_refusal(&tx, metadata).await?;
        let row = tx
            .query_opt(
                "SELECT id FROM base_schedule_occurrences WHERE key=$1 AND intended_at=$2 AND trigger=$3 AND job_id=$4 AND finished_at IS NULL FOR UPDATE",
                &[&metadata.key, &occurrence::to_pg(metadata.intended_at), &metadata.trigger.as_str(), &job_id],
            )
            .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(Err("invalid_schedule_occurrence"));
        };
        let id: i64 = row.get(0);
        if let Some(code) = refusal {
            occurrence::finish(&tx, id, "failed").await?;
            tx.commit().await?;
            return Ok(Err(code));
        }
        tx.execute(
            "UPDATE base_schedule_occurrences SET state='running',started_at=clock_timestamp() WHERE id=$1",
            &[&id],
        )
        .await?;
        tx.commit().await?;
        Ok(Ok(id))
    }
    /// One tenant's run: the run row must be unfinished and bound to exactly
    /// this job in exactly this tenant, under an occurrence of this instant.
    /// The global start check applies only until the run first starts; the
    /// durable `started_at` is the start decision, taken under the same
    /// per-key lock as every review and pause. A retry of a started run
    /// continues even if the definition was disabled since. Returns the run
    /// and its occurrence.
    async fn authorize_tenant_run(
        &self,
        metadata: &Metadata,
        execution: &Execution,
    ) -> Result<std::result::Result<(i64, i64), &'static str>> {
        let mut client = self.db.get().await?;
        let client: &mut tokio_postgres::Client = &mut client;
        let tx = client.transaction().await?;
        lock(&tx, &metadata.key).await?;
        let row = tx
            .query_opt(
                "SELECT r.id,r.occurrence_id,r.started_at IS NOT NULL FROM base_schedule_tenant_runs r \
                 JOIN base_schedule_occurrences o ON o.id=r.occurrence_id \
                 WHERE r.tenant_id=$1 AND r.job_id=$2 AND r.key=$3 AND r.finished_at IS NULL \
                   AND o.key=$3 AND o.intended_at=$4 AND o.trigger=$5 FOR UPDATE OF r",
                &[&execution.tenant(), &execution.job_id, &metadata.key, &occurrence::to_pg(metadata.intended_at), &metadata.trigger.as_str()],
            )
            .await?;
        let Some(row) = row else {
            tx.rollback().await?;
            return Ok(Err("invalid_schedule_occurrence"));
        };
        let (id, occurrence_id, started): (i64, i64, bool) = (row.get(0), row.get(1), row.get(2));
        let refusal = if started {
            None
        } else {
            self.start_refusal(&tx, metadata).await?
        };
        if let Some(code) = refusal {
            tx.execute(
                "UPDATE base_schedule_tenant_runs SET status='failed',code=$2,finished_at=clock_timestamp(),overlap_key=NULL WHERE id=$1",
                &[&id, &code],
            )
            .await?;
            tx.commit().await?;
            return Ok(Err(code));
        }
        tx.execute(
            "UPDATE base_schedule_tenant_runs SET status='running',started_at=COALESCE(started_at,clock_timestamp()) WHERE id=$1",
            &[&id],
        )
        .await?;
        tx.commit().await?;
        Ok(Ok((id, occurrence_id)))
    }
    async fn start_run(
        &self,
        metadata: &Metadata,
        occurrence: i64,
        execution: &Execution,
        tenant: Option<i64>,
    ) -> Option<i64> {
        let result = async {
            let client = self.db.get().await?;
            Ok::<_, Error>(client.query_one(
                "INSERT INTO base_schedule_runs(key,name,expression,trigger,triggered_by_user_id,triggered_by_name,status,started_at,occurrence_id,job_id,attempt,tenant_id) \
                 VALUES($1,$2,$3,$4,$5,$6,'running',clock_timestamp(),$7,$8,$9,$10) RETURNING id",
                &[&metadata.key, &metadata.name, &metadata.expression, &metadata.trigger.as_str(), &metadata.triggered_by_user_id, &metadata.triggered_by_name, &occurrence, &execution.job_id, &execution.attempt, &tenant],
            ).await?.get::<_, i64>(0))
        }
        .await;
        match result {
            Ok(id) => Some(id),
            Err(_) => {
                eprintln!("schedule recorder start_unavailable: key={}", metadata.key);
                None
            }
        }
    }
    async fn finish_run(
        &self,
        metadata: &Metadata,
        (occurrence, tenant_run): (i64, Option<i64>),
        run: Option<i64>,
        outcome: &Outcome,
        execution: &Execution,
        started: Instant,
    ) {
        let (status, terminal, excerpt) = match outcome {
            Outcome::Completed => ("succeeded", true, None),
            Outcome::Cancel(code) => ("failed", true, Some(*code)),
            Outcome::Retry(code) => (
                "failed",
                execution.attempt >= execution.max_attempts,
                Some(*code),
            ),
        };
        let runtime = i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX);
        let result = async {
            let client = self.db.get().await?;
            if let Some(run) = run {
                client.execute(
                    "UPDATE base_schedule_runs SET status=$2,finished_at=clock_timestamp(),runtime_ms=$3,output_excerpt=$4,updated_at=clock_timestamp() WHERE id=$1",
                    &[&run, &status, &runtime, &excerpt],
                ).await?;
            }
            match (tenant_run, outcome) {
                (None, _) if terminal => {
                    occurrence::finish(&**client, occurrence, status).await?;
                }
                (None, _) => {}
                (Some(id), _) => {
                    // A retry that will run again goes back to waiting, still
                    // started; a terminal outcome releases the tenant's lease.
                    let code = match outcome {
                        Outcome::Completed => None,
                        Outcome::Cancel(code) | Outcome::Retry(code) => Some(*code),
                    };
                    if terminal {
                        client.execute(
                            "UPDATE base_schedule_tenant_runs SET status=$2,code=$3,finished_at=clock_timestamp(),overlap_key=NULL WHERE id=$1 AND finished_at IS NULL",
                            &[&id, &status, &code],
                        ).await?;
                    } else {
                        client.execute(
                            "UPDATE base_schedule_tenant_runs SET status='queued',code=$2 WHERE id=$1 AND finished_at IS NULL",
                            &[&id, &code],
                        ).await?;
                    }
                }
            }
            Ok::<_, Error>(())
        }
        .await;
        if result.is_err() {
            eprintln!("schedule recorder finish_unavailable: key={}", metadata.key);
        }
    }
}
