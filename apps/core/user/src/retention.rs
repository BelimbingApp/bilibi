//! User's record classes (ADR 05, `base/privacy` retention). Closed accounts
//! are anonymised once the company's period has passed since they were
//! closed: the row, its ID and its links stay, the name and sign-in email do
//! not, and the sign-in links that remain are deleted. Notifications, which
//! have no business value, are purged once the period has passed since they
//! were sent. Neither is swept until a company turns it on and sets a period,
//! and both skip what legal holds name: the record, or the account's person.
//! No Bilimbi counterpart.
use bilibi_database::{Change, MutationTransaction};
use bilibi_personal_data::{ErasedRecord, Erasure, HandlerFuture, Holds, ERASED};
use bilibi_privacy::{Sweeper, Swept};
use bilibi_tenancy::Scope;
use serde_json::json;
use std::collections::BTreeMap;

const USER_KIND: &str = "user";
const NOTIFICATION_KIND: &str = "notification";
/// Sign-in links an anonymised account no longer needs.
const LINKS: [&str; 5] = [
    "password_reset_tokens",
    "user_external_sessions",
    "user_external_identities",
    "user_credential_attempts",
    "user_sign_ins",
];

/// Which of `rows` (record ID, its person) a hold keeps.
async fn held(
    holds: &Holds,
    kind: &str,
    rows: &[(i64, Option<i64>)],
) -> bilibi_database::Result<Vec<i64>> {
    let ids: Vec<i64> = rows.iter().map(|(id, _)| *id).collect();
    let persons: Vec<i64> = rows.iter().filter_map(|(_, person)| *person).collect();
    let records = holds.records(kind, &ids).await?;
    let people: BTreeMap<i64, i64> = holds.persons(&persons).await?;
    Ok(rows
        .iter()
        .filter(|(id, person)| {
            records.contains_key(id) || person.is_some_and(|p| people.contains_key(&p))
        })
        .map(|(id, _)| *id)
        .collect())
}
fn next(rows: &[(i64, Option<i64>)], limit: i64) -> Option<i64> {
    (rows.len() as i64 == limit)
        .then(|| rows.last().map(|(id, _)| *id))
        .flatten()
}

struct ClosedAccounts;
static CLOSED_ACCOUNTS: ClosedAccounts = ClosedAccounts;
impl Sweeper for ClosedAccounts {
    fn sweep<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        grant: &'a Erasure,
        cutoff: &'a str,
        holds: &'a Holds,
        after: i64,
        limit: i64,
    ) -> HandlerFuture<'a, Swept> {
        Box::pin(async move {
            grant.check(scope)?;
            let tenant = scope.tenant();
            let rows = tx
                .sql()
                .query(
                    "SELECT id,person_id,name,email FROM users WHERE tenant_id=$1 AND status='deleted' AND anonymised_at IS NULL \
                     AND deleted_at < $2::text::timestamptz AND id>$3 ORDER BY id LIMIT $4 FOR UPDATE",
                    &[&tenant, &cutoff, &after, &limit],
                )
                .await?;
            let keys: Vec<(i64, Option<i64>)> =
                rows.iter().map(|row| (row.get(0), row.get(1))).collect();
            let kept = held(holds, USER_KIND, &keys).await?;
            let due: Vec<(i64, String, String)> = rows
                .iter()
                .filter(|row| !kept.contains(&row.get::<_, i64>(0)))
                .map(|row| (row.get(0), row.get(2), row.get(3)))
                .collect();
            let mut swept = Swept {
                kept: kept.len() as i64,
                next: next(&keys, limit),
                ..Swept::default()
            };
            // A write must return changes: nothing to act on is not a write.
            if due.is_empty() {
                return Ok(swept);
            }
            let ids: Vec<i64> = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let mut ids = Vec::with_capacity(due.len());
                        let mut changes = Vec::with_capacity(due.len());
                        for (user, name, email) in due {
                            let placeholder = format!("erased-{user}@erased.invalid");
                            sql.execute(
                                "UPDATE users SET name=$3,email=$4,password_hash='!erased',anonymised_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                                &[&tenant, &user, &ERASED, &placeholder],
                            )
                            .await?;
                            let mut links = 0;
                            for table in LINKS {
                                links += sql
                                    .execute(&format!("DELETE FROM {table} WHERE tenant_id=$1 AND user_id=$2"), &[&tenant, &user])
                                    .await?;
                            }
                            changes.push(Change {
                                kind: USER_KIND.into(),
                                id: user,
                                old: json!({"name": name, "email": email}),
                                new: json!({"name": ERASED, "email": ERASED, "links_removed": links}),
                            });
                            ids.push(user);
                        }
                        Ok((ids, changes))
                    })
                })
                .await?;
            swept.acted = ids.len() as i64;
            swept.records = ids
                .into_iter()
                .map(|id| ErasedRecord {
                    kind: USER_KIND.into(),
                    id,
                })
                .collect();
            Ok(swept)
        })
    }
}

struct Notifications;
static NOTIFICATIONS: Notifications = Notifications;
impl Sweeper for Notifications {
    fn sweep<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        grant: &'a Erasure,
        cutoff: &'a str,
        holds: &'a Holds,
        after: i64,
        limit: i64,
    ) -> HandlerFuture<'a, Swept> {
        Box::pin(async move {
            grant.check(scope)?;
            let tenant = scope.tenant();
            let rows = tx
                .sql()
                .query(
                    "SELECT n.id,u.person_id FROM notifications n JOIN users u ON u.tenant_id=n.tenant_id AND u.id=n.user_id \
                     WHERE n.tenant_id=$1 AND n.created_at < $2::text::timestamptz AND n.id>$3 ORDER BY n.id LIMIT $4 FOR UPDATE OF n",
                    &[&tenant, &cutoff, &after, &limit],
                )
                .await?;
            let keys: Vec<(i64, Option<i64>)> =
                rows.iter().map(|row| (row.get(0), row.get(1))).collect();
            let kept = held(holds, NOTIFICATION_KIND, &keys).await?;
            let due: Vec<i64> = keys
                .iter()
                .map(|(id, _)| *id)
                .filter(|id| !kept.contains(id))
                .collect();
            let mut swept = Swept {
                kept: kept.len() as i64,
                next: next(&keys, limit),
                ..Swept::default()
            };
            // A write must return changes: nothing to act on is not a write.
            if due.is_empty() {
                return Ok(swept);
            }
            let ids: Vec<i64> = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let rows = sql
                            .query(
                                "DELETE FROM notifications WHERE tenant_id=$1 AND id = ANY($2) RETURNING id,user_id",
                                &[&tenant, &due],
                            )
                            .await?;
                        let changes = rows
                            .iter()
                            .map(|row| Change {
                                kind: NOTIFICATION_KIND.into(),
                                id: row.get(0),
                                old: json!({"recipient": row.get::<_, i64>(1)}),
                                new: json!({}),
                            })
                            .collect();
                        Ok((rows.iter().map(|row| row.get(0)).collect(), changes))
                    })
                })
                .await?;
            swept.acted = ids.len() as i64;
            swept.records = ids
                .into_iter()
                .map(|id| ErasedRecord {
                    kind: NOTIFICATION_KIND.into(),
                    id,
                })
                .collect();
            Ok(swept)
        })
    }
}

bilibi_privacy::record_class! {
    module: "core/user", key: "core/user.closed_accounts", label: "Closed user accounts",
    clock_start: "when the account was closed", action: Anonymise,
    business_value: true, floor_days: None, sweeper: &CLOSED_ACCOUNTS,
}
bilibi_privacy::record_class! {
    module: "core/user", key: "core/user.notifications", label: "Notifications",
    clock_start: "when the notification was sent", action: Purge,
    business_value: false, floor_days: None, sweeper: &NOTIFICATIONS,
}
