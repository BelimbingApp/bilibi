//! Reset another person's 2FA (`core.user.two_factor.reset`) and the server
//! command that does the same with no signed-in admin. The page and every
//! agent door share this contract. The command is a system action: there is
//! no session to present a code.
use super::{RESET_KEY, SETTINGS_PATH};
use crate::{notifications, reset::normalize_email, UserService};
use bilibi_agent_api::{
    Channels, Definitions, Error as AgentError, Guide, Handler, Operation, OperationClient,
    OperationFuture, WriteHandler, DEFINITIONS,
};
use bilibi_database::{Change, Context, Error, MutationTransaction};
use bilibi_mail::{Address, Message};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_notifications::Notice;
use bilibi_security::Sessions;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    sync::{Arc, OnceLock},
};

static USERS: OnceLock<Arc<UserService>> = OnceLock::new();

pub(crate) fn bind(users: Arc<UserService>) -> Result<(), bilibi_module_registry::RegistryError> {
    USERS.set(users).map_err(|_| {
        bilibi_module_registry::RegistryError::Service(
            "core/user two-factor reset bound twice".into(),
        )
    })
}
fn users() -> bilibi_agent_api::Result<&'static Arc<UserService>> {
    USERS.get().ok_or(Error::Internal.into())
}

/// What `bilibi-web two-factor-reset` did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TwoFactorEmailReset {
    /// The authenticator was on and is now off. The account was signed out.
    Reset,
    /// The account has no confirmed authenticator. Nothing was written.
    AlreadyOff,
}
#[derive(Debug, thiserror::Error)]
pub enum TwoFactorEmailError {
    #[error("No account uses that email.")]
    NotFound,
    #[error("That email address is not valid.")]
    Invalid,
    #[error(transparent)]
    Other(#[from] Error),
}

struct Account {
    id: i64,
    tenant: i64,
    name: String,
    email: String,
    active: bool,
    enabled: bool,
}
struct Admin {
    id: i64,
    name: String,
    email: String,
}

fn field(message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([("user_id".into(), vec![message.into()])]))
}

const TARGET_SQL: &str = "SELECT u.id, u.tenant_id, u.name, u.email, u.status='active', COALESCE((SELECT t.confirmed_at IS NOT NULL FROM user_two_factor t WHERE t.tenant_id=u.tenant_id AND t.user_id=u.id), false) FROM users u";

fn account_from(row: &tokio_postgres::Row) -> Account {
    Account {
        id: row.get(0),
        tenant: row.get(1),
        name: row.get(2),
        email: row.get(3),
        active: row.get(4),
        enabled: row.get(5),
    }
}

async fn target_in(
    client: &dyn OperationClient,
    tenant: i64,
    user: i64,
) -> bilibi_agent_api::Result<Account> {
    let rows = client
        .query(
            &format!("{TARGET_SQL} WHERE u.tenant_id=$1 AND u.id=$2 AND u.deleted_at IS NULL"),
            &[&tenant, &user],
        )
        .await?;
    rows.first().map(account_from).ok_or(AgentError::NotFound)
}

fn user_id(input: &Value) -> i64 {
    input["user_id"].as_i64().unwrap_or(0)
}

fn refuse(scope: &Scope, account: &Account) -> Result<(), AgentError> {
    if scope.impersonator().is_some() {
        return Err(field("Leave impersonation before resetting 2FA."));
    }
    if scope.user() == account.id {
        return Err(field("You cannot reset your own 2FA."));
    }
    if !account.enabled {
        return Err(field("2FA is not on for this account."));
    }
    Ok(())
}

fn reset_notice(body: &str) -> Result<Notice, Error> {
    let notice = Notice::new("Your 2FA was reset")
        .kind("two_factor_reset")
        .body(body)
        .url(SETTINGS_PATH);
    notice.validate()?;
    Ok(notice)
}

fn reset_mail(origin: &str, name: &str, email: &str, body: &str) -> Option<Message> {
    Some(Message {
        to: Address::new(Some(name), email).ok()?,
        subject: "Your 2FA was reset".into(),
        text: format!("{body}\n\n{origin}{SETTINGS_PATH}\n"),
    })
}

const ADMIN_BODY: &str =
    "An admin turned off your authenticator app. Sign in and set it up again in Settings.";
const SYSTEM_BODY: &str = "A server administrator turned off your authenticator app. Sign in and set it up again in Settings.";

/// Deletes the confirmed authenticator, ends every session of that account
/// and records who did it. Returns the ended session ids for announce after
/// commit. Session ids are not written into the audit payload.
async fn clear_confirmed(
    tx: &mut MutationTransaction<'_>,
    account: &Account,
    by: Value,
) -> Result<Vec<String>, Error> {
    let tenant = account.tenant;
    let user = account.id;
    let old = json!({"status": "enabled", "user_id": account.id, "name": account.name, "email": account.email});
    let mut new = json!({"status": "off", "user_id": account.id, "name": account.name, "email": account.email, "signed_out": true});
    if let (Some(new), Some(by)) = (new.as_object_mut(), by.as_object()) {
        new.extend(by.clone());
    }
    tx.mutate(move |sql| {
        Box::pin(async move {
            let deleted = sql
                .execute(
                    "DELETE FROM user_two_factor WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL",
                    &[&tenant, &user],
                )
                .await?;
            if deleted != 1 {
                return Err(Error::Conflict);
            }
            let attempts = sql
                .execute(
                    "DELETE FROM user_two_factor_attempts WHERE tenant_id=$1 AND user_id=$2",
                    &[&tenant, &user],
                )
                .await?;
            // A reset means the phone is gone: SMS codes to it stop too. The
            // number stays on the account, unproved.
            let sms_off = sql
                .execute(
                    "UPDATE users SET phone_verified_at=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND phone_verified_at IS NOT NULL",
                    &[&tenant, &user],
                )
                .await?;
            let ended = Sessions::end_user_sessions(sql, tenant, user, None).await?;
            let mut changes = vec![Change {
                kind: "user_two_factor".into(),
                id: user,
                old,
                new,
            }];
            if attempts > 0 {
                changes.push(Change {
                    kind: "user_two_factor_attempts".into(),
                    id: user,
                    old: json!({}),
                    new: json!({"reset": true}),
                });
            }
            if sms_off > 0 {
                changes.push(Change {
                    kind: "user".into(),
                    id: user,
                    old: json!({"phone_verified": true}),
                    new: json!({"phone_verified": false}),
                });
            }
            Ok((ended, changes))
        })
    })
    .await
}

async fn notify_target(
    tx: &mut MutationTransaction<'_>,
    scope: &Scope,
    account: &Account,
    body: &str,
) -> Result<(), Error> {
    if !account.active {
        return Ok(());
    }
    notifications::record(tx, scope, &[account.id], &reset_notice(body)?).await
}

fn wake(
    service: &UserService,
    tenant: i64,
    user: i64,
    sessions: Vec<String>,
    mail: Option<Message>,
) {
    service.notifications.changed(tenant, &[user]);
    service.sessions.announce(sessions);
    let Some(message) = mail else {
        return;
    };
    let mailer = service.mailer.clone();
    tokio::spawn(async move {
        if let Err(error) = mailer.deliver(&message).await {
            eprintln!("mail: 2FA reset notice for user {user} was not delivered: {error}");
        }
    });
}

async fn admin_of(sql: &impl tokio_postgres::GenericClient, scope: &Scope) -> Result<Admin, Error> {
    let row = sql
        .query_opt(
            "SELECT name, email FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&scope.tenant(), &scope.user()],
        )
        .await?
        .ok_or(Error::Unauthorized)?;
    Ok(Admin {
        id: scope.user(),
        name: row.get(0),
        email: row.get(1),
    })
}

struct Reset;
impl WriteHandler for Reset {
    fn owner(&self) -> &'static str {
        "core/user"
    }
    fn preview<'a>(
        &'a self,
        client: &'a dyn OperationClient,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let account = target_in(client, scope.tenant(), user_id(input)).await?;
            refuse(scope, &account)?;
            Ok(json!({
                "user_id": account.id,
                "name": account.name,
                "email": account.email,
                "summary": format!("Turn off 2FA for {} and sign them out. They set it up again in Settings.", account.name),
                "step_up": [RESET_KEY],
            })
            .into())
        })
    }
    fn commit<'a>(
        &'a self,
        tx: &'a mut MutationTransaction<'_>,
        scope: &'a Scope,
        input: &'a Value,
    ) -> OperationFuture<'a> {
        Box::pin(async move {
            let service = users()?;
            bilibi_authz::authorize_key(tx.sql(), scope, crate::contributions::UPDATE).await?;
            let account = target_in(tx.sql(), scope.tenant(), user_id(input)).await?;
            refuse(scope, &account)?;
            let admin = admin_of(tx.sql(), scope).await?;
            // Spend the admin's own code before locking the target's row.
            service
                .step_up
                .require_code(scope, RESET_KEY, &format!("user:{}", account.id))
                .await?;
            let ended = clear_confirmed(
                tx,
                &account,
                json!({"reset_by_user_id": admin.id, "reset_by_name": admin.name, "reset_by_email": admin.email}),
            )
            .await
            .map_err(|error| match error {
                Error::Conflict => field("2FA is not on for this account."),
                other => other.into(),
            })?;
            notify_target(tx, scope, &account, ADMIN_BODY).await?;
            let mail = reset_mail(&service.origin, &account.name, &account.email, ADMIN_BODY);
            let (tenant, user) = (account.tenant, account.id);
            let service = Arc::clone(service);
            tx.after_commit(move || wake(&service, tenant, user, ended, mail));
            Ok(json!({"user_id": user, "reset": true}).into())
        })
    }
}

impl UserService {
    /// Turns off the confirmed authenticator for the one non-deleted account
    /// with this email. Already off is success and writes nothing. The audit
    /// actor is the system principal `two-factor-reset`.
    pub async fn reset_two_factor_by_email(
        &self,
        email: &str,
    ) -> Result<TwoFactorEmailReset, TwoFactorEmailError> {
        let email = normalize_email(email).map_err(|_| TwoFactorEmailError::Invalid)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let rows = client
            .query(
                &format!("{TARGET_SQL} WHERE lower(u.email)=$1 AND u.deleted_at IS NULL"),
                &[&email],
            )
            .await
            .map_err(Error::from)?;
        let account = match rows.as_slice() {
            [] => return Err(TwoFactorEmailError::NotFound),
            [row] => account_from(row),
            _ => return Err(Error::Conflict.into()),
        };
        drop(client);
        if !account.enabled {
            return Ok(TwoFactorEmailReset::AlreadyOff);
        }
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(account.tenant, "two-factor-reset", "user.two_factor.reset"),
            )
            .await?;
        let ended = match clear_confirmed(
            &mut tx,
            &account,
            json!({"reset_by": "system", "reset_by_name": "two-factor-reset"}),
        )
        .await
        {
            Ok(ended) => ended,
            Err(Error::Conflict) => {
                tx.rollback().await?;
                return Ok(TwoFactorEmailReset::AlreadyOff);
            }
            Err(error) => return Err(error.into()),
        };
        if account.active {
            let notice = reset_notice(SYSTEM_BODY)?;
            notifications::record_in_tenant(&mut tx, account.tenant, &[account.id], &notice)
                .await?;
        }
        let mail = reset_mail(&self.origin, &account.name, &account.email, SYSTEM_BODY);
        let (tenant, user) = (account.tenant, account.id);
        // Live pages subscribe to the service bound at boot, which is this one.
        let service = USERS.get().cloned().ok_or(Error::Internal)?;
        tx.after_commit(move || wake(&service, tenant, user, ended, mail));
        tx.commit().await?;
        Ok(TwoFactorEmailReset::Reset)
    }
}

fn reset_schema() -> Value {
    json!({"type":"object","required":["user_id"],"properties":{"user_id":{"type":"integer","minimum":1,"description":"The account whose authenticator to turn off. Not the caller's own account."}}})
}
fn reset_output() -> Value {
    json!({"type":"object","description":"The account, whether 2FA was on, and a summary. The commit reports user_id and reset true. The account holder is signed out and notified."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "core/user", Provenance { module: "core/user", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: RESET_KEY, title: "Reset a person's 2FA", summary: "Turn off another person's authenticator and sign them out. They set it up again in Settings. You confirm with your own current authenticator code. You cannot reset your own.", keywords: &["2fa", "two-factor", "authenticator", "reset", "phone", "lost"], capability: crate::contributions::UPDATE, input: reset_schema, output: reset_output, handler: Handler::SafeWrite(&Reset), channels: Channels::Every, step_up: &[RESET_KEY] },
    ],
    guides: &[Guide { key: "core.user.two_factor", title: "Reset 2FA", summary: "Turn off another person's authenticator when they have lost their phone.", keywords: &["2fa", "two-factor", "reset", "authenticator"], capability: crate::contributions::UPDATE,
        body: "There are no recovery codes. To turn off someone else's authenticator, plan core.user.two_factor.reset with their user_id. Show them the review, then commit with the current code from your own authenticator app. You cannot reset your own 2FA this way. The person is signed out and notified, and they set the app up again in Settings. A server owner with no other admin runs bilibi-web two-factor-reset --email on the server." }],
}) }

bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(RESET_KEY, "Reset another person's 2FA").always()
);
