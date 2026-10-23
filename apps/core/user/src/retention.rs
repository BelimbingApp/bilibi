//! User's record classes (ADR 05, `base/privacy` retention). Closed accounts
//! are anonymised once the tenant's period has passed since they were closed:
//! the row, its ID and its links stay, the name and sign-in email do not, and
//! the sign-in links that remain are deleted. Notifications are purged once
//! the period has passed since they were sent. Both keep forever until a
//! tenant sets a period, and both skip what legal holds name: the record, or
//! the account's person. No Bilimbi counterpart.
use bilibi_authz::SystemScope;
use bilibi_database::{Change, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_privacy::{
    ErasedRecord, HandlerFuture, RecordClass, RetentionAction, Sweeper, Swept, TenantHolds, ERASED,
    RECORD_CLASSES,
};
use serde_json::json;

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

struct ClosedAccounts;
static CLOSED_ACCOUNTS: ClosedAccounts = ClosedAccounts;
impl Sweeper for ClosedAccounts {
    fn sweep<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a SystemScope,
        cutoff: &'a str,
        holds: &'a TenantHolds,
        limit: i64,
    ) -> HandlerFuture<'a, Swept> {
        Box::pin(async move {
            let tenant = scope.tenant();
            let (held_users, held_persons) = (holds.records_of(USER_KIND), holds.persons());
            let past = "tenant_id=$1 AND status='deleted' AND deleted_at < $2::text::timestamptz AND name <> $3";
            let held = "(id = ANY($4) OR coalesce(person_id = ANY($5), false))";
            let kept: i64 = tx
                .sql()
                .query_one(
                    &format!("SELECT count(*) FROM users WHERE {past} AND {held}"),
                    &[&tenant, &cutoff, &ERASED, &held_users, &held_persons],
                )
                .await?
                .get(0);
            let rows = tx
                .sql()
                .query(
                    &format!("SELECT id,name,email FROM users WHERE {past} AND NOT {held} ORDER BY id LIMIT $6 FOR UPDATE"),
                    &[&tenant, &cutoff, &ERASED, &held_users, &held_persons, &limit],
                )
                .await?;
            // A write must return changes: nothing to act on is not a write.
            if rows.is_empty() {
                return Ok(Swept {
                    kept,
                    ..Swept::default()
                });
            }
            let ids: Vec<i64> = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let mut ids = Vec::with_capacity(rows.len());
                        let mut changes = Vec::with_capacity(rows.len());
                        for row in rows {
                            let (user, name, email): (i64, String, String) =
                                (row.get(0), row.get(1), row.get(2));
                            let placeholder = format!("erased-{user}@erased.invalid");
                            sql.execute(
                                "UPDATE users SET name=$3,email=$4,password_hash='!erased',version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
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
                                new: json!({"name": ERASED, "email": placeholder, "links_removed": links}),
                            });
                            ids.push(user);
                        }
                        Ok((ids, changes))
                    })
                })
                .await?;
            Ok(Swept {
                acted: ids.len() as i64,
                kept,
                records: ids
                    .into_iter()
                    .map(|id| ErasedRecord {
                        kind: USER_KIND.into(),
                        id,
                        fields: vec!["name".into(), "email".into()],
                    })
                    .collect(),
            })
        })
    }
}

struct Notifications;
static NOTIFICATIONS: Notifications = Notifications;
impl Sweeper for Notifications {
    fn sweep<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a SystemScope,
        cutoff: &'a str,
        holds: &'a TenantHolds,
        limit: i64,
    ) -> HandlerFuture<'a, Swept> {
        Box::pin(async move {
            let tenant = scope.tenant();
            let (held_notices, held_persons) =
                (holds.records_of(NOTIFICATION_KIND), holds.persons());
            let held = "(n.id = ANY($3) OR coalesce(u.person_id = ANY($4), false))";
            let from =
                "FROM notifications n JOIN users u ON u.tenant_id=n.tenant_id AND u.id=n.user_id \
                        WHERE n.tenant_id=$1 AND n.created_at < $2::text::timestamptz";
            let kept: i64 = tx
                .sql()
                .query_one(
                    &format!("SELECT count(*) {from} AND {held}"),
                    &[&tenant, &cutoff, &held_notices, &held_persons],
                )
                .await?
                .get(0);
            let due: Vec<i64> = tx
                .sql()
                .query(
                    &format!(
                        "SELECT n.id {from} AND NOT {held} ORDER BY n.id LIMIT $5 FOR UPDATE OF n"
                    ),
                    &[&tenant, &cutoff, &held_notices, &held_persons, &limit],
                )
                .await?
                .iter()
                .map(|row| row.get(0))
                .collect();
            // A write must return changes: nothing to act on is not a write.
            if due.is_empty() {
                return Ok(Swept {
                    kept,
                    ..Swept::default()
                });
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
            Ok(Swept {
                acted: ids.len() as i64,
                kept,
                records: ids
                    .into_iter()
                    .map(|id| ErasedRecord {
                        kind: NOTIFICATION_KIND.into(),
                        id,
                        fields: vec!["title".into(), "body".into()],
                    })
                    .collect(),
            })
        })
    }
}

inventory::submit! { Contribution::new(RECORD_CLASSES, "core/user.closed_accounts",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordClass {
    label: "Closed user accounts",
    clock_start: "When the account was closed",
    action: RetentionAction::Anonymise,
    default_days: None,
    minimum_days: None,
    sweeper: &CLOSED_ACCOUNTS,
}) }
inventory::submit! { Contribution::new(RECORD_CLASSES, "core/user.notifications",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordClass {
    label: "Notifications",
    clock_start: "When the notification was sent",
    action: RetentionAction::Purge,
    default_days: None,
    minimum_days: None,
    sweeper: &NOTIFICATIONS,
}) }
