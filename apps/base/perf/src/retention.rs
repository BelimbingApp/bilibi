//! The scheduled history prune and the
//! authorization decision-log prune:
//! daily, once an operator enables each on the Schedule board. They are Perf
//! workers, not part of the recorder, so retention still runs if recording is
//! switched off. The decision-log prune lives here because Schedule depends on
//! Authz; an Authz-owned worker would be a cycle.
use crate::recorder::Recorder;
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_schedule::{worker, Definition, Misfire, Overlap, Schedule, DEFINITIONS, EXECUTE};
use bilibi_settings::Settings;
use serde_json::{json, Value};
use std::sync::Arc;

const WORKER: &str = "base/perf-retention";
const AUTHZ_WORKER: &str = "base/authz-decision-log-retention";
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/perf",
            source: file!(),
            line: line!(),
        }
    };
}

fn no_arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 5, validate: |args| worker::validate(args, no_arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/perf.retention", capability: EXECUTE }) }
inventory::submit! { Contribution::new(DEFINITIONS, "base/perf.retention", provenance!(), &Definition {
    name: "Prune performance history", expression: "17 3 * * *", timezone: "Etc/UTC",
    task_name: "Base Perf retention", worker: WORKER, args: || json!({}),
    overlap: Overlap::Forbid, misfire: Misfire::Coalesce, owner_route: None,
}) }

inventory::submit! { Contribution::new(WORKERS, AUTHZ_WORKER, provenance!(), &Worker { max_attempts: 5, validate: |args| worker::validate(args, no_arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, AUTHZ_WORKER, provenance!(), &HandlerBinding { service: "base/perf.authz_retention", capability: EXECUTE }) }
inventory::submit! { Contribution::new(DEFINITIONS, "base/authz.decision_log_retention", provenance!(), &Definition {
    name: "Prune authorization decision logs", expression: "23 3 * * *", timezone: "Etc/UTC",
    task_name: "Base Authz decision log retention", worker: AUTHZ_WORKER, args: || json!({}),
    overlap: Overlap::Forbid, misfire: Misfire::Coalesce, owner_route: None,
}) }

pub(crate) struct AuthzRetention {
    pub schedule: Arc<Schedule>,
    pub settings: Arc<Settings>,
    pub db: Db,
}
impl Handler for AuthzRetention {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let pruned = async {
                let client = self.db.get().await?;
                let days = self
                    .settings
                    .get_global(&**client, bilibi_authz::decision_logs::RETENTION_DAYS)
                    .await?
                    .as_i64()
                    .ok_or(Error::Invalid)?;
                bilibi_authz::decision_logs::prune(&**client, days).await
            }
            .await;
            match pruned {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("authz_store_unavailable"),
            }
        }))
    }
}

pub(crate) struct Retention {
    pub schedule: Arc<Schedule>,
    pub recorder: Arc<Recorder>,
}
impl Handler for Retention {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            match self.recorder.prune().await {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("perf_store_unavailable"),
            }
        }))
    }
}
