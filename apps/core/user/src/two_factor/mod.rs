//! Authenticator-app two-factor authentication (TOTP, RFC 6238) and the
//! step-up check ("sudo mode") built on it. Bilimbi has no counterpart yet;
//! the flow follows GitHub's: re-enter the password, scan a QR code, confirm
//! with a current code, keep ten one-time recovery codes. A sensitive page
//! asks for a fresh code through the host's `Access::StepUp` guard, which
//! reads [`UserService::stepped_up`] and sends the browser to
//! [`CONFIRM_PATH`] or, with no authenticator yet, [`SETTINGS_PATH`].
//!
//! Every account-level write is one `MutationBoundary` transaction whose
//! action names what happened (`user.two_factor.enabled`, `.failed`,
//! `.locked`, ...); no code, secret or recovery code is ever part of a
//! `Change`, an action payload or a log line. The secret is sealed with the
//! installation key (`Settings::seal_owned`) and never leaves the server
//! except inside the enrollment page, once, after the password was re-entered.
mod qr;
mod recovery;
mod totp;

use crate::{password, UserService};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::Html;
use serde_json::json;
use tokio_postgres::GenericClient;

pub const ISSUER_KEY: &str = "two_factor.issuer";
pub const STEP_UP_MINUTES_KEY: &str = "two_factor.step_up_minutes";
/// The account's two-factor settings page (enrollment and management).
pub const SETTINGS_PATH: &str = "/settings/two-factor";
/// The step-up prompt a guarded page sends the browser to.
pub const CONFIRM_PATH: &str = "/settings/two-factor/confirm";
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
    #[error("is not a valid authentication or recovery code")]
    InvalidCode,
    #[error("Too many failed attempts. Try again in {minutes} minute(s).")]
    Locked { minutes: i64 },
    #[error("Two-factor authentication is not set up for this account.")]
    NotEnabled,
    #[error("Two-factor authentication is already on for this account.")]
    AlreadyEnabled,
    #[error("This setup has expired. Start again.")]
    NoPendingEnrollment,
    #[error("Leave impersonation first: only the account's own holder can do this.")]
    Impersonating,
    #[error("Two-factor authentication needs BILIBI_SETTINGS_KEY, which this installation lacks.")]
    Unavailable,
    #[error(transparent)]
    Other(#[from] Error),
}
type Outcome<T> = std::result::Result<T, TwoFactorError>;

/// What the settings page shows about the account.
#[derive(Clone, Copy, Debug, Default)]
pub struct TwoFactorStatus {
    pub enabled: bool,
    pub recovery_codes_left: i64,
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
    StepUp,
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

/// How a code proved the account's holder.
enum Proof {
    Authenticator(i64),
    Recovery(i64),
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
    /// Whether the account has a confirmed authenticator, and how many
    /// recovery codes are unused.
    pub async fn two_factor_status(&self, scope: &Scope) -> Result<TwoFactorStatus> {
        let client = self.db.get().await?;
        let row = client
            .query_opt(
                "SELECT t.confirmed_at IS NOT NULL, (SELECT count(*) FROM user_two_factor_recovery_codes c WHERE c.tenant_id=t.tenant_id AND c.user_id=t.user_id AND c.used_at IS NULL) FROM user_two_factor t WHERE t.tenant_id=$1 AND t.user_id=$2",
                &[&scope.tenant(), &scope.user()],
            )
            .await?;
        Ok(row.map_or_else(TwoFactorStatus::default, |row| TwoFactorStatus {
            enabled: row.get(0),
            recovery_codes_left: row.get(1),
        }))
    }

    /// Where a step-up guard sends a browser that has not confirmed access:
    /// the prompt, or enrollment for an account with no authenticator yet.
    /// Either brings it back to `return_to` afterwards.
    pub async fn step_up_destination(&self, scope: &Scope, return_to: &str) -> Result<String> {
        let enabled = self.two_factor_status(scope).await?.enabled;
        let base = if enabled { CONFIRM_PATH } else { SETTINGS_PATH };
        Ok(with_return(base, Some(return_to)))
    }

    /// Whether this session passed an authenticator check within the
    /// installation's step-up window (`two_factor.step_up_minutes`). An
    /// impersonated session never has: the check proves the account's own
    /// holder, so an operator cannot step up as someone else.
    pub async fn stepped_up(&self, scope: &Scope) -> Result<bool> {
        if scope.impersonator().is_some() {
            return Ok(false);
        }
        let minutes = self.step_up_minutes().await?;
        let client = self.db.get().await?;
        self.sessions
            .stepped_up_within(&**client, scope, minutes.saturating_mul(60))
            .await
    }

    /// The installation's step-up window in minutes.
    pub async fn step_up_minutes(&self) -> Result<i64> {
        let client = self.db.get().await?;
        self.settings
            .get_global(&**client, STEP_UP_MINUTES_KEY)
            .await?
            .as_i64()
            .ok_or(Error::Internal)
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
        let matched =
            current.len() <= password::MAX_LENGTH && password::verify(current, Some(&stored)).await?;
        if !matched {
            self.failed(scope, "password").await?;
            return Err(TwoFactorError::IncorrectPassword);
        }
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
        let (codes, hashes) = fresh_recovery_codes().await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.two_factor.enabled"))
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
        let proof = self.prove(scope, code).await?;
        self.consume(scope, proof, Spend::StepUp).await
    }

    /// Replaces the recovery codes (the old set stops working at once) after
    /// a current code; returns the new set, shown once.
    pub async fn regenerate_recovery_codes(
        &self,
        scope: &Scope,
        code: &str,
    ) -> Outcome<Vec<String>> {
        let proof = self.prove(scope, code).await?;
        let (codes, hashes) = fresh_recovery_codes().await?;
        self.consume(scope, proof, Spend::Regenerate(hashes)).await?;
        Ok(codes)
    }

    /// Turns two-factor off after a current code or recovery code: the
    /// secret and every recovery code are deleted.
    pub async fn disable_two_factor(&self, scope: &Scope, code: &str) -> Outcome<()> {
        let proof = self.prove(scope, code).await?;
        self.consume(scope, proof, Spend::Disable).await
    }

    /// Checks a code against the confirmed authenticator, or the unused
    /// recovery codes, without consuming it. A miss counts as a failure.
    async fn prove(&self, scope: &Scope, code: &str) -> Outcome<Proof> {
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

    /// Spends a proof inside one audited transaction with what it was for:
    /// the authenticator step moves forward (compare-and-set, so a racing
    /// reuse of the same code loses) or the recovery code is marked used, and
    /// failures reset. A step-up proved by a recovery code is audited as
    /// `user.two_factor.recovery_code_used`.
    async fn consume(&self, scope: &Scope, proof: Proof, spend: Spend) -> Outcome<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let method = match proof {
            Proof::Authenticator(_) => "authenticator",
            Proof::Recovery(_) => "recovery_code",
        };
        let event = match (&spend, &proof) {
            (Spend::StepUp, Proof::Recovery(_)) => "user.two_factor.recovery_code_used",
            (Spend::StepUp, Proof::Authenticator(_)) => "user.two_factor.verified",
            (Spend::Regenerate(_), _) => "user.two_factor.recovery_codes_regenerated",
            (Spend::Disable, _) => "user.two_factor.disabled",
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
                    let spent = match proof {
                        Proof::Authenticator(step) => sql
                            .execute(
                                "UPDATE user_two_factor SET last_step=$3,updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL AND (last_step IS NULL OR last_step<$3)",
                                &[&tenant, &user, &step],
                            )
                            .await?,
                        Proof::Recovery(id) => sql
                            .execute(
                                "UPDATE user_two_factor_recovery_codes SET used_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL",
                                &[&id, &tenant, &user],
                            )
                            .await?,
                    };
                    if spent != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    let new = match spent_for(sql, &sessions, &session, spend).await? {
                        Spent::Kept => json!({"status": "enabled", "checked_with": method}),
                        Spent::Deleted => json!({}),
                    };
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

    /// Counts one failed check (`check` names which: password, authenticator
    /// or code), audited as `user.two_factor.failed`; the failure that
    /// reaches the limit then locks the account's checks, audited as
    /// `user.two_factor.locked`.
    async fn failed(&self, scope: &Scope, check: &'static str) -> Result<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.two_factor.failed"))
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
            .begin(&mut client, scope.mutation_context("user.two_factor.locked"))
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
