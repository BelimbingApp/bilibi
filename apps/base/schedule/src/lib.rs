//! Contributed recurrence defaults, operator clock settings, review and suppression, and
//! the durable occurrence claim that hands exactly one job per occurrence to
//! the Queue, the operator board at `/system/schedule` and history retention.
//! Work itself runs on the Queue.
mod board;
pub mod cron;
pub mod definition;
mod fan_out;
mod occurrence;
mod phrase;
pub mod recurrence;
mod retention;
pub mod scheduler;
mod tenant;
mod timing;
mod web;
pub mod worker;
pub use board::{Availability, Diagnostics, DueWork, Pruned, RunPage, RunQuery, RunSort};
pub use definition::{Definition, Misfire, Overlap, Reach, DEFINITIONS, RESERVED_ARGUMENT};
pub use occurrence::{Refusal, Run, Trigger, TriggeredBy};
pub use scheduler::Scheduler;
pub use tenant::{Audience, OwnTenant, TenantCounts, TenantFacts};

use bilibi_authz::{
    authorize_key, authorize_operator, CapabilityDefinition, RoleDefinition,
    SystemPrincipalDefinition, SystemScope, VocabularyContribution, VOCABULARY,
};
use bilibi_database::{Change, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Catalog, Contribution, Module, Provenance, RegistryError, Service};
use bilibi_queue::{JobRef, Queue};
use bilibi_tenancy::{Scope, Tenants};
use jiff::{tz::TimeZone, Timestamp};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc};
use tokio_postgres::{Client, GenericClient, Transaction};

pub const VIEW: &str = "admin.system.schedule.view";
pub const EXECUTE: &str = "admin.system.schedule.execute";
pub const MANAGE: &str = "admin.system.schedule.manage";
/// What the `scheduler` principal needs in a tenant other than the operator's to
/// run an [`Reach::EachTenant`] definition there. A tenant administrator grants
/// it (`Grants::set_system`); declaration, provisioning and the operator's own
/// `EXECUTE` grant never confer it. In the operator tenant the principal uses
/// [`EXECUTE`].
pub const TENANT_WORK: &str = "admin.schedule.tenant-work.execute";
/// A tenant administrator runs one definition now, for their own tenant only.
pub const TENANT_RUN: &str = "admin.schedule.tenant.execute";
/// A tenant administrator reads their own tenant's definitions and runs.
pub const TENANT_VIEW: &str = "admin.schedule.tenant.view";
/// Every worker of an [`Reach::EachTenant`] definition contributes this to
/// `bilibi_queue::CAPABILITY_POLICIES` under its worker id, so Queue decides
/// the tenant attempt on [`TENANT_WORK`] for the principal and [`TENANT_RUN`] for
/// a person, and on its binding's [`EXECUTE`] in the operator tenant. Boot
/// refuses an each-tenant definition whose worker has any other policy.
pub const TENANT_POLICY: bilibi_queue::CapabilityPolicy = bilibi_queue::CapabilityPolicy {
    tenant_system: TENANT_WORK,
    tenant_user: TENANT_RUN,
};
/// The system principal recurrences are enqueued and executed as. It must be
/// granted [`EXECUTE`] in the operator tenant (`Grants::set_system`) before
/// any claimed job can run; declaration alone grants nothing.
pub const PRINCIPAL: &str = "scheduler";

/// A validated definition with its owner, parsed recurrence and fingerprint.
pub struct Registered {
    key: &'static str,
    owner: &'static str,
    definition: &'static Definition,
    cron: cron::Cron,
    zone: TimeZone,
    args: Value,
    fingerprint: String,
}
impl Registered {
    pub fn key(&self) -> &'static str {
        self.key
    }
    /// The contributing module.
    pub fn owner(&self) -> &'static str {
        self.owner
    }
    pub fn definition(&self) -> &'static Definition {
        self.definition
    }
    pub fn zone(&self) -> &TimeZone {
        &self.zone
    }
    /// Worker-validated arguments exactly as Q4 will enqueue them.
    pub fn args(&self) -> &Value {
        &self.args
    }
    /// Hex SHA-256 of every material field; review binds to this value.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }
    /// Earliest occurrence at or after `now` (`now` is the caller's clock).
    pub fn next(&self, now: Timestamp) -> std::result::Result<Timestamp, recurrence::Error> {
        recurrence::next(&self.cron, &self.zone, now)
    }
    /// Latest occurrence at or before `now`.
    pub fn previous(&self, now: Timestamp) -> std::result::Result<Timestamp, recurrence::Error> {
        recurrence::previous(&self.cron, &self.zone, now)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReviewState {
    /// Never reviewed, or reviewed at a fingerprint the definition no longer has.
    Unreviewed,
    Enabled,
    Disabled,
}
/// Whether work may be claimed now. Anything but `Eligible` must not run.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Eligibility {
    Eligible,
    Unreviewed,
    Disabled,
    Suppressed,
}

/// Operator-facing facts; no worker arguments.
#[derive(Clone, Debug)]
pub struct Task {
    pub key: &'static str,
    pub name: &'static str,
    pub owner: &'static str,
    pub owner_route: Option<&'static str>,
    pub task_name: &'static str,
    pub expression: &'static str,
    pub timezone: &'static str,
    pub overlap: Overlap,
    pub misfire: Misfire,
    pub reach: Reach,
    pub fingerprint: String,
    pub review: ReviewState,
    pub suppressed: bool,
    pub next_due_at: Option<Timestamp>,
    /// The operator setting, or the contribution defaults when none applies.
    pub clock_expression: String,
    pub clock_timezone: String,
    pub timing_version: String,
    pub time_overridden: bool,
    /// Per-tenant outcomes (operator) or this tenant's standing (tenant
    /// administrator); `None` for an installation definition.
    pub tenants: Option<TenantFacts>,
    /// The latest recorded run's status; `None` when it never ran.
    pub last_status: Option<String>,
    pub last_started_at: Option<Timestamp>,
    pub last_runtime_ms: Option<i32>,
}

/// A committed claim: the occurrence row and its provisional-until-commit job.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Claimed {
    pub occurrence_id: i64,
    pub job: JobRef,
    pub intended_at: Timestamp,
    pub trigger: Trigger,
}
enum Enqueuer<'a> {
    Scheduler(&'a SystemScope),
    Operator(&'a bilibi_tenancy::Scope),
}

pub struct Schedule {
    definitions: BTreeMap<&'static str, Registered>,
    boundary: Arc<MutationBoundary>,
    queue: Arc<Queue>,
    tenants: Arc<Tenants>,
    db: Db,
    heartbeat: board::Heartbeat,
}
impl Schedule {
    /// Validates every contribution against the Queue's worker catalog: the
    /// worker must exist, belong to the contributing module, accept the args,
    /// and keep the schedule metadata the claim attaches (`worker::validate`).
    pub fn from_catalog(
        catalog: &Catalog,
        queue: Arc<Queue>,
        tenants: Arc<Tenants>,
        boundary: Arc<MutationBoundary>,
        db: Db,
    ) -> bilibi_module_registry::Result<Self> {
        let mut definitions = BTreeMap::new();
        for entry in catalog.entries::<Definition>(DEFINITIONS)? {
            let fail = |reason: &str| {
                RegistryError::Contribution(format!(
                    "schedule definition {} from {} at {}:{}: {reason}",
                    entry.id,
                    entry.provenance.module,
                    entry.provenance.source,
                    entry.provenance.line
                ))
            };
            let definition = entry.get::<Definition>()?;
            let cron = definition::check(entry.id, definition).map_err(|reason| fail(&reason))?;
            let zone = TimeZone::get(definition.timezone).map_err(|_| fail("unknown timezone"))?;
            let worker = queue
                .workers()
                .get(definition.worker)
                .map_err(|_| fail("worker is not a registered Queue worker"))?;
            if worker.module != entry.provenance.module {
                return Err(fail("worker belongs to another module"));
            }
            let args = (definition.args)();
            let normalized = (worker.definition.validate)(args.clone())
                .map_err(|_| fail("worker refuses the arguments"))?;
            bilibi_queue::validate_arguments(&normalized)
                .map_err(|_| fail("normalized arguments exceed the transport bounds"))?;
            if definition.reach == Reach::EachTenant {
                let policy = catalog
                    .entries::<bilibi_queue::CapabilityPolicy>(bilibi_queue::CAPABILITY_POLICIES)?
                    .iter()
                    .find(|policy| policy.id == definition.worker)
                    .map(|policy| policy.get::<bilibi_queue::CapabilityPolicy>().copied())
                    .transpose()?;
                if policy != Some(TENANT_POLICY) {
                    return Err(fail(
                        "an each-tenant worker must contribute bilibi_schedule::TENANT_POLICY to bilibi_queue::CAPABILITY_POLICIES under its worker id",
                    ));
                }
            }
            let fingerprint =
                definition::fingerprint(entry.id, entry.provenance.module, definition, &args);
            // The claim enqueues these arguments plus the reserved metadata; a
            // validator that strips or rejects it would refuse every claim.
            let mut probe = normalized.clone();
            probe[RESERVED_ARGUMENT] = worker::probe(entry.id, definition.task_name, &fingerprint);
            let expected = probe[RESERVED_ARGUMENT].clone();
            match (worker.definition.validate)(probe) {
                Ok(kept) if kept.get(RESERVED_ARGUMENT) == Some(&expected) => {}
                _ => {
                    return Err(fail(
                        "worker does not keep schedule metadata; validate through bilibi_schedule::worker::validate",
                    ))
                }
            }
            definitions.insert(
                entry.id,
                Registered {
                    key: entry.id,
                    owner: entry.provenance.module,
                    definition,
                    cron,
                    zone,
                    fingerprint,
                    args: normalized,
                },
            );
        }
        Ok(Self {
            definitions,
            boundary,
            queue,
            tenants,
            db,
            heartbeat: board::Heartbeat::default(),
        })
    }
    pub fn definitions(&self) -> impl Iterator<Item = &Registered> {
        self.definitions.values()
    }
    pub fn definition(&self, key: &str) -> Option<&Registered> {
        self.definitions.get(key)
    }

    /// Operator view. `now` is the caller's clock, so a replayed instant yields
    /// the same `next_due_at` it would have produced then.
    pub async fn tasks(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        now: Timestamp,
    ) -> Result<Vec<Task>> {
        let audience = self.audience(client, scope).await?;
        // A tenant administrator reads only each-tenant definitions.
        let keys: Vec<&str> = self
            .definitions
            .values()
            .filter(|item| {
                audience == Audience::Operator || item.definition.reach == Reach::EachTenant
            })
            .map(|item| item.key)
            .collect();
        let tenant_filter = (audience == Audience::Tenant).then(|| scope.tenant());
        let facts = self.tenant_facts(client, scope, audience, &keys).await?;
        let reviews: BTreeMap<String, (String, bool)> = client
            .query(
                "SELECT key,fingerprint,enabled FROM base_schedule_definition_reviews WHERE key=ANY($1)",
                &[&keys],
            )
            .await?
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    (row.get::<_, String>(1), row.get::<_, bool>(2)),
                )
            })
            .collect();
        let suppressed: Vec<String> = client
            .query(
                "SELECT key FROM base_schedule_suppressions WHERE key=ANY($1)",
                &[&keys],
            )
            .await?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        let latest: BTreeMap<String, (String, Timestamp, Option<i32>)> = client
            .query(
                "SELECT DISTINCT ON (key) key,status,started_at,runtime_ms FROM base_schedule_runs WHERE key=ANY($1) AND ($2::bigint IS NULL OR tenant_id=$2) ORDER BY key,started_at DESC,id DESC",
                &[&keys, &tenant_filter],
            )
            .await?
            .into_iter()
            .map(|row| {
                (
                    row.get(0),
                    (row.get(1), occurrence::from_pg(row.get(2)), row.get(3)),
                )
            })
            .collect();
        let timings: BTreeMap<String, tokio_postgres::Row> = client
            .query(
                "SELECT * FROM base_schedule_timings WHERE key=ANY($1)",
                &[&keys],
            )
            .await?
            .into_iter()
            .map(|row| (row.get("key"), row))
            .collect();
        self.definitions
            .values()
            .filter(|item| keys.contains(&item.key))
            .map(|item| {
                let timing = timing::Timing::from_row(item, timings.get(item.key))?;
                Ok(Task {
                    key: item.key,
                    name: item.definition.name,
                    owner: item.owner,
                    owner_route: item.definition.owner_route,
                    task_name: item.definition.task_name,
                    expression: item.definition.expression,
                    timezone: item.definition.timezone,
                    overlap: item.definition.overlap,
                    misfire: item.definition.misfire,
                    reach: item.definition.reach,
                    tenants: facts.get(item.key).cloned(),
                    fingerprint: item.fingerprint.clone(),
                    review: review_state(item, reviews.get(item.key)),
                    suppressed: suppressed.iter().any(|key| key == item.key),
                    next_due_at: timing.next(now),
                    clock_expression: timing.expression,
                    clock_timezone: timing.timezone,
                    timing_version: timing.version,
                    time_overridden: timing.overridden,
                    last_status: latest.get(item.key).map(|run| run.0.clone()),
                    last_started_at: latest.get(item.key).map(|run| run.1),
                    last_runtime_ms: latest.get(item.key).and_then(|run| run.2),
                })
            })
            .collect()
    }

    /// Trusted claim-time check for Q4 and queued-job start. Call it inside the
    /// transaction that holds [`lock`] so a concurrent review or pause cannot
    /// slip between the read and the claim. A database error is an `Err`, never
    /// `Eligible`: uncertainty fails closed.
    pub async fn eligibility(&self, client: &impl GenericClient, key: &str) -> Result<Eligibility> {
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        let review = client
            .query_opt(
                "SELECT fingerprint,enabled FROM base_schedule_definition_reviews WHERE key=$1",
                &[&key],
            )
            .await?
            .map(|row| (row.get::<_, String>(0), row.get::<_, bool>(1)));
        let paused = client
            .query_opt(
                "SELECT 1 FROM base_schedule_suppressions WHERE key=$1",
                &[&key],
            )
            .await?
            .is_some();
        Ok(match review_state(item, review.as_ref()) {
            ReviewState::Unreviewed => Eligibility::Unreviewed,
            ReviewState::Disabled => Eligibility::Disabled,
            ReviewState::Enabled if paused => Eligibility::Suppressed,
            ReviewState::Enabled => Eligibility::Eligible,
        })
    }

    /// Records the decision for the definition's *current* fingerprint. A later
    /// change to any material field makes it unreviewed again.
    pub async fn review(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        enabled: bool,
    ) -> Result<()> {
        authorize_operator(&*client, scope, MANAGE).await?;
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        let fingerprint = item.fingerprint.clone();
        let action = if enabled {
            "schedule.task.enabled"
        } else {
            "schedule.task.disabled"
        };
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context(action))
            .await?;
        lock(tx.sql(), key).await?;
        let key = key.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let old = sql
                    .query_opt(
                        "SELECT fingerprint,enabled FROM base_schedule_definition_reviews WHERE key=$1",
                        &[&key],
                    )
                    .await?
                    .map(|row| json!({"fingerprint": row.get::<_, String>(0), "enabled": row.get::<_, bool>(1)}))
                    .unwrap_or(Value::Null);
                let id: i64 = sql
                    .query_one(
                        "INSERT INTO base_schedule_definition_reviews(key,fingerprint,enabled) VALUES($1,$2,$3) \
                         ON CONFLICT (key) DO UPDATE SET fingerprint=EXCLUDED.fingerprint,enabled=EXCLUDED.enabled,reviewed_at=now() RETURNING id",
                        &[&key, &fingerprint, &enabled],
                    )
                    .await?
                    .get(0);
                Ok((
                    (),
                    vec![Change {
                        kind: "base/schedule.review".into(),
                        id,
                        old,
                        new: json!({"key": key, "fingerprint": fingerprint, "enabled": enabled}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }

    /// Pauses a definition (a row means paused). Returns whether anything changed.
    pub async fn suppress(&self, client: &mut Client, scope: &Scope, key: &str) -> Result<bool> {
        self.set_suppressed(client, scope, key, true).await
    }
    /// Resumes a definition (deleting the row means resumed).
    pub async fn resume(&self, client: &mut Client, scope: &Scope, key: &str) -> Result<bool> {
        self.set_suppressed(client, scope, key, false).await
    }
    async fn set_suppressed(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        suppressed: bool,
    ) -> Result<bool> {
        authorize_operator(&*client, scope, MANAGE).await?;
        self.definitions.get(key).ok_or(Error::NotFound)?;
        let action = if suppressed {
            "schedule.task.paused"
        } else {
            "schedule.task.resumed"
        };
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context(action))
            .await?;
        lock(tx.sql(), key).await?;
        let present = tx
            .sql()
            .query_opt(
                "SELECT 1 FROM base_schedule_suppressions WHERE key=$1",
                &[&key],
            )
            .await?
            .is_some();
        // A repeat command changes nothing, so it records nothing.
        if present == suppressed {
            tx.rollback().await?;
            return Ok(false);
        }
        let key = key.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = if suppressed {
                    sql.query_one(
                        "INSERT INTO base_schedule_suppressions(key) VALUES($1) RETURNING id",
                        &[&key],
                    )
                    .await?
                    .get(0)
                } else {
                    sql.query_one(
                        "DELETE FROM base_schedule_suppressions WHERE key=$1 RETURNING id",
                        &[&key],
                    )
                    .await?
                    .get(0)
                };
                Ok((
                    (),
                    vec![Change {
                        kind: "base/schedule.suppression".into(),
                        id,
                        old: json!({"key": key, "suppressed": !suppressed}),
                        new: json!({"key": key, "suppressed": suppressed}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(true)
    }
}

impl Schedule {
    /// The scheduler's claim for the occurrence at `intended_at`. See [`claim`].
    pub async fn claim_scheduled(
        &self,
        client: &mut Client,
        scheduler: &SystemScope,
        key: &str,
        intended_at: Timestamp,
    ) -> Result<std::result::Result<Claimed, Refusal>> {
        self.claim(
            client,
            Enqueuer::Scheduler(scheduler),
            key,
            intended_at,
            Trigger::Scheduled,
            None,
        )
        .await
    }
    /// Operator run-now: one manual occurrence at `now`, queued as the operator
    /// (never executed inline) and audited as `schedule.run.queued`. It goes
    /// through the same review, suppression, overlap and claim path as the
    /// scheduler; an overlap refusal is recorded as a skipped run naming who asked.
    pub async fn run_now(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        now: Timestamp,
    ) -> Result<std::result::Result<Claimed, Refusal>> {
        authorize_operator(&*client, scope, EXECUTE).await?;
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        let name = scope.user_name.trim();
        let triggered_by = TriggeredBy {
            user_id: scope.user(),
            name: (!name.is_empty()).then(|| name.chars().take(255).collect()),
        };
        let claim = self
            .claim(
                client,
                Enqueuer::Operator(scope),
                key,
                now,
                Trigger::Manual,
                Some(&triggered_by),
            )
            .await?;
        if claim == Err(Refusal::Overlap) {
            if let Err(error) = occurrence::record_skipped(
                &*client,
                item,
                occurrence::micros(now),
                Trigger::Manual,
                Some(&triggered_by),
            )
            .await
            {
                eprintln!("schedule overlap history recording unavailable: key={key} {error}");
            }
        }
        Ok(claim)
    }
    /// Occurrence row, job and audit evidence commit or roll back together
    /// under the per-key lock, after reconciling this key's unfinished
    /// occurrences from Queue's terminal state. The same intended instant is
    /// claimed at most once ever, by whichever process gets there first; a
    /// forbid-overlap definition is refused while an earlier occurrence is
    /// unfinished. Refusals write nothing.
    async fn claim(
        &self,
        client: &mut Client,
        by: Enqueuer<'_>,
        key: &str,
        intended_at: Timestamp,
        trigger: Trigger,
        triggered_by: Option<&TriggeredBy>,
    ) -> Result<std::result::Result<Claimed, Refusal>> {
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        let intended_at = occurrence::micros(intended_at);
        let (context, tenant) = match by {
            Enqueuer::Scheduler(scope) => (
                scope.mutation_context("schedule.occurrence.claimed"),
                scope.tenant(),
            ),
            Enqueuer::Operator(scope) => (
                scope.mutation_context("schedule.run.queued"),
                scope.tenant(),
            ),
        };
        let fan_out_scheduler = match by {
            Enqueuer::Operator(scope) if item.definition.reach == Reach::EachTenant => {
                Some(SystemScope::rehydrate(&*client, scope.tenant(), Some(PRINCIPAL)).await?)
            }
            _ => None,
        };
        let mut tx = self.boundary.begin(client, context).await?;
        lock(tx.sql(), key).await?;
        let refusal = match self.eligibility(tx.sql(), key).await? {
            Eligibility::Eligible => None,
            Eligibility::Unreviewed => Some(Refusal::Unreviewed),
            Eligibility::Disabled => Some(Refusal::Disabled),
            Eligibility::Suppressed => Some(Refusal::Suppressed),
        };
        if let Some(refusal) = refusal {
            tx.rollback().await?;
            return Ok(Err(refusal));
        }
        let timing = self.timing(tx.sql(), key).await?;
        if trigger == Trigger::Scheduled
            && timing.previous(intended_at).map_err(|_| Error::Invalid)? != Some(intended_at)
        {
            tx.rollback().await?;
            return Ok(Err(Refusal::Unreviewed));
        }
        occurrence::reconcile(tx.sql(), Some(key)).await?;
        let at = occurrence::to_pg(intended_at);
        if tx
            .sql()
            .query_opt(
                "SELECT 1 FROM base_schedule_occurrences WHERE key=$1 AND intended_at=$2 AND trigger=$3",
                &[&key, &at, &trigger.as_str()],
            )
            .await?
            .is_some()
        {
            tx.rollback().await?;
            return Ok(Err(Refusal::AlreadyClaimed));
        }
        // Each-tenant overlap is per tenant (`base_schedule_tenant_runs`); the
        // occurrence itself is just the fan-out and never holds a lease.
        let each_tenant = item.definition.reach == Reach::EachTenant;
        let overlap_key = (item.definition.overlap == Overlap::Forbid && !each_tenant)
            .then(|| format!("scheduler:{key}"));
        if let Some(overlap_key) = &overlap_key {
            if tx
                .sql()
                .query_opt(
                    "SELECT 1 FROM base_schedule_occurrences WHERE overlap_key=$1 AND finished_at IS NULL",
                    &[overlap_key],
                )
                .await?
                .is_some()
            {
                tx.rollback().await?;
                return Ok(Err(Refusal::Overlap));
            }
        }
        let metadata = worker::Metadata {
            key: key.into(),
            name: item.definition.task_name.into(),
            expression: (trigger == Trigger::Scheduled).then(|| timing.expression.clone()),
            fingerprint: timing.version.clone(),
            intended_at,
            trigger,
            triggered_by_user_id: triggered_by.map(|t| t.user_id),
            triggered_by_name: triggered_by.and_then(|t| t.name.clone()),
        };
        let mut args = if each_tenant {
            json!({})
        } else {
            item.args.clone()
        };
        args[RESERVED_ARGUMENT] = metadata.to_json();
        let job = match (by, fan_out_scheduler) {
            (Enqueuer::Scheduler(scope), _) if each_tenant => {
                self.queue
                    .enqueue_system(&mut tx, scope, fan_out::WORKER, args)
                    .await?
            }
            // The operator asked, but every tenant's work is the scheduler's:
            // the fan-out runs as the principal each tenant has consented to.
            (Enqueuer::Operator(_), Some(scheduler)) => {
                self.queue
                    .enqueue_system_within(&mut tx, &scheduler, fan_out::WORKER, args)
                    .await?
            }
            (Enqueuer::Scheduler(scope), _) => {
                self.queue
                    .enqueue_system(&mut tx, scope, item.definition.worker, args)
                    .await?
            }
            (Enqueuer::Operator(scope), None) => {
                self.queue
                    .enqueue_for(&mut tx, scope, item.definition.worker, args)
                    .await?
            }
        };
        let key = key.to_owned();
        let occurrence_id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO base_schedule_occurrences(key,intended_at,trigger,overlap_key,state,tenant_id,job_id) VALUES($1,$2,$3,$4,'queued',$5,$6) RETURNING id",
                            &[&key, &at, &trigger.as_str(), &overlap_key, &tenant, &job.id()],
                        )
                        .await?
                        .get(0);
                    Ok((
                        id,
                        vec![Change {
                            kind: "base/schedule.occurrence".into(),
                            id,
                            old: Value::Null,
                            new: json!({"key": key, "intended_at": intended_at.to_string(), "trigger": trigger.as_str(), "job_id": job.id(), "overlap_key": overlap_key}),
                        }],
                    ))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(Ok(Claimed {
            occurrence_id,
            job,
            intended_at,
            trigger,
        }))
    }
    /// Redacted run history, newest first, bounded to 500 rows. Needs `VIEW`.
    pub async fn runs(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        key: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Run>> {
        authorize_key(client, scope, VIEW).await?;
        if !(1..=500).contains(&limit) {
            return Err(Error::Invalid);
        }
        occurrence::runs(client, key, limit).await
    }
}

/// Serialises review, suppression and claims for one key until the
/// surrounding transaction ends.
pub async fn lock(tx: &Transaction<'_>, key: &str) -> Result<()> {
    tx.execute(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        &[&format!("scheduler:{key}")],
    )
    .await?;
    Ok(())
}

fn review_state(item: &Registered, review: Option<&(String, bool)>) -> ReviewState {
    match review {
        Some((fingerprint, enabled)) if *fingerprint == item.fingerprint => {
            if *enabled {
                ReviewState::Enabled
            } else {
                ReviewState::Disabled
            }
        }
        _ => ReviewState::Unreviewed,
    }
}

inventory::submit! { Contribution::new(VOCABULARY, "base/schedule", Provenance { module: "base/schedule", source: file!(), line: line!() }, &VocabularyContribution {
    domains: &[],
    verbs: &["execute"],
    // Installation-global: the persistent operator identity is required too.
    capabilities: &[CapabilityDefinition::platform(VIEW), CapabilityDefinition::platform(EXECUTE), CapabilityDefinition::platform(MANAGE), CapabilityDefinition::tenant(TENANT_WORK), CapabilityDefinition::tenant(TENANT_RUN), CapabilityDefinition::tenant(TENANT_VIEW)],
    roles: &[RoleDefinition { code: "system_viewer", name: "System Viewer", description: "Contributed system role; reconciled explicitly", grant_all: false, capabilities: &[VIEW] }],
    system_principals: &[SystemPrincipalDefinition { name: PRINCIPAL, capabilities: &[EXECUTE, TENANT_WORK, bilibi_tenancy::LIST] }],
}) }
inventory::submit! {
    Module { id: "base/schedule", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/audit", "base/tenancy", "base/authz", "base/queue", "base/settings", "base/locale", "base/datetime", "base/ui", "base/menu"],
        migrations: &[("base-schedule-001", include_str!("../migrations/001-schedule.sql")), ("base-schedule-002", include_str!("../migrations/002-schedule-occurrences.sql")), ("base-schedule-003-schedule-timing", include_str!("../migrations/003-schedule-timing.sql")), ("base-schedule-004-tenant-runs", include_str!("../migrations/004-tenant-runs.sql"))],
        services: &[Service { id: "base/schedule.store", factory: |services| {
            let catalog = Catalog::discover(&bilibi_module_registry::discover()?)?;
            let queue = services.get::<Queue>("base/queue.store")?;
            let boundary = services.get::<MutationBoundary>("base/database.mutations")?;
            let tenants = services.get::<Tenants>("base/tenancy.tenants")?;
            let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
            Ok(Arc::new(Schedule::from_catalog(&catalog, queue, tenants, boundary, db)?))
        } }, Service { id: "base/schedule.fan-out", factory: |services| {
            Ok(Arc::new(bilibi_queue::HandlerService(Arc::new(fan_out::FanOut {
                schedule: services.get("base/schedule.store")?,
            }))))
        } }, Service { id: "base/schedule.retention", factory: |services| {
            Ok(Arc::new(bilibi_queue::HandlerService(Arc::new(retention::Retention {
                schedule: services.get("base/schedule.store")?,
                settings: services.get("base/settings.store")?,
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            }))))
        } }, Service { id: "base/schedule.pages", factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages::new(
                services.get::<Db>("base/database.pool")?.as_ref().clone(),
                services.get("base/schedule.store")?,
                services.get("base/settings.store")?,
                services.get("base/datetime.display")?,
                services.get("base/locale.locales")?,
                services.get("base/tenancy.tenants")?,
            )))))
        } }],
    }
}
inventory::submit! { Contribution::new(bilibi_settings::DEFINITIONS, board::KEEP_DAYS,
Provenance { module: "base/schedule", source: file!(), line: line!() },
&bilibi_settings::Setting::Definition(bilibi_settings::Definition {
    value_type: bilibi_settings::ValueType::Integer, scopes: &[bilibi_settings::ScopeKind::Global],
    default: || json!(board::KEEP_DAYS_DEFAULT), nullable: false, encrypted: false,
    minimum: Some(board::KEEP_DAYS_MIN as f64), maximum: Some(board::KEEP_DAYS_MAX as f64), validator: None,
    editor: Some(bilibi_settings::Editor { group: "Schedule", label: "Schedule history retention",
        help: "Days to keep finished schedule runs; 0 keeps them all.", capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(bilibi_module_registry::contributions::ROUTES, "schedule.board",
Provenance { module: "base/schedule", source: file!(), line: line!() },
&bilibi_module_registry::contributions::Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant, method: bilibi_module_registry::contributions::Method::Get, path: "/system/schedule",
    service: "base/schedule.pages", operation: "board", access: bilibi_module_registry::contributions::Access::AnyOf(&[VIEW, TENANT_VIEW]), framed: true }) }
bilibi_menu::contribute!(
    "base/schedule",
    bilibi_menu::MenuItem::page("admin.system.schedule", "Schedule", "/system/schedule")
        .under("admin.system")
        .icon("clipboard-document-list")
        .requires(bilibi_menu::Requirement::AnyOf(&[VIEW, TENANT_VIEW]))
        .order(15)
);

// ADR 01 gap, listed so it stays visible: these pages still write through
// the facade rather than an operation contract (`base/agent_api` page).
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_module_registry::contributions::PAGE_WRITES, "base/schedule.pages", bilibi_module_registry::Provenance { module: "base/schedule", source: file!(), line: line!() }, &bilibi_module_registry::contributions::PageWrites {
    service: "base/schedule.pages",
    contracts: &[],
    pending: bilibi_module_registry::contributions::Pending::Every { reason: "The schedule board writes through the Schedule facade; no contract yet." },
}) }
