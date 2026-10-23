//! Notices as a request nears its due date (ADR 05). One contributed daily
//! schedule definition, which runs only after an operator enables it, visits
//! every tenant's open requests whose due date falls within the tenant's
//! `privacy.request.notice_days` and notifies the people handling each once.
//! No Bilimbi counterpart.
use crate::{requests::REQUEST_KIND, Privacy};
use bilibi_database::{Change, Context, Error, Result};
use bilibi_notifications::{Notice, NotificationBoundary};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome};
use bilibi_schedule::Schedule;
use bilibi_tenancy::{Authentication, Scope};
use serde_json::{json, Value};
use std::sync::Arc;

/// Tenant setting: days before the due date that a request's handlers are told.
pub const NOTICE_DAYS: &str = "privacy.request.notice_days";
pub(crate) const NOTICE_DEFAULT: i64 = 3;
pub const DUE_WORKER: &str = "base/privacy-due-notices";
pub const DUE_SCHEDULE: &str = "base/privacy.due-notices";
const ACTION: &str = "privacy.request.due_notice";
/// Requests notified per tenant in one run; the next run takes the rest.
const BATCH: i64 = 500;

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
        let tenants: Vec<i64> = client
            .query(
                "SELECT id FROM tenants WHERE deleted_at IS NULL ORDER BY id",
                &[],
            )
            .await?
            .iter()
            .map(|row| row.get(0))
            .collect();
        for tenant in tenants {
            let open = client
                .query(
                    "SELECT id,received_by,verified_by,approved_by,to_char(due_at AT TIME ZONE 'UTC','YYYY-MM-DD') \
                     FROM base_privacy_requests WHERE tenant_id=$1 AND status NOT IN ('completed','refused') \
                     AND due_notified_at IS NULL ORDER BY due_at,id LIMIT $2",
                    &[&tenant, &BATCH],
                )
                .await?;
            for row in open {
                let id: i64 = row.get(0);
                let due: String = row.get(4);
                // Only accounts still active in the tenant can be told; the
                // first of them carries the notice.
                let mut people: Vec<(i64, Scope)> = Vec::new();
                for user in [row.get::<_, Option<i64>>(1), row.get(2), row.get(3)]
                    .into_iter()
                    .flatten()
                {
                    if people.iter().any(|(known, _)| *known == user) {
                        continue;
                    }
                    if let Ok(scope) = self
                        .authentication
                        .authenticate(
                            &**client,
                            tenant,
                            user,
                            format!("privacy-due-{tenant}-{id}"),
                        )
                        .await
                    {
                        people.push((user, scope));
                    }
                }
                let Some((_, scope)) = people.first() else {
                    continue;
                };
                let days = self
                    .privacy
                    .settings
                    .get(
                        &**client,
                        scope,
                        bilibi_settings::Target::Tenant,
                        NOTICE_DAYS,
                    )
                    .await?
                    .value
                    .as_i64()
                    .filter(|days| (1..=60).contains(days))
                    .unwrap_or(NOTICE_DEFAULT);
                let near: bool = client
                    .query_one(
                        "SELECT due_at <= clock_timestamp() + make_interval(days => $3::integer) \
                         FROM base_privacy_requests WHERE tenant_id=$1 AND id=$2",
                        &[&tenant, &id, &(days as i32)],
                    )
                    .await?
                    .get(0);
                if !near {
                    continue;
                }
                let recipients: Vec<i64> = people.iter().map(|(user, _)| *user).collect();
                let mut tx = self
                    .privacy
                    .boundary
                    .begin(
                        &mut client,
                        Context::system(tenant, bilibi_schedule::PRINCIPAL, ACTION),
                    )
                    .await?;
                let marked = tx
                    .mutate(move |sql| {
                        Box::pin(async move {
                            let marked = sql
                                .query_opt(
                                    "UPDATE base_privacy_requests SET due_notified_at=clock_timestamp() \
                                     WHERE tenant_id=$1 AND id=$2 AND due_notified_at IS NULL RETURNING id",
                                    &[&tenant, &id],
                                )
                                .await?;
                            let changes = marked
                                .iter()
                                .map(|_| Change {
                                    kind: REQUEST_KIND.into(),
                                    id,
                                    old: json!({"due_notified": false}),
                                    new: json!({"due_notified": true}),
                                })
                                .collect();
                            Ok((marked.is_some(), changes))
                        })
                    })
                    .await?;
                // Another run got there first.
                if !marked {
                    tx.rollback().await?;
                    continue;
                }
                let notice = Notice::new(format!("Personal-data request {id} is due {due}"))
                    .kind("privacy.request")
                    .body("Answer it in every store before the due date.")
                    .url(format!("{}/{id}", crate::web::REQUESTS_PATH));
                self.notifications
                    .notify(&mut tx, scope, &recipients, &notice)
                    .await?;
                tx.commit().await?;
            }
        }
        Ok(())
    }
}
