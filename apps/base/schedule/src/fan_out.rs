//! Per-tenant fan-out (ADR 17). An `EachTenant` occurrence is claimed once, in
//! the operator tenant, like any other; its Queue job is this Platform worker
//! (`base/schedule-fan-out`), which walks the tenant directory and queues the
//! owner's worker once in every tenant that has consented, as that tenant's
//! `scheduler` principal. The owner's worker never enumerates tenants.
//!
//! Each tenant gets exactly one `base_schedule_tenant_runs` row per occurrence
//! (unique on `(occurrence, tenant)`): a queued job written in the same audited
//! transaction as that job, or the reason none was queued. A retried or resumed
//! fan-out therefore converges on one job per tenant, and the cohort is fixed
//! by the tenant-id high-water mark the first attempt stored.
use crate::{
    definition::{Overlap, Reach},
    worker::{self, Metadata},
    Registered, Schedule, EXECUTE, PRINCIPAL, RESERVED_ARGUMENT, TENANT_WORK,
};
use bilibi_authz::{decide_system, SystemScope};
use bilibi_database::{Change, Error, Result};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    Execution, Handler, HandlerBinding, HandlerFuture, Outcome, Worker, HANDLERS, WORKERS,
};
use bilibi_tenancy::{TenantIdentity, PRINCIPAL_PAGE_MAX};
use serde_json::{json, Value};
use std::sync::Arc;

pub(crate) const WORKER: &str = "base/schedule-fan-out";
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
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 10, validate: |args| worker::validate(args, no_arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: "base/schedule.fan-out", capability: EXECUTE }) }

pub(crate) struct FanOut {
    pub schedule: Arc<Schedule>,
}
impl Handler for FanOut {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.fan_out(execution, args))
    }
}

/// Why a tenant got no job.
#[derive(Clone, Copy)]
enum Skipped {
    Suspended,
    NotGranted,
}

impl Schedule {
    /// The whole fan-out for one claimed occurrence. Every step is idempotent,
    /// so a retry (or a second attempt after a crash) only adds what is missing.
    pub(crate) async fn fan_out(&self, execution: &Execution, args: Value) -> Outcome {
        let metadata = match worker::split(args) {
            Ok((_, Some(metadata))) => metadata,
            Ok((_, None)) => return Outcome::Cancel("missing_schedule_metadata"),
            Err(_) => return Outcome::Cancel("invalid_schedule_metadata"),
        };
        let occurrence = match self.authorize_execution(&metadata, execution.job_id).await {
            Ok(Ok(id)) => id,
            Ok(Err(code)) => return Outcome::Cancel(code),
            Err(_) => return Outcome::Retry("schedule_state_unavailable"),
        };
        let Some(item) = self.definitions.get(metadata.key.as_str()) else {
            return Outcome::Cancel("schedule_removed");
        };
        if item.definition.reach != Reach::EachTenant {
            return Outcome::Cancel("invalid_schedule_occurrence");
        }
        match self
            .fan_out_cohort(execution, item, &metadata, occurrence)
            .await
        {
            Ok(()) => Outcome::Completed,
            Err(Error::Unauthorized | Error::Forbidden) => Outcome::Cancel("actor_refused"),
            Err(_) => Outcome::Retry("fan_out_unavailable"),
        }
    }

    async fn fan_out_cohort(
        &self,
        execution: &Execution,
        item: &Registered,
        metadata: &Metadata,
        occurrence: i64,
    ) -> Result<()> {
        let scope = execution.scope().ok_or(Error::Forbidden)?;
        let mut through: Option<i64> = self
            .db
            .get()
            .await?
            .query_one(
                "SELECT fan_out_through FROM base_schedule_occurrences WHERE id=$1",
                &[&occurrence],
            )
            .await?
            .get(0);
        let mut after = 0_i64;
        loop {
            let page = self
                .tenants
                .principal_page(scope, after, through, PRINCIPAL_PAGE_MAX)
                .await?;
            if through.is_none() {
                // The first attempt fixes the cohort; a concurrent or later
                // attempt reads the stored mark, so late tenants never join.
                let stored: i64 = self
                    .db
                    .get()
                    .await?
                    .query_one(
                        "UPDATE base_schedule_occurrences SET fan_out_through=COALESCE(fan_out_through,$2) WHERE id=$1 RETURNING fan_out_through",
                        &[&occurrence, &page.through_id],
                    )
                    .await?
                    .get(0);
                through = Some(stored);
                if stored != page.through_id {
                    continue;
                }
            }
            for target in &page.tenants {
                self.fan_out_tenant(execution, item, metadata, occurrence, target)
                    .await?;
            }
            match page.tenants.last() {
                Some(last) if page.tenants.len() as i64 == PRINCIPAL_PAGE_MAX => after = last.id,
                _ => return Ok(()),
            }
        }
    }

    async fn fan_out_tenant(
        &self,
        execution: &Execution,
        item: &Registered,
        metadata: &Metadata,
        occurrence: i64,
        target: &TenantIdentity,
    ) -> Result<()> {
        if self.tenant_run_exists(occurrence, target.id).await? {
            return Ok(());
        }
        if target.status != "active" {
            return self
                .record_skipped_tenant(
                    execution,
                    item,
                    metadata,
                    occurrence,
                    target.id,
                    Skipped::Suspended,
                )
                .await;
        }
        // The live tenant, declared principal and durable grant are decided on
        // this tenant's own scope; Authz logs it and a deny beats any allow.
        let scope = {
            let client = self.db.get().await?;
            match SystemScope::rehydrate(&**client, target.id, Some(PRINCIPAL)).await {
                Ok(scope) => scope,
                // Suspended or deleted since the page was read.
                Err(Error::Unauthorized) => {
                    return self
                        .record_skipped_tenant(
                            execution,
                            item,
                            metadata,
                            occurrence,
                            target.id,
                            Skipped::Suspended,
                        )
                        .await
                }
                Err(error) => return Err(error),
            }
        };
        let key = if target.is_platform_operator {
            EXECUTE
        } else {
            TENANT_WORK
        };
        let allowed = {
            let client = self.db.get().await?;
            decide_system(&**client, &scope, key).await?.allowed
        };
        if !allowed {
            return self
                .record_skipped_tenant(
                    execution,
                    item,
                    metadata,
                    occurrence,
                    target.id,
                    Skipped::NotGranted,
                )
                .await;
        }
        self.queue_tenant_run(item, metadata, occurrence, &scope)
            .await
    }

    async fn tenant_run_exists(&self, occurrence: i64, tenant: i64) -> Result<bool> {
        Ok(self
            .db
            .get()
            .await?
            .query_opt(
                "SELECT 1 FROM base_schedule_tenant_runs WHERE occurrence_id=$1 AND tenant_id=$2",
                &[&occurrence, &tenant],
            )
            .await?
            .is_some())
    }

    /// A tenant with no job: written in the operator tenant (the fan-out job's
    /// own audited transaction), because nothing may be written in a tenant
    /// that has not consented or cannot act.
    async fn record_skipped_tenant(
        &self,
        execution: &Execution,
        item: &Registered,
        metadata: &Metadata,
        occurrence: i64,
        tenant: i64,
        why: Skipped,
    ) -> Result<()> {
        let (status, code) = match why {
            Skipped::Suspended => ("skipped", "tenant_suspended"),
            Skipped::NotGranted => ("not_granted", "not_granted"),
        };
        let mut client = self.db.get().await?;
        let mut tx = execution
            .begin(&mut client, "schedule.tenant_run.recorded")
            .await?;
        let key = item.key().to_owned();
        let trigger = metadata.trigger.as_str();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = sql
                    .query_one(
                        "INSERT INTO base_schedule_tenant_runs(occurrence_id,key,tenant_id,trigger,status,code,finished_at) \
                         VALUES($1,$2,$3,$4,$5,$6,clock_timestamp()) RETURNING id",
                        &[&occurrence, &key, &tenant, &trigger, &status, &code],
                    )
                    .await?
                    .get(0);
                Ok((
                    (),
                    vec![Change {
                        kind: "base/schedule.tenant_run".into(),
                        id,
                        old: Value::Null,
                        new: json!({"key": key, "occurrence_id": occurrence, "tenant_id": tenant, "status": status, "code": code}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }

    /// The tenant's run row and its job commit together in that tenant's audit
    /// context, as the tenant's `scheduler` principal. Under the tenant's
    /// advisory lock a forbid-overlap definition whose previous run is still
    /// unfinished gets a `skipped` row instead of a job.
    async fn queue_tenant_run(
        &self,
        item: &Registered,
        metadata: &Metadata,
        occurrence: i64,
        scope: &SystemScope,
    ) -> Result<()> {
        let key = item.key();
        let tenant = scope.tenant();
        let mut client = self.db.get().await?;
        let mut tx = self
            .boundary
            .begin(
                &mut client,
                scope.mutation_context("schedule.tenant_run.queued"),
            )
            .await?;
        tx.sql()
            .execute(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                &[&format!("schedule-tenant:{key}:{tenant}")],
            )
            .await?;
        if tx
            .sql()
            .query_opt(
                "SELECT 1 FROM base_schedule_tenant_runs WHERE occurrence_id=$1 AND tenant_id=$2",
                &[&occurrence, &tenant],
            )
            .await?
            .is_some()
        {
            tx.rollback().await?;
            return Ok(());
        }
        let forbid = item.definition.overlap == Overlap::Forbid;
        let overlap_key = forbid.then(|| format!("tenant:{key}:{tenant}"));
        // A cancelled or vanished job must not hold the tenant's lease.
        crate::occurrence::reconcile_tenant_runs(tx.sql(), Some(key), Some(tenant)).await?;
        let overlapped = match &overlap_key {
            Some(overlap_key) => tx
                .sql()
                .query_opt(
                    "SELECT 1 FROM base_schedule_tenant_runs WHERE overlap_key=$1 AND finished_at IS NULL",
                    &[overlap_key],
                )
                .await?
                .is_some(),
            None => false,
        };
        let trigger = metadata.trigger.as_str();
        let owned_key = key.to_owned();
        if overlapped {
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO base_schedule_tenant_runs(occurrence_id,key,tenant_id,trigger,status,code,finished_at) \
                             VALUES($1,$2,$3,$4,'skipped','overlap',clock_timestamp()) RETURNING id",
                            &[&occurrence, &owned_key, &tenant, &trigger],
                        )
                        .await?
                        .get(0);
                    Ok((
                        (),
                        vec![Change {
                            kind: "base/schedule.tenant_run".into(),
                            id,
                            old: Value::Null,
                            new: json!({"key": owned_key, "occurrence_id": occurrence, "tenant_id": tenant, "status": "skipped", "code": "overlap"}),
                        }],
                    ))
                })
            })
            .await?;
            return tx.commit().await;
        }
        let mut args = item.args.clone();
        args[RESERVED_ARGUMENT] = metadata.to_json();
        let job = self
            .queue
            .enqueue_system(&mut tx, scope, item.definition.worker, args)
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let id: i64 = sql
                    .query_one(
                        "INSERT INTO base_schedule_tenant_runs(occurrence_id,key,tenant_id,trigger,status,job_id,overlap_key) \
                         VALUES($1,$2,$3,$4,'queued',$5,$6) RETURNING id",
                        &[&occurrence, &owned_key, &tenant, &trigger, &job.id(), &overlap_key],
                    )
                    .await?
                    .get(0);
                Ok((
                    (),
                    vec![Change {
                        kind: "base/schedule.tenant_run".into(),
                        id,
                        old: Value::Null,
                        new: json!({"key": owned_key, "occurrence_id": occurrence, "tenant_id": tenant, "status": "queued", "job_id": job.id()}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }
}
