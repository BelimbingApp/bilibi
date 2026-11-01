//! Schedule's own retention task: a
//! daily scheduled job that prunes finished occurrences and run history older
//! than `schedule.history.keep_days`. Like every contributed recurrence it is
//! unreviewed, and so runs nothing, until an operator enables it on the board.
use crate::{board, worker, Definition, Misfire, Overlap, Schedule, DEFINITIONS, EXECUTE};
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_settings::Settings;
use jiff::Timestamp;
use serde_json::{json, Value};
use std::sync::Arc;

const WORKER: &str = "base/schedule-occurrence-retention";
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/schedule",
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
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/schedule.retention", capability: EXECUTE }) }
inventory::submit! { Contribution::new(DEFINITIONS, "base/schedule.occurrence-retention", provenance!(), &Definition {
    name: "Prune schedule occurrence history", expression: "37 3 * * *", timezone: "Etc/UTC",
    task_name: "Base Schedule occurrence retention", worker: WORKER, args: || json!({}),
    overlap: Overlap::Forbid, misfire: Misfire::Coalesce, owner_route: None,
}) }

pub(crate) struct Retention {
    pub schedule: Arc<Schedule>,
    pub settings: Arc<Settings>,
    pub db: Db,
}
impl Handler for Retention {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let pruned = async {
                let client = self.db.get().await?;
                let days = board::keep_days(&self.settings, &**client).await?;
                board::prune(&**client, days, Timestamp::now()).await
            }
            .await;
            match pruned {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("schedule_store_unavailable"),
            }
        }))
    }
}
