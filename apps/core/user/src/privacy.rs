//! User's personal-data store (ADR 05, `base/personal_data`): the accounts linked to
//! the person and their notifications. Erasure closes each account the way
//! `delete_account` does (sessions ended, step-up factors removed, reset
//! tokens dropped) and anonymises it: the row, its ID and its links stay, the
//! name and sign-in email do not. Single sign-on links, pending sign-ins and
//! failed-attempt counters have no business value and are deleted, as are
//! notifications, each unless a legal hold names it. Accounts that only share
//! an email are counted, never touched. No Bilimbi counterpart.
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_personal_data::{
    Action, Disclosure, Erased, ErasedRecord, Erasure, Exported, Handler, HandlerFuture, Holds,
    Identifier, Kept, KindCount, Located, PersonalData, RecordKind, Subject, ERASED, PERSONAL_DATA,
    RECORD_KINDS,
};
use bilibi_security::Sessions;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};
use tokio_postgres::Client;

const USER_KIND: &str = "user";
const NOTIFICATION_KIND: &str = "notification";

struct Bound {
    sessions: Arc<Sessions>,
    step_up: Arc<bilibi_step_up::StepUp>,
}
static BOUND: OnceLock<Bound> = OnceLock::new();
/// Bound once by the User service factory.
pub(crate) fn bind(
    sessions: Arc<Sessions>,
    step_up: Arc<bilibi_step_up::StepUp>,
) -> bilibi_module_registry::Result<()> {
    BOUND.set(Bound { sessions, step_up }).map_err(|_| {
        bilibi_module_registry::RegistryError::Service("duplicate user privacy binding".into())
    })
}

fn emails(subject: &Subject) -> Vec<String> {
    subject
        .identifiers
        .iter()
        .filter_map(|identifier| match identifier {
            Identifier::Email(value) => Some(value.to_lowercase()),
            Identifier::Phone(_) => None,
        })
        .collect()
}

struct Accounts;
static ACCOUNTS: Accounts = Accounts;
impl Handler for Accounts {
    fn locate<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Located> {
        Box::pin(async move {
            let row = client
                .query_one(
                    "SELECT count(*) FILTER (WHERE person_id=$2), count(*) FILTER (WHERE person_id IS DISTINCT FROM $2 AND lower(email)=ANY($3)), \
                     (SELECT count(*) FROM notifications n JOIN users u ON u.tenant_id=n.tenant_id AND u.id=n.user_id WHERE n.tenant_id=$1 AND u.person_id=$2) \
                     FROM users WHERE tenant_id=$1",
                    &[&scope.tenant(), &subject.person.get(), &emails(subject)],
                )
                .await?;
            Ok(Located {
                kinds: vec![
                    KindCount {
                        kind: USER_KIND.into(),
                        by_person: row.get(0),
                        by_identifier: row.get(1),
                    },
                    KindCount {
                        kind: NOTIFICATION_KIND.into(),
                        by_person: row.get(2),
                        by_identifier: 0,
                    },
                ],
                ..Located::default()
            })
        })
    }
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Disclosure,
    ) -> HandlerFuture<'a, Exported> {
        Box::pin(async move {
            grant.check_subject(scope, subject)?;
            let accounts = client
                .query(
                    "SELECT u.id,u.name,u.email,u.status,u.email_verified_at::text,u.deleted_at::text,\
                     coalesce((SELECT jsonb_agg(jsonb_build_object('type',n.type,'title',n.title,'body',n.body,'url',n.url,'read_at',n.read_at::text,'created_at',n.created_at::text) ORDER BY n.id) \
                       FROM notifications n WHERE n.tenant_id=u.tenant_id AND n.user_id=u.id),'[]'::jsonb) \
                     FROM users u WHERE u.tenant_id=$1 AND u.person_id=$2 ORDER BY u.id",
                    &[&scope.tenant(), &subject.person.get()],
                )
                .await?;
            let records: Vec<Value> = accounts
                .iter()
                .map(|row| {
                    json!({
                        "id": row.get::<_, i64>(0), "name": row.get::<_, String>(1), "email": row.get::<_, String>(2),
                        "status": row.get::<_, String>(3), "email_verified_at": row.get::<_, Option<String>>(4),
                        "deleted_at": row.get::<_, Option<String>>(5), "notifications": row.get::<_, Value>(6),
                    })
                })
                .collect();
            Ok(Exported {
                records: json!({"accounts": records}),
                files: Vec::new(),
            })
        })
    }
    fn erase<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        subject: &'a Subject,
        grant: &'a Erasure,
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            grant.check_subject(scope, subject)?;
            let bound = BOUND.get().ok_or(Error::Internal)?;
            let tenant = scope.tenant();
            let users = tx
                .sql()
                .query(
                    "SELECT id,name,email,status,(name=$3 AND email LIKE 'erased-%@erased.invalid' AND status='deleted') FROM users WHERE tenant_id=$1 AND person_id=$2 ORDER BY id FOR UPDATE",
                    &[&tenant, &subject.person.get(), &ERASED],
                )
                .await?;
            let ids: Vec<i64> = users.iter().map(|row| row.get(0)).collect();
            let held_users = holds.records(USER_KIND, &ids).await?;
            let mut erased = Erased::default();
            for row in users {
                let (user, name, email, status, done): (i64, String, String, String, bool) =
                    (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
                if let Some(hold) = held_users.get(&user) {
                    erased.kept.push(Kept::held(USER_KIND, user, *hold));
                    continue;
                }
                if user == scope.user() {
                    // The approver's own account cannot be the one erased.
                    return Err(Error::Forbidden);
                }
                // Each notification on its own: a held one stays.
                let notices: Vec<i64> = tx
                    .sql()
                    .query(
                        "SELECT id FROM notifications WHERE tenant_id=$1 AND user_id=$2 ORDER BY id FOR UPDATE",
                        &[&tenant, &user],
                    )
                    .await?
                    .iter()
                    .map(|row| row.get(0))
                    .collect();
                let held_notices = holds.records(NOTIFICATION_KIND, &notices).await?;
                for (notice, hold) in &held_notices {
                    erased
                        .kept
                        .push(Kept::held(NOTIFICATION_KIND, notice, *hold));
                }
                let gone: Vec<i64> = notices
                    .into_iter()
                    .filter(|id| !held_notices.contains_key(id))
                    .collect();
                let before = json!({"name": name, "email": email, "status": status});
                // Already anonymised with nothing to delete: not a write.
                let removed = if done && gone.is_empty() {
                    Vec::new()
                } else {
                    tx.mutate(move |sql| {
                        Box::pin(async move {
                            let placeholder = format!("erased-{user}@erased.invalid");
                            let mut changes = Vec::new();
                            if !done {
                                sql.execute(
                                    "UPDATE users SET name=$3,email=$4,password_hash='!erased',status='deleted',deleted_at=coalesce(deleted_at,now()),\
                                     anonymised_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                                    &[&tenant, &user, &ERASED, &placeholder],
                                )
                                .await?;
                                let links = remove_links(sql, tenant, user).await?;
                                changes.push(Change {
                                    kind: USER_KIND.into(),
                                    id: user,
                                    old: before,
                                    new: json!({"name": ERASED, "email": ERASED, "status": "deleted", "links_removed": links.len()}),
                                });
                                changes.extend(links);
                            }
                            let rows = sql
                                .query("DELETE FROM notifications WHERE tenant_id=$1 AND id=ANY($2) RETURNING id", &[&tenant, &gone])
                                .await?;
                            let ids: Vec<i64> = rows.iter().map(|row| row.get(0)).collect();
                            changes.extend(ids.iter().map(|id| Change {
                                kind: NOTIFICATION_KIND.into(),
                                id: *id,
                                old: json!({"recipient": user}),
                                new: json!({}),
                            }));
                            Ok((ids, changes))
                        })
                    })
                    .await?
                };
                if !done {
                    bound
                        .step_up
                        .lifecycle(tx, user, bilibi_step_up::Lifecycle::Delete)
                        .await?;
                    let ended = bound.sessions.revoke(tx, user, None).await?;
                    let sessions = bound.sessions.clone();
                    tx.after_commit(move || sessions.announce(ended));
                }
                erased.count(USER_KIND, Action::Anonymised, 1);
                erased.count(NOTIFICATION_KIND, Action::Deleted, removed.len() as i64);
                // Only records this transaction changed: the database clears
                // evidence of nothing else.
                if !done {
                    erased.records.push(ErasedRecord {
                        kind: USER_KIND.into(),
                        id: user,
                        person: None,
                    });
                }
                erased
                    .records
                    .extend(removed.into_iter().map(|id| ErasedRecord {
                        kind: NOTIFICATION_KIND.into(),
                        id,
                        person: None,
                    }));
            }
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "core/user.accounts",
Provenance { module: "core/user", source: file!(), line: line!() },
&PersonalData { label: "User accounts and notifications", kinds: &[USER_KIND, NOTIFICATION_KIND], person_subjects: &[], handler: &ACCOUNTS }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.user",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: USER_KIND, fields: &["name", "email"], subject_identifier: false, person: false }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.notification",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: NOTIFICATION_KIND, fields: &["title", "body"], subject_identifier: false, person: false }) }

/// Deletes an account's sign-in and credential rows, one `Change` per row
/// with its key and owner only: never a token, hash or external subject.
pub(crate) async fn remove_links(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    user: i64,
) -> bilibi_database::Result<Vec<Change>> {
    let mut changes = Vec::new();
    let removed = |kind: &str, id: i64, old: Value| Change {
        kind: kind.into(),
        id,
        old,
        new: json!({}),
    };
    for row in sql
        .query(
            "DELETE FROM password_reset_tokens WHERE tenant_id=$1 AND user_id=$2 RETURNING user_id",
            &[&tenant, &user],
        )
        .await?
    {
        changes.push(removed(
            "password_reset_token",
            row.get(0),
            json!({"user": user}),
        ));
    }
    for row in sql
        .query(
            "DELETE FROM user_external_sessions WHERE tenant_id=$1 AND user_id=$2 RETURNING id",
            &[&tenant, &user],
        )
        .await?
    {
        changes.push(removed(
            "user_external_session",
            row.get(0),
            json!({"user": user}),
        ));
    }
    for row in sql
        .query("DELETE FROM user_external_identities WHERE tenant_id=$1 AND user_id=$2 RETURNING id,provider_id", &[&tenant, &user])
        .await?
    {
        changes.push(removed(
            "user_external_identity",
            row.get(0),
            json!({"user": user, "provider": row.get::<_, i64>(1)}),
        ));
    }
    for row in sql
        .query(
            "DELETE FROM user_credential_attempts WHERE tenant_id=$1 AND user_id=$2 RETURNING kind",
            &[&tenant, &user],
        )
        .await?
    {
        changes.push(removed(
            "user_credential_attempt",
            user,
            json!({"user": user, "kind": row.get::<_, String>(0)}),
        ));
    }
    for row in sql
        .query(
            "DELETE FROM user_sign_ins WHERE tenant_id=$1 AND user_id=$2 RETURNING id",
            &[&tenant, &user],
        )
        .await?
    {
        changes.push(removed("user_sign_in", row.get(0), json!({"user": user})));
    }
    Ok(changes)
}
