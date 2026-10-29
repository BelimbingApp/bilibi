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
use bilibi_database::{Context, Error, MutationTransaction};
use bilibi_mail::{Address, Message};
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_notifications::Notice;
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
}

fn field(message: &str) -> AgentError {
    AgentError::InvalidInput(BTreeMap::from([("user_id".into(), vec![message.into()])]))
}

/// Staff accounts only: a customer's factor is not a staff reset (ADR 15).
const TARGET_SQL: &str =
    "SELECT u.id, u.tenant_id, u.name, u.email, u.status='active' FROM users u WHERE u.kind='staff'";

fn account_from(row: &tokio_postgres::Row) -> Account {
    Account {
        id: row.get(0),
        tenant: row.get(1),
        name: row.get(2),
        email: row.get(3),
        active: row.get(4),
    }
}

async fn target_in(
    client: &dyn OperationClient,
    scope: &Scope,
    user: i64,
) -> bilibi_agent_api::Result<Account> {
    let rows = client
        .query(
            &format!("{TARGET_SQL} AND u.tenant_id=$1 AND u.id=$2 AND u.deleted_at IS NULL"),
            &[&scope.tenant(), &user],
        )
        .await?;
    rows.first().map(account_from).ok_or(AgentError::NotFound)
}

fn user_id(input: &Value) -> i64 {
    input["user_id"].as_i64().unwrap_or(0)
}

fn refuse(scope: &Scope, account: &Account, enrolled: bool) -> Result<(), AgentError> {
    if scope.impersonator().is_some() {
        return Err(field("Leave impersonation before resetting 2FA."));
    }
    if scope.user() == account.id {
        return Err(field("You cannot reset your own 2FA."));
    }
    if !enrolled {
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
    Some(Message::text(
        Address::new(Some(name), email).ok()?,
        "Your 2FA was reset",
        format!("{body}\n\n{origin}{SETTINGS_PATH}\n"),
    ))
}

const ADMIN_BODY: &str =
    "An admin turned off your 2FA methods. Sign in and set it up again in Settings.";
const SYSTEM_BODY: &str =
    "A server administrator turned off your 2FA methods. Sign in and set it up again in Settings.";

/// Deletes the confirmed authenticator, ends every session of that account
/// and records who did it. Returns the ended session ids for announce after
/// commit. Session ids are not written into the audit payload.
async fn clear_confirmed(
    service: &UserService,
    tx: &mut MutationTransaction<'_>,
    account: &Account,
    by: Value,
) -> Result<Vec<String>, Error> {
    // Serialize resets and deletion through the owning account row.
    tx.sql()
        .query_opt(
            "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL FOR UPDATE",
            &[&tx.context().tenant, &account.id],
        )
        .await?
        .ok_or(Error::NotFound)?;
    if !service.step_up.account_enrolled(tx, account.id).await? {
        return Err(Error::Conflict);
    }
    service
        .step_up
        .lifecycle(tx, account.id, bilibi_step_up::Lifecycle::Reset)
        .await?;
    let mut after = json!({"status":"off","user_id":account.id,"name":account.name,"email":account.email,"signed_out":true});
    if let (Some(after), Some(by)) = (after.as_object_mut(), by.as_object()) {
        after.extend(by.clone());
    }
    tx.record(bilibi_database::Change {
        kind: "user_two_factor_reset".into(), id: account.id,
        old: json!({"status":"enabled", "user_id":account.id,"name":account.name,"email":account.email}),
        new: after,
    }).await?;
    service.sessions.revoke(tx, account.id, None).await
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
            let account = target_in(client, scope, user_id(input)).await?;
            refuse(
                scope,
                &account,
                users()?
                    .step_up
                    .enrolled_for(client, scope, account.id)
                    .await?,
            )?;
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
            let account = target_in(tx.sql(), scope, user_id(input)).await?;
            refuse(
                scope,
                &account,
                service.step_up.account_enrolled(tx, account.id).await?,
            )?;

            // Spend the admin's own code before locking the target's row.
            service
                .step_up
                .require_code(scope, RESET_KEY, &format!("user:{}", account.id))
                .await?;
            let admin = tx.sql().query_opt("SELECT name,email FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL", &[&scope.tenant(), &scope.user()]).await.map_err(Error::from)?.ok_or(Error::Unauthorized)?;
            let by = json!({"reset_by_user_id":scope.user(), "reset_by_name":admin.get::<_,String>(0), "reset_by_email":admin.get::<_,String>(1)});
            let ended = clear_confirmed(service, tx, &account, by)
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
                &format!("{TARGET_SQL} AND lower(u.email)=$1 AND u.deleted_at IS NULL"),
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
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(account.tenant, "two-factor-reset", "user.two_factor.reset"),
            )
            .await?;
        let ended = match clear_confirmed(
            self,
            &mut tx,
            &account,
            json!({"reset_by":"system", "reset_by_name":"two-factor-reset"}),
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
    json!({"type":"object","required":["user_id"],"properties":{"user_id":{"type":"integer","minimum":1,"description":"The account whose 2FA to turn off. Not the caller's own account."}}})
}
fn reset_output() -> Value {
    json!({"type":"object","description":"The account, whether 2FA was on, and a summary. The commit reports user_id and reset true. The account holder is signed out and notified."})
}

inventory::submit! { Contribution::new(DEFINITIONS, "core/user", Provenance { module: "core/user", source: file!(), line: line!() }, &Definitions {
    operations: &[
        Operation { key: RESET_KEY, title: "Reset a person's 2FA", summary: "Turn off another person's 2FA and sign them out. They set it up again in Settings. You confirm with your own current 2FA code. You cannot reset your own.", keywords: &["2fa", "two-factor", "authenticator", "reset", "phone", "lost"], capability: crate::contributions::UPDATE, input: reset_schema, output: reset_output, handler: Handler::SafeWrite(&Reset), channels: Channels::Every, step_up: &[RESET_KEY] },
    ],
    guides: &[Guide { key: "core.user.two_factor", title: "Reset 2FA", summary: "Turn off another person's 2FA when they have lost their phone.", keywords: &["2fa", "two-factor", "reset", "authenticator"], capability: crate::contributions::UPDATE,
        body: "There are no recovery codes. To turn off someone else's 2FA, plan core.user.two_factor.reset with their user_id. Show them the review, then commit with the current code from your own 2FA methods. You cannot reset your own 2FA this way. The person is signed out and notified, and they set the app up again in Settings. A server owner with no other admin runs bilibi-web two-factor-reset --email on the server." }],
}) }

bilibi_step_up::declare!(
    "core/user",
    bilibi_step_up::Operation::new(RESET_KEY, "Reset another person's 2FA").always()
);
