//! The scheduled sweep of expired sessions (Bilimbi `Session.ExpiryWorker`):
//! every five minutes, once an operator enables it on the Schedule board. The
//! cutoff is the live `session.lifetime_minutes` setting, read by the sweep.
use crate::Sessions;
use bilibi_database::{Db, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_schedule::{worker, Definition, Misfire, Overlap, Schedule, DEFINITIONS, EXECUTE};
use serde_json::{json, Value};
use std::sync::Arc;

const WORKER: &str = "base/session-expiry";
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/session",
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
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/session.expiry", capability: EXECUTE }) }
inventory::submit! { Contribution::new(DEFINITIONS, "base/session-expiry", provenance!(), &Definition {
    name: "Prune expired sessions", expression: "*/5 * * * *", timezone: "Etc/UTC",
    task_name: "Base Session expiry", worker: WORKER, args: || json!({}),
    overlap: Overlap::Forbid, misfire: Misfire::Coalesce, owner_route: None,
}) }

pub(crate) struct Expiry {
    pub schedule: Arc<Schedule>,
    pub sessions: Arc<Sessions>,
    pub db: Db,
}
impl Handler for Expiry {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let pruned = async {
                let client = self.db.get().await?;
                self.sessions.prune_expired(&**client).await
            }
            .await;
            match pruned {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("session_store_unavailable"),
            }
        }))
    }
}
