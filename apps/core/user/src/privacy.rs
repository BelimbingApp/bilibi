//! User's personal-data store (ADR 05, `base/personal_data`): the accounts linked to
//! the person and their notifications. Erasure closes each account the way
//! `delete_account` does (sessions ended, step-up factors removed, reset
//! tokens dropped) and anonymises it: the row, its ID and its links stay, the
//! name and sign-in email do not. Single sign-on links, pending sign-ins and
//! failed-attempt counters have no business value and are deleted, as are
//! notifications, each unless a legal hold names it. The account's SCIM
//! document keeps only its resource ID, and a first administrator's receipt
//! its placeholder email. Accounts that only share an email are counted, never
//! touched. No Bilimbi counterpart.
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
/// The account's SCIM document, as SCIM provisioning records it.
const SCIM_KIND: &str = "user.scim_user";
/// A single sign-on link, as SSO records it.
pub(crate) const EXTERNAL_IDENTITY_KIND: &str = "user.external_identity";
/// The first administrator's receipt, as the bootstrap records it.
const BOOTSTRAP_KIND: &str = "user.bootstrap";
/// An agent's two-factor reset, which once recorded the account's name and
/// email.
const TWO_FACTOR_RESET_KIND: &str = "user_two_factor_reset";
/// Every top-level attribute of a SCIM document: an identity provider may
/// send any attribute or vendor extension, all of them about the person.
const SCIM_FIELDS: [&str; 1] = ["*"];

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
            let held_users = holds.records(grant, USER_KIND, &ids).await?;
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
                let held_notices = holds.records(grant, NOTIFICATION_KIND, &notices).await?;
                for (notice, hold) in &held_notices {
                    erased
                        .kept
                        .push(Kept::held(NOTIFICATION_KIND, notice, *hold));
                }
                let gone: Vec<i64> = notices
                    .into_iter()
                    .filter(|id| !held_notices.contains_key(id))
                    .collect();
                let placeholder = format!("erased-{user}@erased.invalid");
                // The account's other records, each kept while a hold names
                // it, and written only while it, or its history, still holds
                // a value: a repeat erasure of a clean account writes nothing.
                let scim_rows: Vec<(i64, bool)> = tx
                    .sql()
                    .query(
                        "SELECT id,document->>'userName'=$3 FROM user_scim_users WHERE tenant_id=$1 AND user_id=$2 ORDER BY id FOR UPDATE",
                        &[&tenant, &user, &placeholder],
                    )
                    .await?
                    .iter()
                    .map(|row| (row.get(0), row.get(1)))
                    .collect();
                let held_scim = holds
                    .records(
                        grant,
                        SCIM_KIND,
                        &scim_rows.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
                    )
                    .await?;
                let mut scim = Vec::new();
                for (id, clean) in scim_rows {
                    if let Some(hold) = held_scim.get(&id) {
                        erased.kept.push(Kept::held(SCIM_KIND, id, *hold));
                    } else if !clean || bilibi_audit::holds_values(tx, SCIM_KIND, id).await? {
                        scim.push(id);
                    }
                }
                let identities: Vec<i64> = tx
                    .sql()
                    .query(
                        "SELECT id FROM user_external_identities WHERE tenant_id=$1 AND user_id=$2 ORDER BY id FOR UPDATE",
                        &[&tenant, &user],
                    )
                    .await?
                    .iter()
                    .map(|row| row.get(0))
                    .collect();
                let held_identities = holds
                    .records(grant, EXTERNAL_IDENTITY_KIND, &identities)
                    .await?;
                // Ending an account removes every sign-in link it has (the
                // sign-in factor's lifecycle), so a hold on one keeps the
                // whole account, as a hold on the account does.
                if let Some(hold) = held_identities.values().next() {
                    for (identity, hold) in &held_identities {
                        erased
                            .kept
                            .push(Kept::held(EXTERNAL_IDENTITY_KIND, identity, *hold));
                    }
                    erased.kept.push(Kept::held(USER_KIND, user, *hold));
                    continue;
                }
                let receipt = tx
                    .sql()
                    .query_opt(
                        "SELECT admin_email=$3 FROM user_bootstrap WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",
                        &[&tenant, &user, &placeholder],
                    )
                    .await?
                    .map(|row| row.get::<_, bool>(0));
                let bootstrap = match receipt {
                    None => false,
                    Some(clean) => {
                        match holds.records(grant, BOOTSTRAP_KIND, &[1]).await?.get(&1) {
                            Some(hold) => {
                                erased.kept.push(Kept::held(BOOTSTRAP_KIND, 1, *hold));
                                false
                            }
                            None => {
                                !clean || bilibi_audit::holds_values(tx, BOOTSTRAP_KIND, 1).await?
                            }
                        }
                    }
                };
                // Earlier resets recorded the name and email under their own
                // kind: an erasure change of that kind lets the database clear
                // that history too.
                let reset = match holds
                    .records(grant, TWO_FACTOR_RESET_KIND, &[user])
                    .await?
                    .get(&user)
                {
                    Some(hold) => {
                        erased
                            .kept
                            .push(Kept::held(TWO_FACTOR_RESET_KIND, user, *hold));
                        false
                    }
                    None => bilibi_audit::holds_values(tx, TWO_FACTOR_RESET_KIND, user).await?,
                };
                let user_history = done && bilibi_audit::holds_values(tx, USER_KIND, user).await?;
                let reset_before = json!({"name": name, "email": email});
                let before = json!({"name": name, "email": email, "status": status});
                let writes = !done
                    || user_history
                    || !gone.is_empty()
                    || !scim.is_empty()
                    || bootstrap
                    || reset;
                let (removed, other) = if !writes {
                    (Vec::new(), Vec::new())
                } else {
                    tx.mutate(move |sql| {
                        Box::pin(async move {
                            let mut changes = Vec::new();
                            // Every other record holding the account's values,
                            // reported so their history is cleared too.
                            let mut other: Vec<(&'static str, i64)> = Vec::new();
                            if !done {
                                sql.execute(
                                    "UPDATE users SET name=$3,email=$4,password_hash='!erased',status='deleted',deleted_at=coalesce(deleted_at,now()),\
                                     anonymised_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                                    &[&tenant, &user, &ERASED, &placeholder],
                                )
                                .await?;
                                let links = remove_links(sql, tenant, user, &[]).await?;
                                other.extend(
                                    links
                                        .iter()
                                        .filter(|link| link.kind == EXTERNAL_IDENTITY_KIND)
                                        .map(|link| (EXTERNAL_IDENTITY_KIND, link.id)),
                                );
                                changes.push(Change {
                                    kind: USER_KIND.into(),
                                    id: user,
                                    old: before,
                                    new: json!({"name": ERASED, "email": ERASED, "status": "deleted", "links_removed": links.len()}),
                                });
                                changes.extend(links);
                            } else if user_history {
                                // Already anonymised, but earlier history still
                                // holds values: this change lets it be cleared.
                                sql.execute(
                                    "UPDATE users SET version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2",
                                    &[&tenant, &user],
                                )
                                .await?;
                                changes.push(Change {
                                    kind: USER_KIND.into(),
                                    id: user,
                                    old: before,
                                    new: json!({"name": ERASED, "email": ERASED, "status": "deleted"}),
                                });
                                other.push((USER_KIND, user));
                            }
                            // The identity provider keeps the resource ID; its
                            // document keeps nothing about the person.
                            for row in sql
                                .query(
                                    "UPDATE user_scim_users s SET active=false,document=jsonb_build_object('schemas',o.document->'schemas','id',s.resource_id,'userName',$3::text,'active',false) \
                                     FROM (SELECT id,document FROM user_scim_users WHERE tenant_id=$1 AND id=ANY($2)) o WHERE s.id=o.id RETURNING s.id,o.document,s.document",
                                    &[&tenant, &scim, &placeholder],
                                )
                                .await?
                            {
                                let id: i64 = row.get(0);
                                changes.push(Change { kind: SCIM_KIND.into(), id, old: row.get(1), new: row.get(2) });
                                other.push((SCIM_KIND, id));
                            }
                            if bootstrap {
                                let row = sql
                                    .query_one(
                                        "UPDATE user_bootstrap b SET admin_email=$3 FROM (SELECT admin_email FROM user_bootstrap WHERE tenant_id=$1 AND user_id=$2) o \
                                         WHERE b.tenant_id=$1 AND b.user_id=$2 RETURNING o.admin_email",
                                        &[&tenant, &user, &placeholder],
                                    )
                                    .await?;
                                changes.push(Change {
                                    kind: BOOTSTRAP_KIND.into(),
                                    id: 1,
                                    old: json!({"admin_email": row.get::<_, String>(0)}),
                                    new: json!({"admin_email": ERASED}),
                                });
                                other.push((BOOTSTRAP_KIND, 1));
                            }
                            if reset {
                                changes.push(Change {
                                    kind: TWO_FACTOR_RESET_KIND.into(),
                                    id: user,
                                    old: reset_before,
                                    new: json!({"name": ERASED, "email": ERASED}),
                                });
                                other.push((TWO_FACTOR_RESET_KIND, user));
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
                            Ok(((ids, other), changes))
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
                erased
                    .records
                    .extend(other.into_iter().map(|(kind, id)| ErasedRecord {
                        kind: kind.into(),
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
&PersonalData { label: "User accounts and notifications", kinds: &[USER_KIND, NOTIFICATION_KIND, SCIM_KIND, EXTERNAL_IDENTITY_KIND, BOOTSTRAP_KIND, TWO_FACTOR_RESET_KIND], person_subjects: &[], handler: &ACCOUNTS }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.user",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: USER_KIND, fields: &["name", "email"], subject_identifier: false, person: false }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.notification",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: NOTIFICATION_KIND, fields: &["title", "body"], subject_identifier: false, person: false }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.scim_user",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: SCIM_KIND, fields: &SCIM_FIELDS, subject_identifier: false, person: false }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.external_identity",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: EXTERNAL_IDENTITY_KIND, fields: &["subject"], subject_identifier: false, person: false }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.bootstrap",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: BOOTSTRAP_KIND, fields: &["admin_email"], subject_identifier: false, person: false }) }
inventory::submit! { Contribution::new(RECORD_KINDS, "core/user.two_factor_reset",
Provenance { module: "core/user", source: file!(), line: line!() },
&RecordKind { kind: TWO_FACTOR_RESET_KIND, fields: &["name", "email"], subject_identifier: false, person: false }) }

/// Deletes an account's sign-in and credential rows, one `Change` per row
/// with its key and owner only: never a token, hash or external subject. A
/// single sign-on link in `keep_identities` (a legal hold names it) stays.
pub(crate) async fn remove_links(
    sql: &tokio_postgres::Transaction<'_>,
    tenant: i64,
    user: i64,
    keep_identities: &[i64],
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
            "user.external_session",
            row.get(0),
            json!({"user": user}),
        ));
    }
    for row in sql
        .query("DELETE FROM user_external_identities WHERE tenant_id=$1 AND user_id=$2 AND NOT id=ANY($3) RETURNING id,provider_id", &[&tenant, &user, &keep_identities])
        .await?
    {
        changes.push(removed(
            "user.external_identity",
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
            "user_credential_attempts",
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
