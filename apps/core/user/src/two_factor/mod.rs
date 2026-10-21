//! Authenticator-app two-factor authentication (TOTP, RFC 6238), and the
//! second-factor provider behind Base Step-up (`provider.rs`). Bilimbi has
//! no counterpart yet; the flow follows GitHub's: re-enter the password, scan
//! a QR code, confirm with a current code. There are no recovery codes. A
//! person who loses their phone asks an admin to reset 2FA
//! ([`RESET_KEY`], `agent.rs`), or the server owner runs
//! `bilibi-web two-factor-reset`. This module knows nothing of what step-up
//! guards: it checks the account holder, opens the browser session's window
//! ([`CONFIRM_PATH`]) and spends a code an agent carries, for whatever
//! purpose the contract names.
//!
//! Every account-level write is one `MutationBoundary` transaction whose
//! action names what happened (`user.two_factor.enabled`, `.failed`,
//! `.locked`, ...); no code or secret is ever part of a `Change`, an action
//! payload or a log line. The secret is sealed with the installation key
//! (`Settings::seal_owned`) and never leaves the server except inside the
//! enrollment page, once, after the password was re-entered.
mod agent;
mod provider;
mod qr;
mod totp;

pub(crate) use agent::bind;
pub use agent::{TwoFactorEmailError, TwoFactorEmailReset};
pub use provider::{Authenticator, METHOD};

use crate::{password, UserService};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::Html;
use serde_json::json;
use tokio_postgres::GenericClient;

/// The account's two-factor settings page (enrollment and management).
pub const SETTINGS_PATH: &str = "/settings/two-factor";
/// The step-up prompt a browser is sent to (Base Step-up's web proof).
pub const CONFIRM_PATH: &str = "/settings/two-factor/confirm";
/// An admin resets another person's 2FA. Declared `.always()`: their own
/// current authenticator code, every time.
pub(crate) const RESET_KEY: &str = "core.user.two_factor.reset";
/// The account page event that calls [`RESET_KEY`].
pub(crate) const RESET_EVENT: &str = "reset-two-factor";
/// The page service that owns [`RESET_EVENT`].
pub(crate) const PAGES: &str = "core/user.pages";
/// The record the sealed secret is bound to (`Settings::seal_owned`).
const OWNER: &str = "core/user.two_factor";
const KIND: &str = "user_two_factor";
/// Failed checks allowed per window before every check of the account locks.
const MAX_FAILURES: i32 = 5;
const WINDOW_SECONDS: f64 = 900.0;
const LOCK_SECONDS: f64 = 900.0;
const MAX_RETURN: usize = 2048;

/// Why a two-factor operation was refused; pages map each to a field error.
#[derive(Debug, thiserror::Error)]
pub enum TwoFactorError {
    #[error("is incorrect")]
    IncorrectPassword,
    /// Wrong, malformed, expired or already used: one answer for all.
    #[error("is not a valid authentication code")]
    InvalidCode,
    #[error("Too many failed attempts. Try again in {minutes} minute(s).")]
    Locked { minutes: i64 },
    #[error("2FA is not set up for this account.")]
    NotEnabled,
    #[error("2FA is already on for this account.")]
    AlreadyEnabled,
    #[error("This setup has expired. Start again.")]
    NoPendingEnrollment,
    #[error("Leave impersonation first: only the account's own holder can do this.")]
    Impersonating,
    #[error("2FA needs BILIBI_SETTINGS_KEY, which this installation lacks.")]
    Unavailable,
    #[error(transparent)]
    Other(#[from] Error),
}
type Outcome<T> = std::result::Result<T, TwoFactorError>;

/// What the settings page shows about the account.
#[derive(Clone, Copy, Debug, Default)]
pub struct TwoFactorStatus {
    pub enabled: bool,
}

/// A pending enrollment's display: the QR code and the same key for typing.
/// Built only right after the password was re-entered, never stored, never
/// logged (no `Debug`).
pub struct Enrollment {
    pub qr: Html,
    pub key: String,
    pub issuer: String,
    pub account: String,
    /// The base32 secret, for [`UserService::confirm_move`] only.
    secret: String,
}
impl Enrollment {
    fn new(issuer: String, account: String, secret: String) -> Result<Self> {
        let uri = qr::key_uri(&issuer, &account, &secret);
        Ok(Self {
            qr: qr::svg(&uri, &format!("QR code for {issuer} ({account})"))?,
            key: qr::grouped(&secret),
            issuer,
            account,
            secret,
        })
    }
}

/// What a proven code is spent on.
enum Spend {
    /// Opens the browser session's step-up window.
    StepUp,
    /// Covers one step-up action the contract names (an agent's code).
    Purpose {
        operation: String,
        channel: &'static str,
    },
}

async fn spent_for(
    sql: &impl GenericClient,
    sessions: &bilibi_security::Sessions,
    scope: &Scope,
    spend: Spend,
) -> Result<()> {
    if let Spend::StepUp = spend {
        sessions.step_up(sql, scope).await?;
    }
    Ok(())
}

/// A local path the browser may be sent back to after enrollment or step-up:
/// absolute, not protocol-relative, bounded, no backslash or control
/// character, and not a two-factor page itself (which would loop).
pub fn safe_return(path: &str) -> Option<String> {
    let path = path.trim();
    (path.starts_with('/')
        && !path.starts_with("//")
        && path.len() <= MAX_RETURN
        && !path.contains('\\')
        && !path.chars().any(char::is_control)
        && !path.starts_with(SETTINGS_PATH))
    .then(|| path.to_owned())
}

fn with_return(base: &str, return_to: Option<&str>) -> String {
    match return_to.and_then(safe_return) {
        Some(path) => format!(
            "{base}?return={}",
            percent_encoding::utf8_percent_encode(&path, percent_encoding::NON_ALPHANUMERIC)
        ),
        None => base.to_owned(),
    }
}

fn refuse_impersonation(scope: &Scope) -> Outcome<()> {
    if scope.impersonator().is_some() {
        return Err(TwoFactorError::Impersonating);
    }
    Ok(())
}

async fn now_unix(client: &impl GenericClient) -> Result<i64> {
    Ok(client
        .query_one("SELECT floor(extract(epoch FROM now()))::bigint", &[])
        .await?
        .get(0))
}

impl UserService {
    /// Whether the account has a confirmed authenticator.
    pub async fn two_factor_status(&self, scope: &Scope) -> Result<TwoFactorStatus> {
        Ok(TwoFactorStatus {
            enabled: {
                let client = self.db.get().await?;
                client.query_opt("SELECT 1 FROM user_two_factor WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL", &[&scope.tenant(), &scope.user()]).await?.is_some()
            },
        })
    }

    /// Whether any registered method is enrolled for `user` in this tenant.
    pub async fn account_two_factor_on(&self, scope: &Scope, user: i64) -> Result<bool> {
        let client = self.db.get().await?;
        self.step_up.enrolled_for(&**client, scope, user).await
    }

    /// Where a step-up guard sends a browser that has not confirmed access:
    /// the prompt, or enrollment for an account with no authenticator yet.
    /// Either brings it back to `return_to` afterwards.
    pub async fn step_up_destination(&self, scope: &Scope, return_to: &str) -> Result<String> {
        self.step_up.prompt(scope, return_to).await
    }

    /// Step one of enrollment: the password again, then a fresh secret,
    /// sealed and stored as pending (replacing an earlier unfinished one).
    /// Returns what the page shows the app; nothing about it is kept.
    pub async fn begin_two_factor(&self, scope: &Scope, current: &str) -> Outcome<Enrollment> {
        refuse_impersonation(scope)?;
        if !self.settings.encryption_available() {
            return Err(TwoFactorError::Unavailable);
        }
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT password_hash,email FROM users WHERE tenant_id=$1 AND id=$2 AND status='active' AND deleted_at IS NULL",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?;
        let (stored, email): (String, String) = (row.get(0), row.get(1));
        let matched = current.len() <= password::MAX_LENGTH
            && password::verify(current, Some(&stored)).await?;
        if !matched {
            self.failed(scope, "password").await?;
            return Err(TwoFactorError::IncorrectPassword);
        }
        let issuer = self.issuer(&**client).await?;
        drop(client);
        let secret = totp::generate_secret()?;
        let encoded = totp::encode_secret(&secret);
        let sealed = self
            .settings
            .seal_owned(OWNER, tenant, user, &json!(encoded))?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.enrollment_started"),
            )
            .await?;
        let started = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // A confirmed row is never replaced here: turning it off
                    // needs a code.
                    let written = sql
                        .execute(
                            "INSERT INTO user_two_factor(tenant_id,user_id,secret) VALUES($1,$2,$3) ON CONFLICT ON CONSTRAINT user_two_factor_pkey DO UPDATE SET secret=EXCLUDED.secret,last_step=NULL,created_at=now(),updated_at=now() WHERE user_two_factor.confirmed_at IS NULL",
                            &[&tenant, &user, &sealed],
                        )
                        .await?;
                    if written != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    Ok(((), vec![change(user, json!({}), json!({"status": "pending"}))]))
                })
            })
            .await;
        match started {
            Ok(()) => tx.commit().await?,
            Err(Error::Conflict) => {
                tx.rollback().await?;
                return Err(TwoFactorError::AlreadyEnabled);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(Enrollment::new(issuer, email, encoded)?)
    }

    /// Step two: a current code from the app turns the pending secret on
    /// and counts as this session's step-up.
    pub async fn confirm_two_factor(&self, scope: &Scope, code: &str) -> Outcome<()> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT secret,confirmed_at IS NOT NULL,last_step FROM user_two_factor WHERE tenant_id=$1 AND user_id=$2",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(TwoFactorError::NoPendingEnrollment)?;
        if row.get::<_, bool>(1) {
            return Err(TwoFactorError::AlreadyEnabled);
        }
        let secret = self.open_secret(tenant, user, &row.get(0))?;
        let step = totp::verify(&secret, code, now_unix(&**client).await?, row.get(2))?;
        drop(client);
        let Some(step) = step else {
            self.failed(scope, "authenticator").await?;
            return Err(TwoFactorError::InvalidCode);
        };
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.enabled"),
            )
            .await?;
        let sessions = self.sessions.clone();
        let scope_for_tx = scope.clone();
        let enabled = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let confirmed = sql
                        .execute(
                            "UPDATE user_two_factor SET confirmed_at=now(),last_step=$3,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NULL AND (last_step IS NULL OR last_step<$3)",
                            &[&tenant, &user, &step],
                        )
                        .await?;
                    if confirmed != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    sessions.step_up(sql, &scope_for_tx).await?;
                    Ok((
                        (),
                        vec![change(
                            user,
                            json!({"status": "pending"}),
                            json!({"status": "enabled"}),
                        )],
                    ))
                })
            })
            .await;
        match enabled {
            Ok(()) => tx.commit().await?,
            // Confirmed or reused concurrently: this request proved nothing new.
            Err(Error::Conflict) => {
                tx.rollback().await?;
                return Err(TwoFactorError::InvalidCode);
            }
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }

    /// Step-up: a current authenticator code marks this session as having
    /// confirmed access just now.
    pub async fn step_up(&self, scope: &Scope, code: &str) -> Outcome<()> {
        let step = self.prove(scope, code).await?;
        self.consume(scope, step, Spend::StepUp).await
    }

    /// Step one of moving to a new phone: a current code from the old
    /// phone (spent here), then a fresh secret for the new one. Nothing is
    /// stored: the old secret keeps working until [`Self::confirm_move`]
    /// proves the new phone's first code.
    pub async fn begin_move(&self, scope: &Scope, code: &str) -> Outcome<Enrollment> {
        if !self.settings.encryption_available() {
            return Err(TwoFactorError::Unavailable);
        }
        let step = self.prove(scope, code).await?;
        self.consume(
            scope,
            step,
            Spend::Purpose {
                operation: "core.user.two_factor.move".into(),
                channel: "web",
            },
        )
        .await?;
        let client = self.db.get().await.map_err(Error::from)?;
        let email: String = client
            .query_opt(
                "SELECT email FROM users WHERE tenant_id=$1 AND id=$2 AND status='active' AND deleted_at IS NULL",
                &[&scope.tenant(), &scope.user()],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?
            .get(0);
        let issuer = self.issuer(&**client).await?;
        drop(client);
        let encoded = totp::encode_secret(&totp::generate_secret()?);
        Ok(Enrollment::new(issuer, email, encoded)?)
    }

    /// Step two of moving: the new phone's first code replaces the secret.
    /// The old phone stops working. Audited as `user.two_factor.moved`.
    pub async fn confirm_move(&self, scope: &Scope, to: &Enrollment, code: &str) -> Outcome<()> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let secret = data_encoding::BASE32_NOPAD
            .decode(to.secret.as_bytes())
            .map_err(|_| Error::Internal)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let step = totp::verify(&secret, code, now_unix(&**client).await?, None)?;
        drop(client);
        let Some(step) = step else {
            self.failed(scope, "authenticator").await?;
            return Err(TwoFactorError::InvalidCode);
        };
        let sealed = self
            .settings
            .seal_owned(OWNER, tenant, user, &json!(to.secret))?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.two_factor.moved"))
            .await?;
        let moved = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let replaced = sql
                        .execute(
                            "UPDATE user_two_factor SET secret=$3,last_step=$4,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL",
                            &[&tenant, &user, &sealed, &step],
                        )
                        .await?;
                    if replaced != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    Ok((
                        (),
                        vec![change(
                            user,
                            json!({"status": "enabled"}),
                            json!({"status": "enabled", "moved_to_new_phone": true}),
                        )],
                    ))
                })
            })
            .await;
        match moved {
            Ok(()) => Ok(tx.commit().await?),
            Err(Error::Conflict) => {
                tx.rollback().await?;
                Err(TwoFactorError::NotEnabled)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// The name the authenticator app lists this server under, from the
    /// instance identity (Base System): `Bilibi <name> (<role>)`, or
    /// `Bilibi <role>` when the instance still carries its role's own name.
    async fn issuer(&self, client: &(impl GenericClient + Sync)) -> Result<String> {
        let instance = bilibi_system::Instance::resolve(&self.settings, client).await?;
        let role = instance.role.label();
        let name = instance.label.replace(':', " ");
        Ok(if name.trim().eq_ignore_ascii_case(role) {
            format!("Bilibi {role}")
        } else {
            format!("Bilibi {} ({role})", name.trim())
        })
    }

    /// Checks a code against the confirmed authenticator without consuming
    /// it. A miss counts as a failure.
    async fn prove(&self, scope: &Scope, code: &str) -> Outcome<i64> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT secret,last_step FROM user_two_factor WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(TwoFactorError::NotEnabled)?;
        let secret = self.open_secret(tenant, user, &row.get(0))?;
        let step = totp::verify(&secret, code, now_unix(&**client).await?, row.get(1))?;
        drop(client);
        match step {
            Some(step) => Ok(step),
            None => {
                self.failed(scope, "code").await?;
                Err(TwoFactorError::InvalidCode)
            }
        }
    }

    /// Spends a proved step inside one audited transaction with what it was
    /// for. The step moves forward by compare-and-set, so a racing reuse of
    /// the same code loses, and failures reset.
    async fn consume(&self, scope: &Scope, step: i64, spend: Spend) -> Outcome<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let event = "user.two_factor.verified";
        let purpose = match &spend {
            Spend::Purpose { operation, channel } => json!({"for": operation, "channel": channel}),
            _ => json!({}),
        };
        let sessions = self.sessions.clone();
        let session = scope.clone();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context(event))
            .await?;
        let spent = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let spent = sql
                        .execute(
                            "UPDATE user_two_factor SET last_step=$3,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL AND (last_step IS NULL OR last_step<$3)",
                            &[&tenant, &user, &step],
                        )
                        .await?;
                    if spent != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    spent_for(sql, &sessions, &session, spend).await?;
                    let mut new = json!({"status": "enabled", "checked_with": "authenticator"});
                    if let (Some(new), Some(purpose)) = (new.as_object_mut(), purpose.as_object()) {
                        new.extend(purpose.clone());
                    }
                    Ok(((), vec![change(user, json!({"status": "enabled"}), new)]))
                })
            })
            .await;
        match spent {
            Ok(()) => Ok(tx.commit().await?),
            Err(Error::Conflict) => {
                tx.rollback().await?;
                self.failed(scope, "code").await?;
                Err(TwoFactorError::InvalidCode)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Refuses while the account's checks are locked, recording the refusal.
    async fn unlocked(&self, scope: &Scope) -> Outcome<()> {
        let client = self.db.get().await.map_err(Error::from)?;
        let remaining: Option<i64> = client
            .query_opt(
                "SELECT ceil(extract(epoch FROM locked_until-now()))::bigint FROM user_two_factor_attempts WHERE tenant_id=$1 AND user_id=$2 AND locked_until>now()",
                &[&scope.tenant(), &scope.user()],
            )
            .await
            .map_err(Error::from)?
            .map(|row| row.get(0));
        let Some(seconds) = remaining else {
            return Ok(());
        };
        record_action(
            &**client,
            &Attribution::from_scope(scope),
            None,
            "user.two_factor.refused",
            json!({"semantic": true, "source": "TwoFactor", "summary": "Two-factor check refused while locked", "result": "refused", "reason": "locked"}),
            false,
        )
        .await?;
        Err(TwoFactorError::Locked {
            minutes: (seconds + 59) / 60,
        })
    }

    /// Counts one failed check (`check` names which: password or authenticator
    /// code), audited as `user.two_factor.failed`; the failure that
    /// reaches the limit then locks the account's checks, audited as
    /// `user.two_factor.locked`.
    async fn failed(&self, scope: &Scope, check: &'static str) -> Result<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.failed"),
            )
            .await?;
        let failures = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let failures: i32 = sql
                        .query_one(
                            "INSERT INTO user_two_factor_attempts AS a(tenant_id,user_id,failures) VALUES($1,$2,1) ON CONFLICT ON CONSTRAINT user_two_factor_attempts_pkey DO UPDATE SET failures=CASE WHEN a.window_started_at<=now()-make_interval(secs => $3::double precision) THEN 1 ELSE a.failures+1 END, window_started_at=CASE WHEN a.window_started_at<=now()-make_interval(secs => $3::double precision) THEN now() ELSE a.window_started_at END RETURNING failures",
                            &[&tenant, &user, &WINDOW_SECONDS],
                        )
                        .await?
                        .get(0);
                    Ok((
                        failures,
                        vec![attempts_change(user, json!({"failures": failures, "check": check}))],
                    ))
                })
            })
            .await?;
        tx.commit().await?;
        if failures < MAX_FAILURES {
            return Ok(());
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.locked"),
            )
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let until: String = sql
                    .query_one(
                        "UPDATE user_two_factor_attempts SET failures=0,window_started_at=now(),locked_until=now()+make_interval(secs => $3::double precision) WHERE tenant_id=$1 AND user_id=$2 RETURNING locked_until::text",
                        &[&tenant, &user, &LOCK_SECONDS],
                    )
                    .await?
                    .get(0);
                Ok(((), vec![attempts_change(user, json!({"locked_until": until}))]))
            })
        })
        .await?;
        tx.commit().await
    }

    fn open_secret(&self, tenant: i64, user: i64, sealed: &serde_json::Value) -> Outcome<Vec<u8>> {
        if !self.settings.encryption_available() {
            return Err(TwoFactorError::Unavailable);
        }
        let opened = self.settings.open_owned(OWNER, tenant, user, sealed)?;
        let encoded = opened.as_str().ok_or(Error::Internal)?;
        Ok(data_encoding::BASE32_NOPAD
            .decode(encoded.as_bytes())
            .map_err(|_| Error::Internal)?)
    }
}

fn change(user: i64, old: serde_json::Value, new: serde_json::Value) -> Change {
    Change {
        kind: KIND.into(),
        id: user,
        old,
        new,
    }
}
fn attempts_change(user: i64, new: serde_json::Value) -> Change {
    Change {
        kind: "user_two_factor_attempts".into(),
        id: user,
        old: json!({}),
        new,
    }
}

async fn clear_failures(sql: &impl GenericClient, tenant: i64, user: i64) -> Result<()> {
    sql.execute(
        "UPDATE user_two_factor_attempts SET failures=0,window_started_at=now() WHERE tenant_id=$1 AND user_id=$2 AND failures>0",
        &[&tenant, &user],
    )
    .await?;
    Ok(())
}
