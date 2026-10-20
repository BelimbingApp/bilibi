//! Two-factor authentication, and the second-factor provider behind Base
//! Step-up (`provider.rs`). Two methods, one per account: an authenticator
//! app (TOTP, RFC 6238) the holder sets up, or, for an account with a phone
//! number and no app, a code by text message (`sms.rs`). Bilimbi has no
//! counterpart yet; the app flow follows GitHub's: re-enter the password, scan
//! a QR code, confirm with a current code, keep ten one-time recovery codes.
//! This module knows nothing of what step-up guards: it checks the account
//! holder, opens the browser session's window ([`CONFIRM_PATH`]) and spends a
//! code an agent carries, for whatever purpose the contract names.
//!
//! Every account-level write is one `MutationBoundary` transaction whose
//! action names what happened (`user.two_factor.enabled`, `.failed`,
//! `.locked`, ...); no code, secret or recovery code is ever part of a
//! `Change`, an action payload or a log line. The secret is sealed with the
//! installation key (`Settings::seal_owned`) and never leaves the server
//! except inside the enrollment page, once, after the password was re-entered.
mod provider;
mod qr;
mod recovery;
mod sms;
mod totp;

pub use provider::Authenticator;
pub use sms::{
    CODE_ATTEMPTS_KEY as TEXT_CODE_ATTEMPTS_KEY, CODE_MINUTES_KEY as TEXT_CODE_MINUTES_KEY,
    RESEND_SECONDS_KEY as TEXT_RESEND_SECONDS_KEY, SENDS_PER_HOUR_KEY as TEXT_SENDS_PER_HOUR_KEY,
};

use crate::{password, UserService};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Error, Result};
use bilibi_sms::{Message, PhoneNumber};
use bilibi_tenancy::Scope;
use bilibi_ui::Html;
use serde_json::json;
use tokio_postgres::GenericClient;

pub const ISSUER_KEY: &str = "two_factor.issuer";
/// The account's two-factor settings page (enrollment and management).
pub const SETTINGS_PATH: &str = "/settings/two-factor";
/// The step-up prompt a browser is sent to (Base Step-up's web proof).
pub const CONFIRM_PATH: &str = "/settings/two-factor/confirm";
/// The record the sealed secret is bound to (`Settings::seal_owned`).
const OWNER: &str = "core/user.two_factor";
const KIND: &str = "user_two_factor";
const SMS_KIND: &str = "user_two_factor_sms_codes";
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
    #[error("is not a valid authentication or recovery code")]
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
    #[error("is not the code in the text message")]
    WrongTextCode,
    #[error("This code has expired. Send a new one.")]
    CodeExpired,
    #[error("This account has no phone number for 2FA. Ask an administrator to add yours.")]
    NoPhone,
    #[error("Text messages are not available on this server. Ask an administrator.")]
    TextUnavailable,
    #[error("This account uses an authenticator app. Enter the code it shows.")]
    AuthenticatorInUse,
    #[error("Wait {seconds} seconds before asking for another code.")]
    ResendTooSoon { seconds: i64 },
    #[error("Too many codes were sent. Try again in {minutes} minute(s).")]
    TooManyCodes { minutes: i64 },
    #[error("The text message could not be sent. Try again shortly.")]
    NotDelivered,
    #[error(transparent)]
    Other(#[from] Error),
}
type Outcome<T> = std::result::Result<T, TwoFactorError>;

/// What the settings page shows about the account.
#[derive(Clone, Debug, Default)]
pub struct TwoFactorStatus {
    /// The authenticator app is on.
    pub enabled: bool,
    pub recovery_codes_left: i64,
    /// The account's phone number, masked, when it has one.
    pub phone: Option<String>,
    /// A code sent to that number was entered correctly at least once.
    pub phone_verified: bool,
    /// This server can send text messages (Base SMS has a transport).
    pub text_available: bool,
}

/// Which second factor checks the account's holder.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Method {
    /// The authenticator app and its recovery codes; it wins over a phone.
    Authenticator,
    /// A code by text message to the account's phone number.
    TextMessage,
    /// Neither: the holder cannot pass a check yet.
    None,
}

impl TwoFactorStatus {
    pub fn method(&self) -> Method {
        if self.enabled {
            Method::Authenticator
        } else if self.phone.is_some() && self.text_available {
            Method::TextMessage
        } else {
            Method::None
        }
    }
}

/// A text-message code on its way, as the code pages show it.
#[derive(Clone, Debug)]
pub struct CodeSent {
    /// The number it went to, masked.
    pub phone: String,
    /// Seconds until another code may be asked for.
    pub resend_in: i64,
    /// Minutes a code works (the operator's setting).
    pub minutes: i64,
}

/// A pending enrollment's display: the QR code and the same key for typing.
/// Built only right after the password was re-entered, never stored, never
/// logged (no `Debug`).
pub struct Enrollment {
    pub qr: Html,
    pub key: String,
    pub issuer: String,
    pub account: String,
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
    /// The new set's Argon2id hashes.
    Regenerate(Vec<String>),
    Disable,
}
enum Spent {
    Kept,
    Deleted,
}

async fn spent_for(
    sql: &impl GenericClient,
    sessions: &bilibi_security::Sessions,
    scope: &Scope,
    spend: Spend,
) -> Result<Spent> {
    let (tenant, user) = (scope.tenant(), scope.user());
    match spend {
        Spend::StepUp => sessions.step_up(sql, scope).await?,
        Spend::Purpose { .. } => {}
        Spend::Regenerate(hashes) => replace_recovery_codes(sql, tenant, user, &hashes).await?,
        Spend::Disable => {
            sql.execute(
                "DELETE FROM user_two_factor WHERE tenant_id=$1 AND user_id=$2",
                &[&tenant, &user],
            )
            .await?;
            return Ok(Spent::Deleted);
        }
    }
    Ok(Spent::Kept)
}

/// One change per text-message code voided by a newer code, a new number or
/// a failed delivery.
fn voided_changes(ids: &[i64]) -> Vec<Change> {
    ids.iter()
        .map(|id| Change {
            kind: SMS_KIND.into(),
            id: *id,
            old: json!({"status": "sent"}),
            new: json!({"status": "voided"}),
        })
        .collect()
}

/// How a code proved the account's holder.
enum Proof {
    Authenticator(i64),
    Recovery(i64),
    /// The id of the text-message code row.
    TextMessage(i64),
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

/// Refuses another send while the throttle holds: too soon after the last
/// one, or too many within the hour.
fn refuse_sending(sending: &sms::Sending, limits: sms::Limits) -> Outcome<()> {
    if sending.wait_seconds > 0 {
        return Err(TwoFactorError::ResendTooSoon {
            seconds: sending.wait_seconds,
        });
    }
    if sending.sent_last_hour >= limits.sends_per_hour {
        return Err(TwoFactorError::TooManyCodes {
            minutes: (sending.hour_frees_in + 59) / 60,
        });
    }
    Ok(())
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
    /// Whether the account has a confirmed authenticator, how many recovery
    /// codes are unused, and its phone number for text-message codes.
    pub async fn two_factor_status(&self, scope: &Scope) -> Result<TwoFactorStatus> {
        let client = self.db.get().await?;
        let row = client
            .query_opt(
                "SELECT t.confirmed_at IS NOT NULL, (SELECT count(*) FROM user_two_factor_recovery_codes c WHERE c.tenant_id=u.tenant_id AND c.user_id=u.id AND c.used_at IS NULL), u.phone_number, u.phone_verified_at IS NOT NULL FROM users u LEFT JOIN user_two_factor t ON t.tenant_id=u.tenant_id AND t.user_id=u.id WHERE u.tenant_id=$1 AND u.id=$2",
                &[&scope.tenant(), &scope.user()],
            )
            .await?;
        Ok(row.map_or_else(TwoFactorStatus::default, |row| {
            let phone: Option<String> = row.get(2);
            TwoFactorStatus {
                enabled: row.get(0),
                recovery_codes_left: row.get(1),
                phone: phone
                    .as_deref()
                    .and_then(PhoneNumber::parse)
                    .map(|phone| phone.masked()),
                phone_verified: row.get(3),
                text_available: self.sms.available(),
            }
        }))
    }

    /// Where a step-up guard sends a browser that has not confirmed access:
    /// the prompt, or the two-factor page for an account with no second
    /// factor yet. Either brings it back to `return_to` afterwards.
    pub async fn step_up_destination(&self, scope: &Scope, return_to: &str) -> Result<String> {
        let method = self.two_factor_status(scope).await?.method();
        let base = if method == Method::None {
            SETTINGS_PATH
        } else {
            CONFIRM_PATH
        };
        Ok(with_return(base, Some(return_to)))
    }

    /// Where the holder's text-message codes go and the wait before another
    /// may be sent, without sending one: for a page showing a refused code,
    /// where the holder decides whether to ask for another.
    pub async fn text_code_status(&self, scope: &Scope) -> Outcome<CodeSent> {
        let phone = self
            .two_factor_status(scope)
            .await?
            .phone
            .ok_or(TwoFactorError::NoPhone)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let limits = sms::Limits::read(&self.settings, &**client).await?;
        let sending = sms::Sending::read(&**client, scope.tenant(), scope.user(), limits).await?;
        Ok(CodeSent {
            phone,
            resend_in: sending.wait_seconds,
            minutes: limits.code_minutes,
        })
    }

    /// Sends the holder a text-message code, unless one that can still be
    /// entered is out and `resend` is false: then it says where that one
    /// went. Throttled per account by the operator's limits (`sms.rs`); the
    /// number is read under the account's row lock and stored with the code,
    /// so a change of number at the same moment cannot verify the new number
    /// with a code the old one got. Each code sent is audited as
    /// `user.two_factor.code_sent` without the code. A code the transport
    /// failed to deliver is voided again.
    pub async fn send_two_factor_code(&self, scope: &Scope, resend: bool) -> Outcome<CodeSent> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let status = self.two_factor_status(scope).await?;
        match status.method() {
            Method::TextMessage => {}
            Method::Authenticator => return Err(TwoFactorError::AuthenticatorInUse),
            Method::None if status.phone.is_some() => return Err(TwoFactorError::TextUnavailable),
            Method::None => return Err(TwoFactorError::NoPhone),
        }
        let masked = status.phone.ok_or(TwoFactorError::NoPhone)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let limits = sms::Limits::read(&self.settings, &**client).await?;
        let sending = sms::Sending::read(&**client, tenant, user, limits).await?;
        if sending.active && !resend {
            return Ok(CodeSent {
                phone: masked,
                resend_in: sending.wait_seconds,
                minutes: limits.code_minutes,
            });
        }
        refuse_sending(&sending, limits)?;
        let issuer = self
            .settings
            .get_global(&**client, ISSUER_KEY)
            .await?
            .as_str()
            .ok_or(Error::Internal)?
            .to_owned();
        drop(client);
        // Hashed before the transaction opens, so no row lock waits on it.
        let code = sms::generate()?;
        let hash = password::hash(&code).await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.code_sent"),
            )
            .await?;
        let stored = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // One send at a time per account, to the number it has
                    // under this lock: an administrator's change of number
                    // waits for it, or has already voided what came before.
                    let phone = sql
                        .query_one(
                            "SELECT phone_number FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                            &[&tenant, &user],
                        )
                        .await?
                        .get::<_, Option<String>>(0)
                        .as_deref()
                        .and_then(PhoneNumber::parse)
                        .ok_or(Error::NotFound)?;
                    let sending = sms::Sending::read(sql, tenant, user, limits).await?;
                    if sending.wait_seconds > 0 || sending.sent_last_hour >= limits.sends_per_hour {
                        return Err(Error::Conflict);
                    }
                    let (id, voided) =
                        sms::store(sql, tenant, user, &hash, phone.as_str(), limits).await?;
                    let mut changes = voided_changes(&voided);
                    changes.push(Change {
                        kind: SMS_KIND.into(),
                        id,
                        old: json!({}),
                        new: json!({"status": "sent", "to": phone.masked(), "expires_in_minutes": limits.code_minutes}),
                    });
                    Ok((phone, changes))
                })
            })
            .await;
        let phone = match stored {
            Ok(phone) => {
                tx.commit().await?;
                phone
            }
            Err(Error::Conflict) => {
                tx.rollback().await?;
                let client = self.db.get().await.map_err(Error::from)?;
                refuse_sending(
                    &sms::Sending::read(&**client, tenant, user, limits).await?,
                    limits,
                )?;
                return Err(TwoFactorError::ResendTooSoon {
                    seconds: limits.resend_seconds,
                });
            }
            Err(Error::NotFound) => {
                tx.rollback().await?;
                return Err(TwoFactorError::NoPhone);
            }
            Err(error) => return Err(error.into()),
        };
        let text = format!(
            "{issuer} verification code: {code}. It expires in {} minute{}. Do not share it with anyone.",
            limits.code_minutes,
            if limits.code_minutes == 1 { "" } else { "s" }
        );
        // The origin-bound line needs a host name or IPv4 address; an origin
        // it cannot name sends the code without it.
        let message =
            Message::one_time_code(phone.clone(), &text, sms::origin_host(&self.origin), &code)
                .or_else(|_| Message::new(phone.clone(), &text))
                .map_err(|_| Error::Internal)?;
        if let Err(error) = self.sms.send(&message).await {
            eprintln!("warning: two-factor text message for account {user} failed: {error}");
            self.void_text_codes(scope, "user.two_factor.code_undelivered")
                .await?;
            return Err(TwoFactorError::NotDelivered);
        }
        Ok(CodeSent {
            phone: phone.masked(),
            resend_in: limits.resend_seconds,
            minutes: limits.code_minutes,
        })
    }

    /// Turning the app on for an account whose codes come by text message
    /// replaces its second factor: the step-up operation
    /// `core.user.two_factor.replace`, on the floor, so the holder proves the
    /// phone first and a password alone cannot swap the factor. A browser is
    /// sent to the prompt and back (`Error::StepUpRequired`).
    async fn replacing_text(&self, scope: &Scope) -> Outcome<()> {
        if self.two_factor_status(scope).await?.method() == Method::TextMessage {
            self.step_up
                .require(
                    scope,
                    crate::step_up::REPLACE_FACTOR,
                    &format!("user:{}", scope.user()),
                )
                .await?;
        }
        Ok(())
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
        drop(client);
        self.replacing_text(scope).await?;
        let client = self.db.get().await.map_err(Error::from)?;
        let issuer = self
            .settings
            .get_global(&**client, ISSUER_KEY)
            .await?
            .as_str()
            .ok_or(Error::Internal)?
            .to_owned();
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
        let uri = qr::key_uri(&issuer, &email, &encoded);
        let qr = qr::svg(&uri, &format!("QR code for {issuer} ({email})"))?;
        Ok(Enrollment {
            qr,
            key: qr::grouped(&encoded),
            issuer,
            account: email,
        })
    }

    /// Step two: a current code from the app turns the pending secret on,
    /// replaces any recovery codes with a fresh set (returned once, stored
    /// hashed) and counts as this session's step-up.
    pub async fn confirm_two_factor(&self, scope: &Scope, code: &str) -> Outcome<Vec<String>> {
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
        self.replacing_text(scope).await?;
        let (codes, hashes) = fresh_recovery_codes().await?;
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
                    replace_recovery_codes(sql, tenant, user, &hashes).await?;
                    clear_failures(sql, tenant, user).await?;
                    sessions.step_up(sql, &scope_for_tx).await?;
                    Ok((
                        (),
                        vec![change(
                            user,
                            json!({"status": "pending"}),
                            json!({"status": "enabled", "recovery_codes": recovery::COUNT}),
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
        Ok(codes)
    }

    /// Step-up: a current authenticator code (or one recovery code) marks
    /// this session as having confirmed access just now.
    pub async fn step_up(&self, scope: &Scope, code: &str) -> Outcome<()> {
        let proof = self.prove(scope, code, true).await?;
        self.consume(scope, proof, Spend::StepUp).await
    }

    /// Replaces the recovery codes (the old set stops working at once) after
    /// a current code; returns the new set, shown once.
    pub async fn regenerate_recovery_codes(
        &self,
        scope: &Scope,
        code: &str,
    ) -> Outcome<Vec<String>> {
        let proof = self.prove(scope, code, false).await?;
        let (codes, hashes) = fresh_recovery_codes().await?;
        self.consume(scope, proof, Spend::Regenerate(hashes))
            .await?;
        Ok(codes)
    }

    /// Turns two-factor off after a current code or recovery code: the
    /// secret and every recovery code are deleted.
    pub async fn disable_two_factor(&self, scope: &Scope, code: &str) -> Outcome<()> {
        let proof = self.prove(scope, code, false).await?;
        self.consume(scope, proof, Spend::Disable).await
    }

    /// Checks a code against the confirmed authenticator, or the unused
    /// recovery codes, without consuming it. With `text_message`, an account
    /// without an authenticator is checked against its newest text-message
    /// code instead. A miss counts as a failure.
    async fn prove(&self, scope: &Scope, code: &str, text_message: bool) -> Outcome<Proof> {
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
            .map_err(Error::from)?;
        let row = match row {
            Some(row) => row,
            None if text_message => {
                drop(client);
                return self.prove_text(scope, code).await;
            }
            None => return Err(TwoFactorError::NotEnabled),
        };
        let proof = if totp::normalize(code).is_some() {
            let secret = self.open_secret(tenant, user, &row.get(0))?;
            totp::verify(&secret, code, now_unix(&**client).await?, row.get(1))?
                .map(Proof::Authenticator)
        } else if let Some(code) = recovery::normalize(code) {
            let rows = client
                .query(
                    "SELECT id,code_hash FROM user_two_factor_recovery_codes WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL ORDER BY id",
                    &[&tenant, &user],
                )
                .await
                .map_err(Error::from)?;
            // Every unused hash is checked, so the time taken does not say
            // which position matched.
            let mut matched = None;
            for row in rows {
                let hash: String = row.get(1);
                if password::verify(&code, Some(&hash)).await? && matched.is_none() {
                    matched = Some(Proof::Recovery(row.get(0)));
                }
            }
            matched
        } else {
            None
        };
        drop(client);
        match proof {
            Some(proof) => Ok(proof),
            None => {
                self.failed(scope, "code").await?;
                Err(TwoFactorError::InvalidCode)
            }
        }
    }

    /// Checks a code against the account's newest text-message code that was
    /// sent to the number it has now. A wrong code counts against that code
    /// (the operator's attempts void it) and against the account's checks.
    async fn prove_text(&self, scope: &Scope, code: &str) -> Outcome<Proof> {
        let (tenant, user) = (scope.tenant(), scope.user());
        if !self.sms.available() || self.two_factor_status(scope).await?.phone.is_none() {
            return Err(TwoFactorError::NotEnabled);
        }
        let client = self.db.get().await.map_err(Error::from)?;
        let limits = sms::Limits::read(&self.settings, &**client).await?;
        let active = sms::active(&**client, tenant, user, limits.attempts).await?;
        let expired = active.is_none() && sms::newest_expired(&**client, tenant, user).await?;
        drop(client);
        let (Some(code), Some((id, hash))) = (sms::normalize(code), active) else {
            self.failed(scope, "text_message").await?;
            return Err(if expired {
                TwoFactorError::CodeExpired
            } else {
                TwoFactorError::WrongTextCode
            });
        };
        if password::verify(&code, Some(&hash)).await? {
            return Ok(Proof::TextMessage(id));
        }
        self.text_code_failed(scope, id, limits.attempts).await?;
        self.failed(scope, "text_message").await?;
        Err(TwoFactorError::WrongTextCode)
    }

    /// Counts one wrong attempt on a text-message code; the attempt that
    /// reaches the operator's limit leaves it `invalidated`.
    async fn text_code_failed(&self, scope: &Scope, id: i64, attempts: i32) -> Result<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.code_failed"),
            )
            .await?;
        let counted = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // Voided or used meanwhile: nothing left to count.
                    let failures = sms::count_failure(sql, tenant, user, id)
                        .await?
                        .ok_or(Error::NotFound)?;
                    let status = if failures >= attempts {
                        "invalidated"
                    } else {
                        "sent"
                    };
                    Ok((
                        (),
                        vec![Change {
                            kind: SMS_KIND.into(),
                            id,
                            old: json!({"failures": failures - 1}),
                            new: json!({"status": status, "failures": failures}),
                        }],
                    ))
                })
            })
            .await;
        match counted {
            Ok(()) => tx.commit().await,
            Err(Error::NotFound) => tx.rollback().await,
            Err(error) => Err(error),
        }
    }

    /// Voids every outstanding text-message code of the account, one change
    /// per code, audited as `action` (an undelivered code).
    async fn void_text_codes(&self, scope: &Scope, action: &'static str) -> Result<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context(action))
            .await?;
        let voided = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let voided = sms::void_outstanding(sql, tenant, user).await?;
                    if voided.is_empty() {
                        return Err(Error::NotFound);
                    }
                    Ok(((), voided_changes(&voided)))
                })
            })
            .await;
        match voided {
            Ok(()) => tx.commit().await,
            Err(Error::NotFound) => tx.rollback().await,
            Err(error) => Err(error),
        }
    }

    /// Spends a proof inside one audited transaction with what it was for:
    /// the authenticator step moves forward (compare-and-set, so a racing
    /// reuse of the same code loses), the recovery code or the text-message
    /// code is marked used, and failures reset. The account's lock is
    /// checked again under its row lock first, so a check that passed
    /// `unlocked` before a concurrent failure locked the account does not
    /// complete. A step-up proved by a recovery code is audited as
    /// `user.two_factor.recovery_code_used`.
    async fn consume(&self, scope: &Scope, proof: Proof, spend: Spend) -> Outcome<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let method = match proof {
            Proof::Authenticator(_) => "authenticator",
            Proof::Recovery(_) => "recovery_code",
            Proof::TextMessage(_) => "text_message",
        };
        let event = match (&spend, &proof) {
            (Spend::StepUp | Spend::Purpose { .. }, Proof::Recovery(_)) => {
                "user.two_factor.recovery_code_used"
            }
            (
                Spend::StepUp | Spend::Purpose { .. },
                Proof::Authenticator(_) | Proof::TextMessage(_),
            ) => "user.two_factor.verified",
            (Spend::Regenerate(_), _) => "user.two_factor.recovery_codes_regenerated",
            (Spend::Disable, _) => "user.two_factor.disabled",
        };
        let purpose = match &spend {
            Spend::Purpose { operation, channel } => json!({"for": operation, "channel": channel}),
            _ => json!({}),
        };
        let attempts = match proof {
            Proof::TextMessage(_) => {
                let client = self.db.get().await.map_err(Error::from)?;
                sms::Limits::read(&self.settings, &**client).await?.attempts
            }
            _ => 0,
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
                    let locked: Option<bool> = sql
                        .query_opt(
                            "SELECT locked_until IS NOT NULL AND locked_until>now() FROM user_two_factor_attempts WHERE tenant_id=$1 AND user_id=$2 FOR UPDATE",
                            &[&tenant, &user],
                        )
                        .await?
                        .map(|row| row.get(0));
                    if locked == Some(true) {
                        return Err(Error::Forbidden);
                    }
                    let mut changes = Vec::new();
                    // What the proof spent, for the account's own change.
                    let mut checked = json!({"checked_with": method});
                    if let (Some(checked), Some(purpose)) =
                        (checked.as_object_mut(), purpose.as_object())
                    {
                        checked.extend(purpose.clone());
                    }
                    let text_message = matches!(proof, Proof::TextMessage(_));
                    let spent = match proof {
                        Proof::Authenticator(step) => sql
                            .execute(
                                "UPDATE user_two_factor SET last_step=$3,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL AND (last_step IS NULL OR last_step<$3)",
                                &[&tenant, &user, &step],
                            )
                            .await?
                            == 1,
                        Proof::Recovery(id) => sql
                            .execute(
                                "UPDATE user_two_factor_recovery_codes SET used_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL",
                                &[&id, &tenant, &user],
                            )
                            .await?
                            == 1,
                        Proof::TextMessage(id) => {
                            let spent = sms::spend(sql, tenant, user, id, attempts).await?;
                            if spent.used {
                                let mut new = json!({"status": "used"});
                                if let (Some(new), Some(checked)) =
                                    (new.as_object_mut(), checked.as_object())
                                {
                                    new.extend(checked.clone());
                                }
                                changes.push(Change {
                                    kind: SMS_KIND.into(),
                                    id,
                                    old: json!({"status": "sent"}),
                                    new,
                                });
                            }
                            if spent.verified {
                                changes.push(Change {
                                    kind: "user".into(),
                                    id: user,
                                    old: json!({"phone_verified_at": null}),
                                    new: json!({"phone_verified_at": "set"}),
                                });
                            }
                            spent.used
                        }
                    };
                    if !spent {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    match spent_for(sql, &sessions, &session, spend).await? {
                        // An account with only text messages has no
                        // `user_two_factor` row: the code's change says it.
                        Spent::Kept if text_message => {}
                        Spent::Kept => {
                            let mut new = json!({"status": "enabled"});
                            if let (Some(new), Some(checked)) =
                                (new.as_object_mut(), checked.as_object())
                            {
                                new.extend(checked.clone());
                            }
                            changes.push(change(user, json!({"status": "enabled"}), new));
                        }
                        Spent::Deleted => {
                            changes.push(change(user, json!({"status": "enabled"}), json!({})));
                        }
                    }
                    Ok(((), changes))
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
            Err(Error::Forbidden) => {
                tx.rollback().await?;
                self.unlocked(scope).await?;
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

    /// Counts one failed check (`check` names which: password, authenticator
    /// or code), audited as `user.two_factor.failed`. The failure that
    /// reaches the limit locks every check of the account in the same
    /// transaction, under the attempts row's lock, so concurrent failures
    /// cannot count past it; the lock is then recorded as the action
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
        let locked = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let failures: i32 = sql
                        .query_one(
                            "INSERT INTO user_two_factor_attempts AS a(tenant_id,user_id,failures) VALUES($1,$2,1) ON CONFLICT ON CONSTRAINT user_two_factor_attempts_pkey DO UPDATE SET failures=CASE WHEN a.window_started_at<=now()-make_interval(secs => $3::double precision) THEN 1 ELSE a.failures+1 END, window_started_at=CASE WHEN a.window_started_at<=now()-make_interval(secs => $3::double precision) THEN now() ELSE a.window_started_at END RETURNING failures",
                            &[&tenant, &user, &WINDOW_SECONDS],
                        )
                        .await?
                        .get(0);
                    let mut changes =
                        vec![attempts_change(user, json!({"failures": failures, "check": check}))];
                    if failures < MAX_FAILURES {
                        return Ok((None, changes));
                    }
                    let until: String = sql
                        .query_one(
                            "UPDATE user_two_factor_attempts SET failures=0,window_started_at=now(),locked_until=now()+make_interval(secs => $3::double precision) WHERE tenant_id=$1 AND user_id=$2 RETURNING locked_until::text",
                            &[&tenant, &user, &LOCK_SECONDS],
                        )
                        .await?
                        .get(0);
                    changes.push(attempts_change(user, json!({"locked_until": until})));
                    Ok((Some(until), changes))
                })
            })
            .await?;
        tx.commit().await?;
        if let Some(until) = locked {
            record_action(
                &**client,
                &Attribution::from_scope(scope),
                None,
                "user.two_factor.locked",
                json!({"semantic": true, "source": "TwoFactor", "summary": "Two-factor checks locked after repeated failures", "result": "locked", "locked_until": until}),
                false,
            )
            .await?;
        }
        Ok(())
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

/// A new recovery set and its Argon2id hashes, computed before any
/// transaction opens so no row lock waits on the hasher.
async fn fresh_recovery_codes() -> Result<(Vec<String>, Vec<String>)> {
    let codes = recovery::generate()?;
    let mut hashes = Vec::with_capacity(codes.len());
    for code in &codes {
        let canonical = recovery::normalize(code).ok_or(Error::Internal)?;
        hashes.push(password::hash(&canonical).await?);
    }
    Ok((codes, hashes))
}

async fn replace_recovery_codes(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    hashes: &[String],
) -> Result<()> {
    sql.execute(
        "DELETE FROM user_two_factor_recovery_codes WHERE tenant_id=$1 AND user_id=$2",
        &[&tenant, &user],
    )
    .await?;
    sql.execute(
        "INSERT INTO user_two_factor_recovery_codes(tenant_id,user_id,code_hash) SELECT $1,$2,unnest($3::text[])",
        &[&tenant, &user, &hashes],
    )
    .await?;
    Ok(())
}
