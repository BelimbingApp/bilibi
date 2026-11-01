//! Whether an invitation's email arrived (issue 137, behaviour 13). A relay
//! that refuses the message, or a bounce an Extension adapting a mail provider
//! reports through `base/mail`'s delivery reports, marks the open invitation
//! `undelivered` or `bounced` and tells the inviter once, in the same
//! transaction. The invitation stays open: staff revoke it or invite again,
//! to the same address or to another. Bilimbi has no customer portal.
use bilibi_database::{Change, Context, Db, MutationBoundary, Result};
use bilibi_mail::{DeliveryReport, ObserveFuture};
use bilibi_notifications::{Notice, NotificationBoundary};
use bilibi_tenancy::Authentication;
use serde_json::json;
use std::sync::{Arc, OnceLock};

/// The kind of the `X-Bilibi-Reference` this module tags its messages with.
pub(crate) const REFERENCE_KIND: &str = "customer_access.invitation";

/// How the message failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Failure {
    /// The relay refused it when it was sent.
    Undelivered,
    /// A mail adapter reported a bounce.
    Bounced,
}
impl Failure {
    fn column(self) -> &'static str {
        match self {
            Self::Undelivered => "undelivered",
            Self::Bounced => "bounced",
        }
    }
}

/// What recording a failure needs, cheap to clone into the task that sends.
#[derive(Clone)]
pub(crate) struct Desk {
    pub db: Db,
    pub mutations: Arc<MutationBoundary>,
    pub authentication: Arc<Authentication>,
    pub notifications: Arc<NotificationBoundary>,
}

static BOUND: OnceLock<Desk> = OnceLock::new();
pub(crate) fn bind(desk: Desk) -> bilibi_module_registry::Result<()> {
    BOUND.set(desk).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "duplicate customer access delivery binding".into(),
        )
    })
}

impl Desk {
    /// Marks the open invitation `id` and notifies its inviter, once. `address`
    /// (when a report names one) must be the invitation's own, so a late
    /// report about an address the invitation no longer holds changes
    /// nothing. `Ok(false)`: no such open invitation, or already marked.
    pub(crate) async fn record(
        &self,
        id: i64,
        address: Option<&str>,
        failure: Failure,
    ) -> Result<bool> {
        let address = address.map(str::to_lowercase);
        let (tenant, inviter, email, person) = {
            let client = self.db.get().await?;
            let Some(row) = client
                .query_opt(
                    "SELECT tenant_id,invited_by,email,person_id FROM customer_invitations WHERE id=$1 AND accepted_at IS NULL AND revoked_at IS NULL AND delivery IS NULL AND ($2::text IS NULL OR email=$2)",
                    &[&id, &address],
                )
                .await?
            else {
                return Ok(false);
            };
            (
                row.get::<_, i64>(0),
                row.get::<_, i64>(1),
                row.get::<_, String>(2),
                row.get::<_, i64>(3),
            )
        };
        // The inviter's own scope names the tenant for the notification; an
        // inviter who is gone is simply not told.
        let scope = {
            let client = self.db.get().await?;
            self.authentication
                .authenticate(&**client, tenant, inviter, String::new())
                .await
                .ok()
        };
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(
                    tenant,
                    "customer_access",
                    "customer_access.invitation.delivery",
                ),
            )
            .await?;
        let column = failure.column();
        let marked = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let rows = sql
                        .query(
                            "UPDATE customer_invitations SET delivery=$3,delivery_at=now() WHERE tenant_id=$1 AND id=$2 AND accepted_at IS NULL AND revoked_at IS NULL AND delivery IS NULL RETURNING id",
                            &[&tenant, &id, &column],
                        )
                        .await?;
                    let changes = rows
                        .iter()
                        .map(|row| Change {
                            kind: "customer_access.invitation".into(),
                            id: row.get(0),
                            old: json!({"status": "pending"}),
                            new: json!({"status": column}),
                        })
                        .collect::<Vec<_>>();
                    Ok((!changes.is_empty(), changes))
                })
            })
            .await?;
        if !marked {
            tx.rollback().await?;
            return Ok(false);
        }
        if let Some(scope) = scope {
            let body = match failure {
                Failure::Undelivered => format!(
                    "The mail server refused the invitation to {email}. Check the address, then invite again."
                ),
                Failure::Bounced => format!(
                    "The invitation to {email} bounced. Check the address, then invite again."
                ),
            };
            self.notifications
                .notify(
                    &mut tx,
                    &scope,
                    &[inviter],
                    &Notice::new(format!("Invitation to {email} was not delivered"))
                        .kind("customer_access.invitation_failed")
                        .body(body)
                        .url(format!("/contacts/{person}"))
                        .icon("envelope"),
                )
                .await?;
        }
        tx.commit().await?;
        Ok(true)
    }
}

/// The observer `base/mail` hands a bounce report to.
pub(crate) fn observe<'a>(record: &'a str, report: &'a DeliveryReport) -> ObserveFuture<'a> {
    Box::pin(async move {
        let Ok(id) = record.parse::<i64>() else {
            return Ok(false);
        };
        let Some(desk) = BOUND.get() else {
            return Ok(false);
        };
        desk.record(id, Some(&report.address), Failure::Bounced)
            .await
            .map_err(|error| bilibi_mail::Error::Report(error.to_string()))
    })
}
