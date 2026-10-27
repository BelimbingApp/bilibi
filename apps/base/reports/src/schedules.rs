//! Scheduled deliveries. A subscription asks for one
//! report daily, weekly or monthly at a wall-clock time in its zone, for its
//! subscriber and the holders of chosen roles. Schedule's contributed
//! recurrence (`base/reports.deliveries`, run by the `scheduler` principal)
//! sweeps due subscriptions: it records one delivery per recipient, proves
//! the recipient's account, and queues the delivery as that recipient, so
//! Queue re-proves them on every attempt. The delivery runs the report under
//! their own access, keeps the file as their prepared export, rings their
//! bell and mails them a link that needs sign-in. A recipient who has lost
//! access (a file is an export, so that includes `reports.export`), or whose
//! result is empty when the subscription skips empty results, gets nothing;
//! the delivery records why. No Bilimbi counterpart.
use crate::{
    authorize, contributions,
    export::Format,
    exports::{self, Purpose},
    store, ReportError, Reports, EXPORT, SCHEDULE, VIEW,
};
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_queue::{
    execution::{Execution, Handler, HandlerFuture, Outcome as JobOutcome},
    store as jobs,
};
use bilibi_tenancy::Scope;
use jiff::{civil::Date, tz::TimeZone, Timestamp};
use serde::Serialize;
use serde_json::{json, Value};
use std::sync::Arc;

/// The scheduled sweep and the per-recipient delivery.
pub const SWEEP: &str = "base/reports-schedule";
pub const DELIVER: &str = "base/reports-deliver";
const KIND: &str = "base_report_subscription";
const DELIVERY: &str = "base_report_delivery";
/// Recipients one delivery round reaches at most.
const MAX_RECIPIENTS: usize = 500;
/// Due subscriptions and pending deliveries one sweep takes per tenant.
const BATCH: i64 = 100;

type Outcome<T> = Result<T, ReportError>;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Frequency {
    Daily,
    Weekly,
    Monthly,
}
impl Frequency {
    pub fn key(self) -> &'static str {
        match self {
            Self::Daily => "daily",
            Self::Weekly => "weekly",
            Self::Monthly => "monthly",
        }
    }
    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "daily" => Some(Self::Daily),
            "weekly" => Some(Self::Weekly),
            "monthly" => Some(Self::Monthly),
            _ => None,
        }
    }
}

/// When and how a subscription delivers.
#[derive(Clone, Debug)]
pub struct Plan {
    pub frequency: Frequency,
    /// Minutes after local midnight.
    pub at_minute: i16,
    /// 1 = Monday … 7 = Sunday, weekly only.
    pub weekday: Option<i16>,
    /// 1–28, monthly only.
    pub monthday: Option<i16>,
    pub format: Format,
    pub include_owner: bool,
    pub skip_empty: bool,
    pub roles: Vec<i64>,
}

/// A subscription as its owner sees it.
#[derive(Clone, Debug, Serialize)]
pub struct Subscription {
    pub id: i64,
    pub report_id: i64,
    pub frequency: Frequency,
    pub at_minute: i16,
    pub weekday: Option<i16>,
    pub monthday: Option<i16>,
    pub timezone: String,
    pub format: &'static str,
    pub include_owner: bool,
    pub skip_empty: bool,
    pub paused: bool,
    pub roles: Vec<i64>,
    pub next_run_at: String,
    pub last_run_at: Option<String>,
    /// The latest delivery to the owner: `sent`, `skipped` with its reason.
    pub last_state: Option<String>,
    pub last_reason: Option<String>,
    pub version: i64,
}

/// The first wall-clock occurrence of `plan` after `after`, in `zone`.
pub fn next_run(after: Timestamp, plan: &Plan, zone: &str) -> Option<Timestamp> {
    let tz = TimeZone::get(zone).ok()?;
    let mut date: Date = after.to_zoned(tz.clone()).date();
    let (hour, minute) = ((plan.at_minute / 60) as i8, (plan.at_minute % 60) as i8);
    for _ in 0..400 {
        let due = match plan.frequency {
            Frequency::Daily => true,
            Frequency::Weekly => Some(date.weekday().to_monday_one_offset() as i16) == plan.weekday,
            Frequency::Monthly => Some(date.day() as i16) == plan.monthday,
        };
        if due {
            let at = date
                .at(hour, minute, 0, 0)
                .to_zoned(tz.clone())
                .ok()?
                .timestamp();
            if at > after {
                return Some(at);
            }
        }
        date = date.tomorrow().ok()?;
    }
    None
}

fn check(plan: &Plan) -> Outcome<()> {
    let bad = |field: &str, message: &str| {
        Err(ReportError::Invalid(std::collections::BTreeMap::from([(
            field.to_owned(),
            vec![message.to_owned()],
        )])))
    };
    if !(0..1440).contains(&plan.at_minute) {
        return bad("at", "must be a time of day");
    }
    match plan.frequency {
        Frequency::Weekly if !plan.weekday.is_some_and(|d| (1..=7).contains(&d)) => {
            return bad("weekday", "must be a day of the week")
        }
        Frequency::Monthly if !plan.monthday.is_some_and(|d| (1..=28).contains(&d)) => {
            return bad("monthday", "must be a day from 1 to 28")
        }
        _ => {}
    }
    if !plan.include_owner && plan.roles.is_empty() {
        return bad("recipients", "must include you or at least one role");
    }
    if plan.roles.len() > 50 {
        return bad("roles", "names at most 50 roles");
    }
    Ok(())
}

const COLUMNS: &str = "s.id, s.report_id, s.frequency, s.at_minute, s.weekday, s.monthday, s.timezone, s.format, s.include_owner, s.skip_empty, s.paused, COALESCE((SELECT array_agg(r.role_id ORDER BY r.role_id) FROM base_report_subscription_roles r WHERE r.tenant_id=s.tenant_id AND r.subscription_id=s.id), '{}'), to_char(s.next_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), to_char(s.last_run_at AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"'), d.state, d.reason, s.version";
const LAST: &str = "LEFT JOIN LATERAL (SELECT state, reason FROM base_report_deliveries x WHERE x.tenant_id=s.tenant_id AND x.subscription_id=s.id AND x.recipient_id=s.owner_id ORDER BY x.due_at DESC LIMIT 1) d ON true";

fn subscription(row: &tokio_postgres::Row) -> Subscription {
    let format: String = row.get(7);
    Subscription {
        id: row.get(0),
        report_id: row.get(1),
        frequency: Frequency::from_key(row.get(2)).unwrap_or(Frequency::Daily),
        at_minute: row.get(3),
        weekday: row.get(4),
        monthday: row.get(5),
        timezone: row.get(6),
        format: if format == "pdf" { "pdf" } else { "csv" },
        include_owner: row.get(8),
        skip_empty: row.get(9),
        paused: row.get(10),
        roles: row.get(11),
        next_run_at: row.get(12),
        last_run_at: row.get(13),
        last_state: row.get(14),
        last_reason: row.get(15),
        version: row.get(16),
    }
}

impl Reports {
    /// The reader's own subscriptions to a report. Needs `reports.schedule`.
    pub async fn subscriptions(&self, scope: &Scope, report: i64) -> Outcome<Vec<Subscription>> {
        let client = self.db.get().await.map_err(Error::from)?;
        authorize(&**client, scope, SCHEDULE).await?;
        let rows = client
            .query(
                &format!("SELECT {COLUMNS} FROM base_report_subscriptions s {LAST} WHERE s.tenant_id=$1 AND s.owner_id=$2 AND s.report_id=$3 ORDER BY s.id"),
                &[&scope.tenant(), &scope.user(), &report],
            )
            .await
            .map_err(Error::from)?;
        Ok(rows.iter().map(subscription).collect())
    }

    /// Subscribes the reader to a report they may run, in the caller's
    /// audited transaction. Roles need `reports.share` too; the time is read
    /// in the reader's display zone. Needs `reports.schedule`.
    pub async fn subscribe(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        report: i64,
        plan: &Plan,
    ) -> Outcome<i64> {
        authorize(tx.sql(), scope, SCHEDULE).await?;
        check(plan)?;
        if !plan.roles.is_empty() {
            authorize(tx.sql(), scope, crate::SHARE).await?;
            let mut roles = plan.roles.clone();
            roles.sort_unstable();
            roles.dedup();
            if store::known_roles(tx.sql(), scope.tenant(), &roles).await? != roles {
                return Err(ReportError::Invalid(std::collections::BTreeMap::from([(
                    "roles".to_owned(),
                    vec!["names a role that does not exist".to_owned()],
                )])));
            }
        }
        match store::read(tx.sql(), scope.tenant(), scope.user(), report, false).await {
            Err(Error::NotFound) => return Err(ReportError::NotFound),
            other => other?,
        };
        let limit = self
            .setting(tx.sql(), scope, contributions::MAX_SUBSCRIPTIONS_KEY)
            .await? as i64;
        let held: i64 = tx
            .sql()
            .query_one(
                "SELECT count(*) FROM base_report_subscriptions WHERE tenant_id=$1 AND owner_id=$2",
                &[&scope.tenant(), &scope.user()],
            )
            .await
            .map_err(Error::from)?
            .get(0);
        if held >= limit {
            return Err(ReportError::Refused(format!(
                "You already have {held} scheduled reports, the most allowed. Delete one first."
            )));
        }
        let zone = self
            .display
            .display(tx.sql(), scope, None, "en")
            .await?
            .timezone;
        let zone = if TimeZone::get(&zone).is_ok() {
            zone
        } else {
            "UTC".to_owned()
        };
        let next = next_run(Timestamp::now(), plan, &zone).ok_or_else(|| {
            ReportError::Refused("That time does not occur in your time zone.".into())
        })?;
        let (tenant, owner, plan, next) =
            (scope.tenant(), scope.user(), plan.clone(), next.to_string());
        Ok(tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO base_report_subscriptions(tenant_id,report_id,owner_id,frequency,at_minute,weekday,monthday,timezone,format,include_owner,skip_empty,next_run_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12::text::timestamptz) RETURNING id",
                            &[&tenant, &report, &owner, &plan.frequency.key(), &plan.at_minute, &plan.weekday, &plan.monthday, &zone, &plan.format.key(), &plan.include_owner, &plan.skip_empty, &next],
                        )
                        .await?
                        .get(0);
                    sql.execute(
                        "INSERT INTO base_report_subscription_roles(tenant_id,subscription_id,role_id) SELECT $1,$2,role FROM unnest($3::bigint[]) role",
                        &[&tenant, &id, &plan.roles],
                    )
                    .await?;
                    let new = json!({"report_id": report, "frequency": plan.frequency.key(), "at_minute": plan.at_minute, "format": plan.format.key(), "roles": plan.roles});
                    Ok((id, vec![Change { kind: KIND.into(), id, old: Value::Null, new }]))
                })
            })
            .await?)
    }

    /// Pauses or resumes an own subscription at `version`. Pausing cancels
    /// the deliveries not yet started.
    pub async fn pause(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
        paused: bool,
    ) -> Outcome<()> {
        authorize(tx.sql(), scope, SCHEDULE).await?;
        self.own_subscription(tx, scope, id, version).await?;
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE base_report_subscriptions SET paused=$3, version=version+1 WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id, &paused],
                )
                .await?;
                let mut changes = vec![Change { kind: KIND.into(), id, old: json!({"paused": !paused}), new: json!({"paused": paused}) }];
                if paused {
                    let cancelled = sql
                        .query(
                            "UPDATE base_report_deliveries d SET state='cancelled', reason='The subscription was paused.', finished_at=now() FROM base_report_deliveries o WHERE o.tenant_id=d.tenant_id AND o.id=d.id AND d.tenant_id=$1 AND d.subscription_id=$2 AND d.state IN ('pending','queued') RETURNING d.id, o.state",
                            &[&tenant, &id],
                        )
                        .await?;
                    changes.extend(cancelled.iter().map(|row| Change {
                        kind: DELIVERY.into(),
                        id: row.get(0),
                        old: json!({"state": row.get::<_, String>(1)}),
                        new: json!({"state": "cancelled", "reason": "The subscription was paused."}),
                    }));
                }
                Ok(((), changes))
            })
        })
        .await
        .map_err(ReportError::from)
    }

    /// Deletes an own subscription at `version`, with its deliveries.
    pub async fn unsubscribe(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> Outcome<()> {
        authorize(tx.sql(), scope, SCHEDULE).await?;
        self.own_subscription(tx, scope, id, version).await?;
        let tenant = scope.tenant();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let deliveries = sql
                    .query(
                        "DELETE FROM base_report_deliveries WHERE tenant_id=$1 AND subscription_id=$2 RETURNING id, state",
                        &[&tenant, &id],
                    )
                    .await?;
                sql.execute(
                    "DELETE FROM base_report_subscriptions WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id],
                )
                .await?;
                let mut changes: Vec<Change> = deliveries
                    .iter()
                    .map(|row| Change {
                        kind: DELIVERY.into(),
                        id: row.get(0),
                        old: json!({"state": row.get::<_, String>(1)}),
                        new: Value::Null,
                    })
                    .collect();
                changes.push(Change {
                    kind: KIND.into(),
                    id,
                    old: json!({"id": id}),
                    new: Value::Null,
                });
                Ok(((), changes))
            })
        })
        .await
        .map_err(ReportError::from)
    }

    async fn own_subscription(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
        version: i64,
    ) -> Outcome<()> {
        let row = tx
            .sql()
            .query_opt(
                "SELECT version FROM base_report_subscriptions WHERE tenant_id=$1 AND id=$2 AND owner_id=$3 FOR UPDATE",
                &[&scope.tenant(), &id, &scope.user()],
            )
            .await
            .map_err(Error::from)?
            .ok_or(ReportError::NotFound)?;
        if row.get::<_, i64>(0) != version {
            return Err(ReportError::Conflict);
        }
        Ok(())
    }

    /// One sweep, as the `scheduler` principal: every tenant's due
    /// subscriptions become deliveries, and pending deliveries are queued as
    /// their recipients. Bounded per tenant; the next sweep takes the rest.
    pub(crate) async fn sweep(&self) -> JobOutcome {
        let Ok(client) = self.db.get().await else {
            return JobOutcome::Retry("database_unavailable");
        };
        let Ok(tenants) = client.query("SELECT DISTINCT tenant_id FROM base_report_subscriptions WHERE NOT paused AND next_run_at <= now()  UNION SELECT DISTINCT tenant_id FROM base_report_deliveries WHERE state IN ('pending','queued')", &[]).await else {
            return JobOutcome::Retry("database_unavailable");
        };
        drop(client);
        for tenant in tenants.iter().map(|row| row.get::<_, i64>(0)) {
            if let Err(error) = self.due(tenant).await {
                eprintln!("reports: scheduled sweep of tenant {tenant} stopped: {error}");
                return JobOutcome::Retry("sweep_failed");
            }
            self.queue_pending(tenant).await;
            if let Err(error) = self.reconcile(tenant).await {
                eprintln!("reports: delivery reconciliation of tenant {tenant} stopped: {error}");
            }
        }
        JobOutcome::Completed
    }

    /// Closes each queued delivery whose job Queue has ended without the
    /// delivery finishing it (the recipient was refused before it ran, or
    /// every attempt failed), so none waits for ever.
    async fn reconcile(&self, tenant: i64) -> Result<(), Error> {
        let mut client = self.db.get().await?;
        let queued = client
            .query(
                "SELECT id, job_id FROM base_report_deliveries WHERE tenant_id=$1 AND state='queued' AND job_id IS NOT NULL ORDER BY id LIMIT $2",
                &[&tenant, &(BATCH * 5)],
            )
            .await?;
        if queued.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = queued.iter().map(|row| row.get(1)).collect();
        let ended: Vec<i64> = jobs::states(&**client, tenant, &ids)
            .await?
            .into_iter()
            .filter(|job| job.state.terminal())
            .map(|job| job.id)
            .collect();
        let stopped: Vec<i64> = queued
            .iter()
            .filter(|row| ended.contains(&row.get(1)))
            .map(|row| row.get(0))
            .collect();
        if stopped.is_empty() {
            return Ok(());
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                sweep_context(tenant, "reports.delivery.reconcile"),
            )
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let reason = "The delivery stopped before it finished; the recipient may no longer be allowed to receive it.";
                let closed = sql
                    .query(
                        "UPDATE base_report_deliveries SET state='skipped', reason=$3, finished_at=now() WHERE tenant_id=$1 AND id=ANY($2) AND state='queued' RETURNING id",
                        &[&tenant, &stopped, &reason],
                    )
                    .await?;
                let changes = closed
                    .iter()
                    .map(|row| Change { kind: DELIVERY.into(), id: row.get(0), old: json!({"state": "queued"}), new: json!({"state": "skipped", "reason": reason}) })
                    .collect();
                Ok(((), changes))
            })
        })
        .await?;
        tx.commit().await
    }

    /// Advances each due subscription to its next run and records one
    /// pending delivery per recipient, in one audited transaction.
    async fn due(&self, tenant: i64) -> Result<(), Error> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, sweep_context(tenant, "reports.schedule.sweep"))
            .await?;
        let due = tx
            .sql()
            .query(
                "SELECT id, owner_id, include_owner, frequency, at_minute, weekday, monthday, timezone, format, skip_empty, next_run_at::text FROM base_report_subscriptions WHERE tenant_id=$1 AND NOT paused AND next_run_at <= now() ORDER BY next_run_at LIMIT $2 FOR UPDATE SKIP LOCKED",
                &[&tenant, &BATCH],
            )
            .await?;
        for row in due {
            let id: i64 = row.get(0);
            let owner: i64 = row.get(1);
            let plan = Plan {
                frequency: Frequency::from_key(row.get(3)).unwrap_or(Frequency::Daily),
                at_minute: row.get(4),
                weekday: row.get(5),
                monthday: row.get(6),
                format: Format::Csv,
                include_owner: row.get(2),
                skip_empty: row.get(9),
                roles: Vec::new(),
            };
            let zone: String = row.get(7);
            let due_at: String = row.get(10);
            let next = next_run(Timestamp::now(), &plan, &zone)
                .unwrap_or_else(|| {
                    let now = Timestamp::now();
                    now.checked_add(jiff::SignedDuration::from_hours(24))
                        .unwrap_or(now)
                })
                .to_string();
            let include_owner = plan.include_owner;
            tx.mutate(move |sql| {
                Box::pin(async move {
                    let mut recipients: Vec<i64> = sql
                        .query(
                            "SELECT DISTINCT p.user_id FROM base_report_subscription_roles r JOIN effective_principal_roles p ON p.tenant_id=r.tenant_id AND p.role_id=r.role_id WHERE r.tenant_id=$1 AND r.subscription_id=$2 ORDER BY 1 LIMIT $3",
                            &[&tenant, &id, &(MAX_RECIPIENTS as i64)],
                        )
                        .await?
                        .iter()
                        .map(|r| r.get(0))
                        .collect();
                    if include_owner && !recipients.contains(&owner) {
                        recipients.push(owner);
                    }
                    recipients.truncate(MAX_RECIPIENTS);
                    let created = sql
                        .query(
                            "INSERT INTO base_report_deliveries(tenant_id,subscription_id,recipient_id,due_at) SELECT $1,$2,recipient,$3::text::timestamptz FROM unnest($4::bigint[]) recipient ON CONFLICT ON CONSTRAINT base_report_deliveries_once DO NOTHING RETURNING id, recipient_id",
                            &[&tenant, &id, &due_at, &recipients],
                        )
                        .await?;
                    sql.execute(
                        "UPDATE base_report_subscriptions SET next_run_at=$3::text::timestamptz, last_run_at=now() WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &id, &next],
                    )
                    .await?;
                    let new = json!({"due_at": due_at, "recipients": recipients.len(), "next_run_at": next});
                    let mut changes = vec![Change { kind: KIND.into(), id, old: Value::Null, new }];
                    changes.extend(created.iter().map(|row| Change {
                        kind: DELIVERY.into(),
                        id: row.get(0),
                        old: Value::Null,
                        new: json!({"subscription_id": id, "recipient_id": row.get::<_, i64>(1), "due_at": due_at, "state": "pending"}),
                    }));
                    Ok(((), changes))
                })
            })
            .await?;
        }
        tx.commit().await
    }

    /// Queues each pending delivery as its recipient. A recipient whose
    /// account no longer proves, or who cannot run reports, is skipped with
    /// the reason; the rest are queued in their own audited transaction.
    async fn queue_pending(&self, tenant: i64) {
        let Ok(client) = self.db.get().await else {
            return;
        };
        let Ok(pending) = client
            .query(
                "SELECT id, recipient_id FROM base_report_deliveries WHERE tenant_id=$1 AND state='pending' ORDER BY id LIMIT $2",
                &[&tenant, &(BATCH * 5)],
            )
            .await
        else {
            return;
        };
        drop(client);
        for row in pending {
            let (delivery, recipient): (i64, i64) = (row.get(0), row.get(1));
            if let Err(error) = self.queue_one(tenant, delivery, recipient).await {
                eprintln!("reports: delivery {delivery} not queued: {error}");
            }
        }
    }

    async fn queue_one(&self, tenant: i64, delivery: i64, recipient: i64) -> Result<(), Error> {
        let mut client = self.db.get().await?;
        let scope = match self
            .authentication
            .authenticate(&**client, tenant, recipient, String::new())
            .await
        {
            Ok(scope) => scope,
            Err(Error::Unauthorized | Error::Forbidden | Error::NotFound) => {
                return self
                    .skip_unqueued(
                        tenant,
                        delivery,
                        "The recipient's account is no longer active.",
                    )
                    .await;
            }
            Err(error) => return Err(error),
        };
        for (capability, reason) in [
            (VIEW, "The recipient can no longer run reports."),
            (EXPORT, "The recipient may not export reports."),
        ] {
            match bilibi_authz::authorize_key(&**client, &scope, capability).await {
                Ok(()) => {}
                Err(Error::Forbidden) => return self.skip_unqueued(tenant, delivery, reason).await,
                Err(error) => return Err(error),
            }
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("reports.delivery.queue"),
            )
            .await?;
        let queued = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let moved = sql
                        .execute(
                            "UPDATE base_report_deliveries SET state='queued' WHERE tenant_id=$1 AND id=$2 AND state='pending'",
                            &[&tenant, &delivery],
                        )
                        .await?;
                    if moved != 1 {
                        return Err(Error::Conflict);
                    }
                    Ok(((), vec![Change { kind: DELIVERY.into(), id: delivery, old: json!({"state": "pending"}), new: json!({"state": "queued"}) }]))
                })
            })
            .await;
        match queued {
            Ok(()) => {}
            Err(Error::Conflict) => return Ok(()),
            Err(error) => return Err(error),
        }
        let job = self
            .queue
            .enqueue_for(&mut tx, &scope, DELIVER, json!({"delivery": delivery}))
            .await?
            .id();
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE base_report_deliveries SET job_id=$3 WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &delivery, &job],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: DELIVERY.into(),
                        id: delivery,
                        old: json!({"job_id": null}),
                        new: json!({"job_id": job}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }

    /// A pending delivery that can never be queued, recorded as skipped by
    /// the platform (no recipient scope exists to attribute it to).
    async fn skip_unqueued(&self, tenant: i64, delivery: i64, reason: &str) -> Result<(), Error> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, sweep_context(tenant, "reports.delivery.skip"))
            .await?;
        let reason = reason.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE base_report_deliveries SET state='skipped', reason=$3, finished_at=now() WHERE tenant_id=$1 AND id=$2 AND state='pending'",
                    &[&tenant, &delivery, &reason],
                )
                .await?;
                Ok(((), vec![Change { kind: DELIVERY.into(), id: delivery, old: json!({"state": "pending"}), new: json!({"state": "skipped", "reason": reason}) }]))
            })
        })
        .await?;
        tx.commit().await
    }

    /// One delivery, as its recipient: run the report under their access,
    /// keep the file as their prepared export, ring their bell and mail the
    /// link. Lost access or an empty result (when skipped) sends nothing.
    pub(crate) async fn deliver(&self, execution: &Execution, delivery: i64) -> JobOutcome {
        let Some(scope) = execution.scope() else {
            return JobOutcome::Cancel("not_a_user_job");
        };
        let Ok(client) = self.db.get().await else {
            return JobOutcome::Retry("database_unavailable");
        };
        let row = client
            .query_opt(
                "SELECT d.state, d.export_id, s.report_id, s.format, s.skip_empty, s.paused FROM base_report_deliveries d JOIN base_report_subscriptions s ON s.tenant_id=d.tenant_id AND s.id=d.subscription_id WHERE d.tenant_id=$1 AND d.id=$2 AND d.recipient_id=$3",
                &[&scope.tenant(), &delivery, &scope.user()],
            )
            .await;
        drop(client);
        let row = match row {
            Ok(Some(row)) => row,
            // Deleted with its subscription: nothing is owed.
            Ok(None) => return JobOutcome::Completed,
            Err(_) => return JobOutcome::Retry("database_unavailable"),
        };
        let state: String = row.get(0);
        if state != "queued" {
            return JobOutcome::Completed;
        }
        let (export, report, format, skip_empty, paused): (Option<i64>, i64, String, bool, bool) =
            (row.get(1), row.get(2), row.get(3), row.get(4), row.get(5));
        if paused {
            return self
                .close(
                    execution,
                    scope,
                    delivery,
                    "cancelled",
                    Some("The subscription was paused."),
                )
                .await;
        }
        let format = Format::from_key(&format).unwrap_or(Format::Csv);
        let export = match export {
            Some(export) => export,
            None => match self
                .delivery_export(execution, scope, delivery, report, format)
                .await
            {
                Ok(Some(export)) => export,
                Ok(None) => {
                    return self
                        .close(
                            execution,
                            scope,
                            delivery,
                            "skipped",
                            Some("The report is no longer shared with the recipient."),
                        )
                        .await
                }
                Err(_) => {
                    return self
                        .retry_or_close(
                            execution,
                            scope,
                            delivery,
                            "export_not_recorded",
                            "skipped",
                            "The report could not be prepared for the recipient.",
                        )
                        .await
                }
            },
        };
        match self
            .prepare(execution, export, Purpose::Delivery { skip_empty })
            .await
        {
            JobOutcome::Completed => {}
            other => return other,
        }
        let Ok(client) = self.db.get().await else {
            return self
                .retry_or_close(
                    execution,
                    scope,
                    delivery,
                    "database_unavailable",
                    "skipped",
                    "The delivery could not be completed after several tries.",
                )
                .await;
        };
        let outcome = client
            .query_opt(
                "SELECT state, message, report_name FROM base_report_exports WHERE tenant_id=$1 AND id=$2 AND requester_id=$3",
                &[&scope.tenant(), &export, &scope.user()],
            )
            .await;
        drop(client);
        let (state, message, name): (String, Option<String>, String) = match outcome {
            Ok(Some(row)) => (row.get(0), row.get(1), row.get(2)),
            Ok(None) => return JobOutcome::Retry("export_missing"),
            Err(_) => return JobOutcome::Retry("database_unavailable"),
        };
        // An empty result the subscription skips was refused before it was
        // stored or announced, with exports::EMPTY as its message.
        if state == "failed" {
            let reason =
                message.unwrap_or_else(|| "The report could not run for the recipient.".into());
            return self
                .close(execution, scope, delivery, "skipped", Some(&reason))
                .await;
        }
        // The link needs sign-in; the mail carries no data. A refused or
        // failed mail is retried; the bell has already rung.
        let address = match bilibi_mail::Address::new(Some(&scope.user_name), &scope.email) {
            Ok(address) => address,
            Err(_) => {
                return self
                    .close(
                        execution,
                        scope,
                        delivery,
                        "sent",
                        Some("Delivered to the bell; the account has no usable email."),
                    )
                    .await
            }
        };
        let message = bilibi_mail::Message::text(
            address,
            format!("Scheduled report: {}", name.chars().take(150).collect::<String>()),
            format!(
                "Your scheduled report {name} is ready.\n\nDownload it, signed in, before it expires:\n{}{}\n",
                self.origin,
                exports::path(export)
            ),
        );
        if self.mailer.deliver(&message).await.is_err() {
            return self
                .retry_or_close(
                    execution,
                    scope,
                    delivery,
                    "mail_failed",
                    "sent",
                    "Delivered to the bell; the email could not be sent.",
                )
                .await;
        }
        self.close(execution, scope, delivery, "sent", None).await
    }

    /// The prepared export a delivery fills, recorded on the delivery in
    /// one transaction so a retry reuses it. `None` when the recipient can
    /// no longer see the report.
    async fn delivery_export(
        &self,
        execution: &Execution,
        scope: &Scope,
        delivery: i64,
        report: i64,
        format: Format,
    ) -> Result<Option<i64>, Error> {
        let mut client = self.db.get().await?;
        let mut tx = execution
            .begin(&mut client, "reports.delivery.export")
            .await?;
        let company = match scope.actor().organisation() {
            Some(company) => company,
            None => return Ok(None),
        };
        let name = match store::read(tx.sql(), scope.tenant(), scope.user(), report, false).await {
            Ok(report) => report.name,
            Err(Error::NotFound) => return Ok(None),
            Err(error) => return Err(error),
        };
        let (tenant, user, job) = (scope.tenant(), scope.user(), execution.job_id);
        let format = format.key();
        let export = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let export: i64 = sql
                        .query_one(
                            "INSERT INTO base_report_exports(tenant_id,requester_id,company_id,report_id,report_name,format,job_id) VALUES($1,$2,$3,$4,$5,$6,$7) RETURNING id",
                            &[&tenant, &user, &company.get(), &report, &name, &format, &job],
                        )
                        .await?
                        .get(0);
                    sql.execute(
                        "UPDATE base_report_deliveries SET export_id=$3 WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &delivery, &export],
                    )
                    .await?;
                    Ok((export, vec![
                        Change { kind: "base_report_export".into(), id: export, old: Value::Null, new: json!({"report_id": report, "format": format, "delivery": delivery}) },
                        Change { kind: DELIVERY.into(), id: delivery, old: json!({"export_id": null}), new: json!({"export_id": export}) },
                    ]))
                })
            })
            .await?;
        tx.commit().await?;
        Ok(Some(export))
    }

    /// A fault that may pass: retried, until the job's last attempt, which
    /// closes the delivery as `state` with `reason` instead of leaving it
    /// queued for ever.
    async fn retry_or_close(
        &self,
        execution: &Execution,
        scope: &Scope,
        delivery: i64,
        code: &'static str,
        state: &'static str,
        reason: &str,
    ) -> JobOutcome {
        if !crate::exports::last_attempt(execution) {
            return JobOutcome::Retry(code);
        }
        self.close(execution, scope, delivery, state, Some(reason))
            .await
    }

    async fn close(
        &self,
        execution: &Execution,
        scope: &Scope,
        delivery: i64,
        state: &'static str,
        reason: Option<&str>,
    ) -> JobOutcome {
        let closed = async {
            let mut client = self.db.get().await?;
            let mut tx = execution.begin(&mut client, "reports.delivery.finish").await?;
            let (tenant, reason) = (scope.tenant(), reason.map(|r| r.chars().take(500).collect::<String>()));
            tx.mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE base_report_deliveries SET state=$3, reason=$4, finished_at=now() WHERE tenant_id=$1 AND id=$2 AND state='queued'",
                        &[&tenant, &delivery, &state, &reason],
                    )
                    .await?;
                    Ok(((), vec![Change { kind: DELIVERY.into(), id: delivery, old: json!({"state": "queued"}), new: json!({"state": state, "reason": reason}) }]))
                })
            })
            .await?;
            tx.commit().await
        }
        .await;
        match closed {
            Ok(()) => JobOutcome::Completed,
            Err(_) => JobOutcome::Retry("finish_failed"),
        }
    }
}

/// The sweep writes each tenant's own rows, so it is attributed in that
/// tenant to Schedule's `scheduler` principal, the one that runs it, not to
/// the tenant the scheduled job happens to live in.
fn sweep_context(tenant: i64, action: &str) -> bilibi_database::Context {
    bilibi_database::Context::system(tenant, bilibi_schedule::PRINCIPAL, action)
}

/// The scheduled sweep, run through Schedule so review and suppression apply.
pub(crate) struct Sweeper {
    pub reports: Arc<Reports>,
    pub schedule: Arc<bilibi_schedule::Schedule>,
}
impl Handler for Sweeper {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| self.reports.sweep()))
    }
}

/// One delivery, bound to [`DELIVER`].
pub(crate) struct Deliverer(pub Arc<Reports>);
impl Handler for Deliverer {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            match args["delivery"].as_i64() {
                Some(delivery) if delivery > 0 => self.0.deliver(execution, delivery).await,
                _ => JobOutcome::Cancel("invalid_arguments"),
            }
        })
    }
}

pub(crate) fn validate_delivery(args: Value) -> bilibi_database::Result<Value> {
    match args.as_object() {
        Some(object)
            if object.len() == 1
                && object
                    .get("delivery")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0) =>
        {
            Ok(args)
        }
        _ => Err(Error::Invalid),
    }
}
pub(crate) fn validate_sweep(args: Value) -> bilibi_database::Result<Value> {
    bilibi_schedule::worker::validate(args, |args| match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    })
}
