//! What the Schedule board shows per tenant, and a tenant administrator's own
//! Run now (ADR 17). One board serves both audiences: an operator reads every
//! definition with the outcome counts of its latest fan-out; a tenant
//! administrator reads only each-tenant definitions and only this tenant's
//! rows, and says plainly when the tenant has not consented.
use crate::{
    definition::Overlap,
    lock,
    occurrence::{self, Refusal, Trigger, TriggeredBy},
    worker, Claimed, Eligibility, Reach, Schedule, EXECUTE, PRINCIPAL, RESERVED_ARGUMENT,
    TENANT_RUN, TENANT_VIEW, TENANT_WORK, VIEW,
};
use bilibi_authz::{authorize_key, decide_system, in_operator_tenant, SystemScope};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use jiff::Timestamp;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use tokio_postgres::{Client, GenericClient};

/// Who is reading the board.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Audience {
    /// The platform operator: every definition and every tenant.
    Operator,
    /// A tenant administrator: each-tenant definitions, this tenant only.
    Tenant,
}

/// Outcomes of the latest fan-out of one definition, one count per tenant.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TenantCounts {
    pub intended_at: Option<Timestamp>,
    pub queued: i64,
    pub running: i64,
    pub succeeded: i64,
    pub failed: i64,
    pub skipped: i64,
    pub not_granted: i64,
}
impl TenantCounts {
    pub fn total(&self) -> i64 {
        self.queued + self.running + self.succeeded + self.failed + self.skipped + self.not_granted
    }
}
/// This tenant's standing for one definition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OwnTenant {
    /// Whether the `scheduler` principal may act here (the tenant's consent).
    pub granted: bool,
    /// The latest run row: its status and bounded code.
    pub latest: Option<(String, Option<String>)>,
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TenantFacts {
    Counts(TenantCounts),
    Own(OwnTenant),
}

impl Schedule {
    /// Operator (`VIEW`) or tenant administrator (`TENANT_VIEW`); anything else
    /// is refused. Both are decided on every call.
    pub(crate) async fn audience(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Audience> {
        match authorize_key(client, scope, VIEW).await {
            Ok(()) => Ok(Audience::Operator),
            Err(Error::Forbidden) => {
                authorize_key(client, scope, TENANT_VIEW).await?;
                Ok(Audience::Tenant)
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn tenant_facts(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        audience: Audience,
        keys: &[&str],
    ) -> Result<BTreeMap<String, TenantFacts>> {
        let mut facts = BTreeMap::new();
        match audience {
            Audience::Operator => {
                let rows = client
                    .query(
                        "SELECT o.key,o.intended_at,r.status,count(r.id) FROM \
                         (SELECT DISTINCT ON (key) id,key,intended_at FROM base_schedule_occurrences \
                          WHERE fan_out_through IS NOT NULL AND key=ANY($1) ORDER BY key,id DESC) o \
                         LEFT JOIN base_schedule_tenant_runs r ON r.occurrence_id=o.id GROUP BY o.key,o.intended_at,r.status",
                        &[&keys],
                    )
                    .await?;
                for row in rows {
                    let key: String = row.get(0);
                    let counts = match facts
                        .entry(key)
                        .or_insert_with(|| TenantFacts::Counts(TenantCounts::default()))
                    {
                        TenantFacts::Counts(counts) => counts,
                        TenantFacts::Own(_) => continue,
                    };
                    counts.intended_at = Some(occurrence::from_pg(row.get(1)));
                    let (status, count): (Option<String>, i64) = (row.get(2), row.get(3));
                    match status.as_deref() {
                        Some("queued") => counts.queued = count,
                        Some("running") => counts.running = count,
                        Some("succeeded") => counts.succeeded = count,
                        Some("failed") => counts.failed = count,
                        Some("skipped") => counts.skipped = count,
                        Some("not_granted") => counts.not_granted = count,
                        _ => {}
                    }
                }
            }
            Audience::Tenant => {
                let tenant = scope.tenant();
                let latest: BTreeMap<String, (String, Option<String>)> = client
                    .query(
                        "SELECT DISTINCT ON (key) key,status,code FROM base_schedule_tenant_runs \
                         WHERE tenant_id=$1 AND key=ANY($2) ORDER BY key,id DESC",
                        &[&tenant, &keys],
                    )
                    .await?
                    .into_iter()
                    .map(|row| (row.get(0), (row.get(1), row.get(2))))
                    .collect();
                let granted = self.consented(client, tenant).await?;
                for key in keys {
                    facts.insert(
                        (*key).to_owned(),
                        TenantFacts::Own(OwnTenant {
                            granted,
                            latest: latest.get(*key).cloned(),
                        }),
                    );
                }
            }
        }
        Ok(facts)
    }

    /// Whether the scheduler principal may run here: the tenant's grant, or in
    /// the operator tenant its `EXECUTE`. Read through Authz's one decision.
    async fn consented(&self, client: &impl GenericClient, tenant: i64) -> Result<bool> {
        let scope = SystemScope::rehydrate(client, tenant, Some(PRINCIPAL)).await?;
        let key = if in_operator_tenant(client, tenant).await? {
            EXECUTE
        } else {
            TENANT_WORK
        };
        Ok(decide_system(client, &scope, key).await?.allowed)
    }

    /// A tenant administrator runs one each-tenant definition now, in their
    /// own tenant only, as themselves (`TENANT_RUN`); it creates no system
    /// grant. The operator tenant is refused: its operators use the board's
    /// Run now, which fans out. Review, pause, overlap and the idempotent
    /// manual-occurrence key apply as for the scheduled run.
    pub async fn run_now_for_tenant(
        &self,
        client: &mut Client,
        scope: &Scope,
        key: &str,
        now: Timestamp,
    ) -> Result<std::result::Result<Claimed, Refusal>> {
        authorize_key(&*client, scope, TENANT_RUN).await?;
        let item = self.definitions.get(key).ok_or(Error::NotFound)?;
        if item.definition.reach != Reach::EachTenant {
            return Err(Error::NotFound);
        }
        let tenant = scope.tenant();
        if in_operator_tenant(&*client, tenant).await? {
            return Err(Error::Forbidden);
        }
        let name = scope.user_name.trim();
        let triggered_by = TriggeredBy {
            user_id: scope.user(),
            name: (!name.is_empty()).then(|| name.chars().take(255).collect()),
        };
        let intended_at = occurrence::micros(now);
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("schedule.tenant_run.queued"))
            .await?;
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
        tx.sql()
            .execute(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                &[&format!("schedule-tenant:{key}:{tenant}")],
            )
            .await?;
        occurrence::reconcile_tenant_runs(tx.sql(), Some(key), Some(tenant)).await?;
        let overlap_key =
            (item.definition.overlap == Overlap::Forbid).then(|| format!("tenant:{key}:{tenant}"));
        if let Some(overlap_key) = &overlap_key {
            if tx
                .sql()
                .query_opt(
                    "SELECT 1 FROM base_schedule_tenant_runs WHERE overlap_key=$1 AND finished_at IS NULL",
                    &[overlap_key],
                )
                .await?
                .is_some()
            {
                tx.rollback().await?;
                return Ok(Err(Refusal::Overlap));
            }
        }
        let at = occurrence::to_pg(intended_at);
        if tx
            .sql()
            .query_opt(
                "SELECT 1 FROM base_schedule_occurrences WHERE key=$1 AND intended_at=$2 AND trigger='manual'",
                &[&key, &at],
            )
            .await?
            .is_some()
        {
            tx.rollback().await?;
            return Ok(Err(Refusal::AlreadyClaimed));
        }
        let timing = self.timing(tx.sql(), key).await?;
        let metadata = worker::Metadata {
            key: key.into(),
            name: item.definition.task_name.into(),
            expression: None,
            fingerprint: timing.version.clone(),
            intended_at,
            trigger: Trigger::Manual,
            triggered_by_user_id: Some(triggered_by.user_id),
            triggered_by_name: triggered_by.name.clone(),
        };
        let mut args = item.args.clone();
        args[RESERVED_ARGUMENT] = metadata.to_json();
        let job = self
            .queue
            .enqueue_for(&mut tx, scope, item.definition.worker, args)
            .await?;
        let owned = key.to_owned();
        let occurrence_id = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let occurrence: i64 = sql
                        .query_one(
                            "INSERT INTO base_schedule_occurrences(key,intended_at,trigger,state,tenant_id,job_id) \
                             VALUES($1,$2,'manual','queued',$3,$4) RETURNING id",
                            &[&owned, &at, &tenant, &job.id()],
                        )
                        .await?
                        .get(0);
                    let run: i64 = sql
                        .query_one(
                            "INSERT INTO base_schedule_tenant_runs(occurrence_id,key,tenant_id,trigger,status,job_id,overlap_key) \
                             VALUES($1,$2,$3,'manual','queued',$4,$5) RETURNING id",
                            &[&occurrence, &owned, &tenant, &job.id(), &overlap_key],
                        )
                        .await?
                        .get(0);
                    let new: Value = json!({"key": owned, "occurrence_id": occurrence, "tenant_id": tenant, "status": "queued", "trigger": "manual", "job_id": job.id()});
                    Ok((
                        occurrence,
                        vec![
                            Change {
                                kind: "base/schedule.occurrence".into(),
                                id: occurrence,
                                old: Value::Null,
                                new: new.clone(),
                            },
                            Change {
                                kind: "base/schedule.tenant_run".into(),
                                id: run,
                                old: Value::Null,
                                new,
                            },
                        ],
                    ))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(Ok(Claimed {
            occurrence_id,
            job,
            intended_at,
            trigger: Trigger::Manual,
        }))
    }
}
