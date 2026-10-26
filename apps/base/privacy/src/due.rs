//! Notices as a request nears its due date (ADR 05). One contributed daily
//! schedule definition, which runs only after an operator enables it, visits
//! every tenant as the scheduler principal and pages through its open
//! requests whose due date falls within `privacy.request.notice_days`, in due
//! order, past any it cannot notify. Each request is rechecked under its row
//! lock, so a request closed meanwhile gets no notice, and its handlers who
//! are still active accounts are told once. The due instant is shown in the
//! request's company zone, named, and in UTC. No Bilimbi counterpart.
use crate::{requests::REQUEST_KIND, retention::tenants, Privacy};
use bilibi_authz::{SystemAuthentication, SystemScope};
use bilibi_database::{Change, Error, Result};
use bilibi_datetime::{DisplayContext, Format, Mode, Precision};
use bilibi_notifications::{Notice, NotificationBoundary};
use bilibi_party_ids::OrganisationId;
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome};
use bilibi_schedule::Schedule;
use bilibi_settings::Target;
use bilibi_tenancy::{Authentication, Scope};
use serde_json::{json, Value};
use std::sync::Arc;
use tokio_postgres::Client;

/// Tenant setting: days before the due date that a request's handlers are told.
pub const NOTICE_DAYS: &str = "privacy.request.notice_days";
pub(crate) const NOTICE_DEFAULT: i64 = 3;
pub const DUE_WORKER: &str = "base/privacy-due-notices";
pub const DUE_SCHEDULE: &str = "base/privacy.due-notices";
const ACTION: &str = "privacy.request.due_notice";
/// Requests read per page; the scan pages on until none are left.
const PAGE: i64 = 500;

/// The definition takes no arguments of its own.
pub(crate) fn arguments(args: Value) -> Result<Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(Error::Invalid),
    }
}

pub(crate) struct DueJob {
    pub(crate) privacy: Arc<Privacy>,
    pub(crate) schedule: Arc<Schedule>,
    pub(crate) authentication: Arc<Authentication>,
    pub(crate) notifications: Arc<NotificationBoundary>,
}
impl Handler for DueJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            self.schedule
                .run(execution, args, |_| async move {
                    match self.scan().await {
                        Ok(()) => Outcome::Completed,
                        Err(_) => Outcome::Retry("due_notices_failed"),
                    }
                })
                .await
        })
    }
}
impl DueJob {
    async fn scan(&self) -> Result<()> {
        let mut client = self.privacy.db.get().await?;
        let mut failed = false;
        for tenant in tenants(&client).await? {
            failed |= self.tenant(&mut client, tenant).await.is_err();
        }
        if failed {
            Err(Error::Internal)
        } else {
            Ok(())
        }
    }
    async fn tenant(&self, client: &mut Client, tenant: i64) -> Result<()> {
        let system =
            SystemScope::rehydrate(&*client, tenant, Some(bilibi_schedule::PRINCIPAL)).await?;
        let scope = self.authentication.for_system(&*client, &system).await?;
        let days = self
            .privacy
            .settings
            .get(&*client, &scope, Target::Tenant, NOTICE_DAYS)
            .await?
            .value
            .as_i64()
            .filter(|days| (1..=60).contains(days))
            .unwrap_or(NOTICE_DEFAULT) as i32;
        // Keyset over (due_at, id): a request that cannot be notified does
        // not hold back the ones after it.
        let mut after: Option<(String, i64)> = None;
        loop {
            let (at, id) = after.clone().unwrap_or(("-infinity".into(), 0));
            let page = client
                .query(
                    "SELECT id,due_at::text FROM base_privacy_requests WHERE tenant_id=$1 AND status NOT IN ('completed','refused') \
                     AND due_notified_at IS NULL AND due_at <= clock_timestamp() + make_interval(days => $2::integer) \
                     AND (due_at, id) > ($3::text::timestamptz, $4) ORDER BY due_at,id LIMIT $5",
                    &[&tenant, &days, &at, &id, &PAGE],
                )
                .await?;
            let done = (page.len() as i64) < PAGE;
            for row in &page {
                let id: i64 = row.get(0);
                after = Some((row.get(1), id));
                self.notify(client, &scope, id, days).await?;
            }
            if done {
                return Ok(());
            }
        }
    }
    /// Notifies one request's handlers, rechecking it under its row lock.
    async fn notify(&self, client: &mut Client, scope: &Scope, id: i64, days: i32) -> Result<()> {
        let tenant = scope.tenant();
        let mut tx = self
            .privacy
            .boundary
            .begin(client, scope.mutation_context(ACTION))
            .await?;
        let Some(row) = tx
            .sql()
            .query_opt(
                "SELECT received_by,verified_by,approved_by,company_id,\
                 to_char(due_at AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') FROM base_privacy_requests \
                 WHERE tenant_id=$1 AND id=$2 AND due_notified_at IS NULL AND status NOT IN ('completed','refused') \
                 AND due_at <= clock_timestamp() + make_interval(days => $3::integer) FOR UPDATE",
                &[&tenant, &id, &days],
            )
            .await?
        else {
            // Closed, notified or no longer near since the page was read.
            return tx.rollback().await;
        };
        let handlers: Vec<i64> = [row.get::<_, Option<i64>>(0), row.get(1), row.get(2)]
            .into_iter()
            .flatten()
            .collect();
        let recipients = self.notifications.eligible(&tx, &handlers).await?;
        if recipients.is_empty() {
            return tx.rollback().await;
        }
        let company = OrganisationId::new(row.get(3)).map_err(|_| Error::Internal)?;
        let due: String = row.get(4);
        let instant: jiff::Timestamp = due.parse().map_err(|_| Error::Internal)?;
        let timezone = self
            .privacy
            .settings
            .get(
                tx.sql(),
                scope,
                Target::Company(company),
                bilibi_datetime::TIMEZONE_KEY,
            )
            .await?
            .value
            .as_str()
            .filter(|zone| bilibi_datetime::valid_timezone(zone))
            .unwrap_or(bilibi_datetime::DEFAULT_TIMEZONE)
            .to_owned();
        let shown = DisplayContext {
            mode: Mode::Company,
            timezone,
            locale: bilibi_locale::FALLBACK,
        }
        .render(instant, Format::DateTime, Precision::Minute);
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE base_privacy_requests SET due_notified_at=clock_timestamp() WHERE tenant_id=$1 AND id=$2",
                    &[&tenant, &id],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: REQUEST_KIND.into(),
                        id,
                        old: json!({"due_notified": false}),
                        new: json!({"due_notified": true}),
                    }],
                ))
            })
        })
        .await?;
        let notice = Notice::new(format!(
            "Personal-data request {id} is due {}",
            shown.company
        ))
        .kind("privacy.request")
        .body(if shown.company == shown.utc {
            format!("Answer it in every store before {}.", shown.utc)
        } else {
            format!(
                "Answer it in every store before {} ({}).",
                shown.company, shown.utc
            )
        })
        .url(format!("{}/{id}", crate::web::REQUESTS_PATH));
        self.notifications
            .notify(&mut tx, scope, &recipients, &notice)
            .await?;
        tx.commit().await
    }
}
