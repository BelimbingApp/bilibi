//! Daily payload retention. Like every contributed
//! recurrence it runs nothing until an operator reviews and enables it on the
//! Schedule board.
use crate::contributions::{RETENTION_DAYS, RETENTION_MAX};
use crate::Integration;
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_schedule::{worker, Definition, Misfire, Overlap, Schedule, DEFINITIONS, EXECUTE};
use bilibi_settings::Settings;
use serde_json::{json, Value};
use std::{sync::Arc, time::SystemTime};

const WORKER: &str = "base/integration-payload-retention";
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/integration",
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
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/integration.retention", capability: EXECUTE }) }
inventory::submit! { Contribution::new(DEFINITIONS, "base/integration-payload-retention", provenance!(), &Definition {
    name: "Prune API exchange payloads", expression: "17 3 * * *", timezone: "Etc/UTC",
    task_name: "Base Integration payload retention", worker: WORKER, args: || json!({}),
    overlap: Overlap::Forbid, misfire: Misfire::Coalesce, owner_route: None,
}) }

pub(crate) struct Retention {
    pub integration: Arc<Integration>,
    pub schedule: Arc<Schedule>,
    pub settings: Arc<Settings>,
    pub db: Db,
}
impl Handler for Retention {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let pruned = async {
                let mut client = self.db.get().await?;
                let days = self
                    .settings
                    .get_global(&**client, RETENTION_DAYS)
                    .await?
                    .as_i64()
                    .filter(|days| (1..=RETENTION_MAX).contains(days))
                    .ok_or(Error::Invalid)?;
                self.integration
                    .prune_payloads(&mut client, days, SystemTime::now())
                    .await
            }
            .await;
            match pruned {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("integration_store_unavailable"),
            }
        }))
    }
}
