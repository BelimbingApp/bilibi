//! The controls in front of every database-tables view: owner proof on each
//! request, the password step-up Bilibi already uses before a stored secret
//! is shown (`Authentication::confirm_password`, throttled), and one Audit
//! action per attempt and per distinct view. No audit row, no data: a view
//! whose record cannot be written is refused.
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::{Authentication, Scope};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

/// Five failed confirmations per account in a minute, as the stored-secret
/// reveal allows.
const LIMIT: usize = 5;
const WINDOW: Duration = Duration::from_secs(60);

pub(super) const VIEWED: &str = "system.database_tables.viewed";
const STEP_UP: &str = "system.database_tables.step_up";

#[derive(Default)]
struct Throttle(Mutex<HashMap<(i64, i64), Vec<Instant>>>);
impl Throttle {
    fn allowed(&self, key: (i64, i64)) -> bool {
        let mut failures = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        let now = Instant::now();
        failures.retain(|_, times| {
            times.retain(|at| now.duration_since(*at) < WINDOW);
            !times.is_empty()
        });
        failures.get(&key).map_or(0, Vec::len) < LIMIT
    }
    fn fail(&self, key: (i64, i64)) {
        let mut failures = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        failures.entry(key).or_default().push(Instant::now());
    }
    fn reset(&self, key: (i64, i64)) {
        let mut failures = self.0.lock().unwrap_or_else(|poison| poison.into_inner());
        failures.remove(&key);
    }
}

pub(super) enum StepUp {
    Confirmed,
    Rejected,
    Throttled,
}

#[derive(Clone)]
pub(super) struct Gate {
    db: Db,
    authentication: Arc<Authentication>,
    throttle: Arc<Throttle>,
}
impl Gate {
    pub fn new(db: Db, authentication: Arc<Authentication>) -> Self {
        Self {
            db,
            authentication,
            throttle: Arc::default(),
        }
    }
    pub fn db(&self) -> &Db {
        &self.db
    }

    /// The installation owner, proved afresh; anyone else is `Forbidden`.
    pub async fn owner(&self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_tenancy::prove_owner(&**client, scope).await?;
        Ok(())
    }

    /// Re-proves the owner's password once for this mounted page. Every
    /// attempt is recorded, never with the password; `subject` names the page.
    pub async fn step_up(&self, scope: &Scope, password: &str, subject: Value) -> Result<StepUp> {
        self.owner(scope).await?;
        let client = self.db.get().await?;
        let key = (scope.tenant(), scope.user());
        let outcome = if !self.throttle.allowed(key) {
            StepUp::Throttled
        } else {
            match self
                .authentication
                .confirm_password(&**client, scope, password)
                .await
            {
                Ok(true) => {
                    self.throttle.reset(key);
                    StepUp::Confirmed
                }
                Ok(false) => {
                    self.throttle.fail(key);
                    StepUp::Rejected
                }
                Err(Error::Forbidden) => StepUp::Rejected,
                Err(error) => return Err(error),
            }
        };
        let result = match outcome {
            StepUp::Confirmed => "succeeded",
            StepUp::Rejected => "refused",
            StepUp::Throttled => "throttled",
        };
        record(
            &**client,
            scope,
            STEP_UP,
            json!({"page": subject, "result": result}),
        )
        .await?;
        Ok(outcome)
    }

    /// Records one view: what was opened and how, never a row value.
    pub async fn viewed(&self, scope: &Scope, view: Value) -> Result<()> {
        let client = self.db.get().await?;
        record(&**client, scope, VIEWED, view).await
    }
}

async fn record(
    client: &(impl tokio_postgres::GenericClient + Sync),
    scope: &Scope,
    event: &str,
    payload: Value,
) -> Result<()> {
    record_action(
        client,
        &Attribution::from_scope(scope),
        None,
        event,
        payload,
        true,
    )
    .await
    .map(|_| ())
    .map_err(|_| Error::Internal)
}
