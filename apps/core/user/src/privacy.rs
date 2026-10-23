//! User's personal-data store (ADR 05, `base/privacy`): the accounts linked to
//! the person and their notifications. Erasure closes each account the way
//! `delete_account` does (sessions ended, step-up factors removed, reset
//! tokens dropped) and anonymises it: the row, its ID and its links stay, the
//! name and sign-in email do not. Single sign-on links, pending sign-ins and
//! failed-attempt counters have no business value and are deleted, as are
//! notifications. Accounts that only share an email are counted, never
//! touched. No Bilimbi counterpart.
use bilibi_database::{Change, Error, MutationTransaction};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_privacy::{
    Action, Erased, ErasedRecord, Exported, Handler, HandlerFuture, Holds, Identifier, Kept,
    KeptReason, KindAction, KindCount, Located, PersonalData, Subject, ERASED, PERSONAL_DATA,
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
            })
        })
    }
    fn export<'a>(
        &'a self,
        client: &'a Client,
        scope: &'a Scope,
        subject: &'a Subject,
    ) -> HandlerFuture<'a, Exported> {
        Box::pin(async move {
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
        holds: &'a Holds,
    ) -> HandlerFuture<'a, Erased> {
        Box::pin(async move {
            let bound = BOUND.get().ok_or(Error::Internal)?;
            let tenant = scope.tenant();
            let users = tx
                .sql()
                .query(
                    "SELECT id,name,email,status FROM users WHERE tenant_id=$1 AND person_id=$2 ORDER BY id FOR UPDATE",
                    &[&tenant, &subject.person.get()],
                )
                .await?;
            let mut erased = Erased::default();
            let (mut accounts, mut notifications) = (0, 0);
            for row in users {
                let (user, name, email, status): (i64, String, String, String) =
                    (row.get(0), row.get(1), row.get(2), row.get(3));
                if holds.record_held(USER_KIND, user) {
                    erased.kept.push(Kept {
                        kind: USER_KIND.into(),
                        id: user.to_string(),
                        reason: KeptReason::LegalHold,
                        until: None,
                    });
                    continue;
                }
                if user == scope.user() {
                    // The approver's own account cannot be the one erased.
                    return Err(Error::Forbidden);
                }
                let before = json!({"name": name, "email": email, "status": status});
                let removed = tx
                    .mutate(move |sql| {
                        Box::pin(async move {
                            let placeholder = format!("erased-{user}@erased.invalid");
                            sql.execute(
                                "UPDATE users SET name=$3,email=$4,password_hash='!erased',status='deleted',deleted_at=coalesce(deleted_at,now()),\
                                 version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                                &[&tenant, &user, &ERASED, &placeholder],
                            )
                            .await?;
                            let mut links = 0;
                            for table in [
                                "password_reset_tokens",
                                "user_external_sessions",
                                "user_external_identities",
                                "user_credential_attempts",
                                "user_sign_ins",
                            ] {
                                links += sql
                                    .execute(&format!("DELETE FROM {table} WHERE tenant_id=$1 AND user_id=$2"), &[&tenant, &user])
                                    .await?;
                            }
                            let gone = sql
                                .query("DELETE FROM notifications WHERE tenant_id=$1 AND user_id=$2 RETURNING id", &[&tenant, &user])
                                .await?;
                            let mut changes = vec![Change {
                                kind: USER_KIND.into(),
                                id: user,
                                old: before,
                                new: json!({"name": ERASED, "email": placeholder, "status": "deleted", "links_removed": links}),
                            }];
                            let ids: Vec<i64> = gone.iter().map(|row| row.get(0)).collect();
                            changes.extend(ids.iter().map(|id| Change {
                                kind: NOTIFICATION_KIND.into(),
                                id: *id,
                                old: json!({"recipient": user}),
                                new: json!({}),
                            }));
                            Ok((ids, changes))
                        })
                    })
                    .await?;
                bound
                    .step_up
                    .lifecycle(tx, user, bilibi_step_up::Lifecycle::Delete)
                    .await?;
                let ended = bound.sessions.revoke(tx, user, None).await?;
                let sessions = bound.sessions.clone();
                tx.after_commit(move || sessions.announce(ended));
                accounts += 1;
                notifications += removed.len() as i64;
                erased.records.push(ErasedRecord {
                    kind: USER_KIND.into(),
                    id: user,
                    fields: vec!["name".into(), "email".into()],
                });
                erased
                    .records
                    .extend(removed.into_iter().map(|id| ErasedRecord {
                        kind: NOTIFICATION_KIND.into(),
                        id,
                        fields: vec!["title".into(), "body".into()],
                    }));
            }
            if accounts > 0 {
                erased.actions.push(KindAction {
                    kind: USER_KIND.into(),
                    action: Action::Anonymised,
                    count: accounts,
                });
            }
            if notifications > 0 {
                erased.actions.push(KindAction {
                    kind: NOTIFICATION_KIND.into(),
                    action: Action::Deleted,
                    count: notifications,
                });
            }
            Ok(erased)
        })
    }
}

inventory::submit! { Contribution::new(PERSONAL_DATA, "core/user.accounts",
Provenance { module: "core/user", source: file!(), line: line!() },
&PersonalData { label: "User accounts and notifications", record_kinds: &[USER_KIND, NOTIFICATION_KIND], handler: &ACCOUNTS }) }
