//! The signed-in account's notifications (Bilimbi `Core.User.Notifications`):
//! persistence for the `base/notifications` boundary, the reads the bell and
//! `/notifications` render, and the read operations. Every read and write is
//! the sealed actor's own: no call names another account's notifications.
//!
//! Delivery is process-local and recipient-scoped: a committed write wakes the
//! mounted live states of exactly the accounts it touched, through
//! [`Notifications::subscribe`]. Web drops a wake addressed to anyone else.
use bilibi_database::{
    Change, Db, Error, MutationBoundary, MutationTransaction, Result as DbResult,
};
use bilibi_notifications::Notice;
use bilibi_security::{guard, Scope, Sessions};
use bilibi_ui::page::{self, Subscription};
use serde_json::json;
use std::time::SystemTime;
use tokio::sync::broadcast;
use tokio_postgres::{GenericClient, Row};

/// Bilimbi's `/notifications` filter tabs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
    All,
    Unread,
    Read,
}
impl Status {
    pub const KEYS: [&'static str; 3] = ["all", "unread", "read"];
    pub fn parse(value: &str) -> Self {
        match value {
            "unread" => Self::Unread,
            "read" => Self::Read,
            _ => Self::All,
        }
    }
    pub fn key(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Unread => "unread",
            Self::Read => "read",
        }
    }
    fn clause(self) -> &'static str {
        match self {
            Self::All => "",
            Self::Unread => " AND read_at IS NULL",
            Self::Read => " AND read_at IS NOT NULL",
        }
    }
}

/// One stored notification as its recipient sees it.
#[derive(Clone, Debug)]
pub struct Item {
    pub id: i64,
    pub kind: String,
    pub title: String,
    pub body: String,
    /// Only a link that is still a local path; stored data is not trusted.
    pub url: Option<String>,
    pub icon: Option<String>,
    pub read: bool,
    pub created_at: SystemTime,
}
const COLUMNS: &str = "id,type,title,body,url,icon,read_at IS NOT NULL,created_at";
fn item(row: &Row) -> Item {
    Item {
        id: row.get(0),
        kind: row.get(1),
        title: row.get(2),
        body: row.get(3),
        url: row
            .get::<_, Option<String>>(4)
            .filter(|url| bilibi_notifications::local_path(url)),
        icon: row.get(5),
        read: row.get(6),
        created_at: row.get(7),
    }
}

/// The in-process fan-out the live pages and the bell subscribe to.
#[derive(Clone)]
pub struct Notifications {
    tx: broadcast::Sender<page::Change>,
}
impl Default for Notifications {
    fn default() -> Self {
        let (tx, _) = broadcast::channel(128);
        Self { tx }
    }
}
impl Notifications {
    pub fn subscribe(&self) -> Subscription {
        Subscription {
            receiver: self.tx.subscribe(),
        }
    }
    /// After commit only. A send with no live subscriber is not a failure.
    pub fn changed(&self, tenant: i64, users: &[i64]) {
        for user in users {
            let _ = self.tx.send(page::Change {
                tenant,
                user: Some(*user),
            });
        }
    }
}

pub async fn unread(client: &impl GenericClient, scope: &Scope) -> DbResult<i64> {
    count(client, scope, Status::Unread).await
}

pub async fn count(client: &impl GenericClient, scope: &Scope, status: Status) -> DbResult<i64> {
    let sql = format!(
        "SELECT count(*) FROM notifications WHERE tenant_id=$1 AND user_id=$2{}",
        status.clause()
    );
    Ok(client
        .query_one(&sql, &[&scope.tenant(), &scope.user()])
        .await?
        .get(0))
}

/// Newest first, `limit` rows from `offset`.
pub async fn list(
    client: &impl GenericClient,
    scope: &Scope,
    status: Status,
    limit: i64,
    offset: i64,
) -> DbResult<Vec<Item>> {
    if !(1..=300).contains(&limit) || offset < 0 {
        return Err(Error::Invalid);
    }
    let sql = format!(
        "SELECT {COLUMNS} FROM notifications WHERE tenant_id=$1 AND user_id=$2{} ORDER BY created_at DESC,id DESC LIMIT $3 OFFSET $4",
        status.clause()
    );
    let rows = client
        .query(&sql, &[&scope.tenant(), &scope.user(), &limit, &offset])
        .await?;
    Ok(rows.iter().map(item).collect())
}

/// The sink's half of the boundary: one row per recipient, each refused
/// unless it is an active account of the transaction's own tenant.
pub(crate) async fn record(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    recipients: &[i64],
    notice: &Notice,
) -> DbResult<()> {
    record_in_tenant(tx, scope.tenant(), recipients, notice).await
}

/// [`record`] for a caller with no sealed scope, the server-side reset
/// command: the tenant is the transaction's own, and the same checks apply.
pub(crate) async fn record_in_tenant(
    tx: &mut MutationTransaction<'_>,
    tenant: i64,
    recipients: &[i64],
    notice: &Notice,
) -> DbResult<()> {
    if tx.context().tenant != tenant {
        return Err(Error::Forbidden);
    }
    let recipients = recipients.to_vec();
    let notice = notice.clone();
    tx.mutate(move |sql| {
        Box::pin(async move {
            let active: i64 = sql
                .query_one(
                    "SELECT count(*) FROM users WHERE tenant_id=$1 AND id=ANY($2) AND deleted_at IS NULL AND status='active'",
                    &[&tenant, &recipients],
                )
                .await?
                .get(0);
            if usize::try_from(active).ok() != Some(recipients.len()) {
                return Err(Error::NotFound);
            }
            let rows = sql
                .query(
                    "INSERT INTO notifications(tenant_id,user_id,type,title,body,url,icon) SELECT $1,recipient,$3,$4,$5,$6,$7 FROM unnest($2::bigint[]) recipient RETURNING id,user_id",
                    &[&tenant, &recipients, &notice.kind, &notice.title, &notice.body, &notice.url, &notice.icon],
                )
                .await?;
            let changes = rows
                .iter()
                .map(|row| Change {
                    kind: "notification".into(),
                    id: row.get(0),
                    old: json!({}),
                    new: json!({"recipient": row.get::<_, i64>(1), "type": notice.kind, "title": notice.title}),
                })
                .collect();
            Ok(((), changes))
        })
    })
    .await
}

/// Marks one of the actor's own notifications read and returns it; one
/// already read is returned unchanged with no write. Another account's
/// notification is `NotFound`, never a hint that it exists.
pub async fn mark_read(
    db: &Db,
    token: &str,
    id: i64,
    notifications: &Notifications,
    mutations: &MutationBoundary,
    sessions: &Sessions,
) -> DbResult<Item> {
    let mut client = db.get().await?;
    let scope = guard(&**client, token, sessions).await?;
    let mut tx = mutations
        .begin(
            &mut client,
            scope.mutation_context("notifications.mark_read"),
        )
        .await?;
    let scope = guard(tx.sql(), token, sessions).await?;
    let (tenant, user) = (scope.tenant(), scope.user());
    let row = tx
        .sql()
        .query_opt(
            &format!("SELECT {COLUMNS} FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND id=$3 FOR UPDATE"),
            &[&tenant, &user, &id],
        )
        .await?
        .ok_or(Error::NotFound)?;
    let found = item(&row);
    if found.read {
        tx.rollback().await?;
        return Ok(found);
    }
    tx.mutate(move |sql| {
        Box::pin(async move {
            sql.execute(
                "UPDATE notifications SET read_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=$3",
                &[&tenant, &user, &id],
            )
            .await?;
            Ok((
                (),
                vec![Change {
                    kind: "notification".into(),
                    id,
                    old: json!({"read": false}),
                    new: json!({"read": true}),
                }],
            ))
        })
    })
    .await?;
    let notifications = notifications.clone();
    tx.after_commit(move || notifications.changed(tenant, &[user]));
    tx.commit().await?;
    Ok(Item {
        read: true,
        ..found
    })
}

/// Marks every unread notification of the actor read in one audited bulk
/// write, and returns how many changed.
pub async fn mark_all_read(
    db: &Db,
    token: &str,
    notifications: &Notifications,
    mutations: &MutationBoundary,
    sessions: &Sessions,
) -> DbResult<u64> {
    let mut client = db.get().await?;
    let scope = guard(&**client, token, sessions).await?;
    let mut tx = mutations
        .begin(
            &mut client,
            scope.mutation_context("notifications.mark_all_read"),
        )
        .await?;
    let scope = guard(tx.sql(), token, sessions).await?;
    let (tenant, user) = (scope.tenant(), scope.user());
    // Lock the rows first so the bulk write changes exactly what was seen.
    let ids: Vec<i64> = tx
        .sql()
        .query(
            "SELECT id FROM notifications WHERE tenant_id=$1 AND user_id=$2 AND read_at IS NULL FOR UPDATE",
            &[&tenant, &user],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect();
    if ids.is_empty() {
        tx.rollback().await?;
        return Ok(0);
    }
    let changed = tx
        .mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE notifications SET read_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=ANY($3)",
                    &[&tenant, &user, &ids],
                )
                .await?;
                let changes: Vec<_> = ids
                    .iter()
                    .map(|id| Change {
                        kind: "notification".into(),
                        id: *id,
                        old: json!({"read": false}),
                        new: json!({"read": true}),
                    })
                    .collect();
                Ok((changes.len() as u64, changes))
            })
        })
        .await?;
    let notifications = notifications.clone();
    tx.after_commit(move || notifications.changed(tenant, &[user]));
    tx.commit().await?;
    Ok(changed)
}

impl bilibi_notifications::NotificationSink for Notifications {
    fn record<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        recipients: &'a [i64],
        notice: &'a Notice,
    ) -> bilibi_notifications::RecordFuture<'a> {
        Box::pin(record(tx, scope, recipients, notice))
    }
    fn committed(&self, tenant: i64, recipients: &[i64]) {
        self.changed(tenant, recipients);
    }
}
