//! One-time codes by SMS (ADR 03), the second method beside the
//! authenticator. A code is six digits from the OS CSPRNG, stored only as an
//! Argon2id hash, lives five minutes, is used once and is void after five
//! wrong tries; a newer code supersedes it. Sends are throttled per account
//! (60 seconds apart, five an hour). Delivery goes through `base/sms`; the
//! plain code exists only in the message.
//!
//! The phone is the account's own (`users.phone_number`), set by an
//! administrator. A code to it proves the holder has the phone: on the
//! settings page that marks it verified, and only a verified phone answers a
//! step-up or sign-in check.
use super::{clear_failures, refuse_impersonation, Outcome, TwoFactorError};
use crate::{password, UserService};
use bilibi_database::{Change, Error, Result};
use bilibi_sms::{Message, PhoneNumber};
use bilibi_tenancy::Scope;
use serde_json::json;
use tokio_postgres::GenericClient;

pub(super) const CODE_SECONDS: f64 = 300.0;
/// Wrong tries one code survives.
pub(super) const MAX_ATTEMPTS: i32 = 5;
const RESEND_SECONDS: f64 = 60.0;
const SENDS_PER_HOUR: i64 = 5;
const KIND: &str = "user_sms_code";

/// What a code is sent for, kept for the audit trail.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SmsPurpose {
    /// A step-up or sign-in check: the phone must be verified.
    Check,
    /// Proving an administrator-set phone on the settings page.
    VerifyPhone,
}
impl SmsPurpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Check => "check",
            Self::VerifyPhone => "verify_phone",
        }
    }
}

/// Where the code went, as a page may show it.
pub struct SmsSent {
    pub phone: String,
}

/// Six digits, every value equally likely: draws past the largest multiple
/// of a million below 2^32 are thrown away rather than folded in.
fn generate() -> Result<String> {
    const LIMIT: u32 = u32::MAX - (u32::MAX % 1_000_000);
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
        let draw = u32::from_be_bytes(bytes);
        if draw < LIMIT {
            return Ok(format!("{:06}", draw % 1_000_000));
        }
    }
}

/// The account's phone and whether its holder proved it. A stored value that
/// no longer parses counts as no phone.
pub(super) async fn phone(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Option<(PhoneNumber, bool)>> {
    let row = client
        .query_opt(
            "SELECT phone_number,phone_verified_at IS NOT NULL FROM users WHERE tenant_id=$1 AND id=$2 AND deleted_at IS NULL",
            &[&tenant, &user],
        )
        .await?;
    Ok(row.and_then(|row| {
        let number: Option<String> = row.get(0);
        let parsed = PhoneNumber::parse(&number?).ok()?;
        Some((parsed, row.get(1)))
    }))
}

/// The newest live code (unspent, unexpired, tries left, sent to the phone
/// the account still has): its row id and hash.
pub(super) async fn live_code(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Option<(i64, String)>> {
    Ok(client
        .query_opt(
            "SELECT c.id,c.code_hash FROM user_sms_codes c JOIN users u ON u.tenant_id=c.tenant_id AND u.id=c.user_id WHERE c.tenant_id=$1 AND c.user_id=$2 AND c.spent_at IS NULL AND c.expires_at>now() AND c.attempts<$3 AND c.phone_number=u.phone_number ORDER BY c.sent_at DESC, c.id DESC LIMIT 1",
            &[&tenant, &user, &MAX_ATTEMPTS],
        )
        .await?
        .map(|row| (row.get(0), row.get(1))))
}

/// Uses one live code inside the spending transaction; `false` when it was
/// spent, voided or expired meanwhile.
pub(super) async fn spend(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<bool> {
    Ok(sql
        .execute(
            "UPDATE user_sms_codes SET spent_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND spent_at IS NULL AND expires_at>now() AND attempts<$4",
            &[&id, &tenant, &user, &MAX_ATTEMPTS],
        )
        .await?
        == 1)
}

/// Counts one wrong try against a code inside the failure's transaction; the
/// try that reaches the limit voids it. Returns whether it is now void.
pub(super) async fn miss(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<bool> {
    let row = sql
        .query_opt(
            "UPDATE user_sms_codes SET attempts=attempts+1,spent_at=CASE WHEN attempts+1>=$4 THEN now() ELSE spent_at END WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND spent_at IS NULL RETURNING attempts>=$4",
            &[&id, &tenant, &user, &MAX_ATTEMPTS],
        )
        .await?;
    Ok(row.is_some_and(|row| row.get(0)))
}

/// Seconds until the next send is allowed (60 seconds apart), and how many
/// codes went out in the last hour with when the oldest of them ages out.
async fn throttle(client: &impl GenericClient, tenant: i64, user: i64) -> Result<(i64, i64, i64)> {
    let row = client
        .query_one(
            "SELECT coalesce(ceil(extract(epoch FROM max(sent_at)+make_interval(secs => $3::double precision)-now()))::bigint,0), count(*), coalesce(ceil(extract(epoch FROM min(sent_at)+interval '1 hour'-now()))::bigint,0) FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND sent_at>now()-interval '1 hour'",
            &[&tenant, &user, &RESEND_SECONDS],
        )
        .await?;
    Ok((
        row.get::<_, i64>(0).max(0),
        row.get(1),
        row.get::<_, i64>(2).max(0),
    ))
}

/// Seconds the account's checks stay locked (`UserService::unlocked`), which
/// also holds every send.
async fn locked_for(client: &impl GenericClient, tenant: i64, user: i64) -> Result<Option<i64>> {
    Ok(client
        .query_opt(
            "SELECT ceil(extract(epoch FROM locked_until-now()))::bigint FROM user_two_factor_attempts WHERE tenant_id=$1 AND user_id=$2 AND locked_until>now()",
            &[&tenant, &user],
        )
        .await?
        .map(|row| row.get(0)))
}

fn throttled(wait: i64, sent: i64, ages_out: i64) -> Option<TwoFactorError> {
    if wait > 0 {
        Some(TwoFactorError::ResendTooSoon { seconds: wait })
    } else if sent >= SENDS_PER_HOUR {
        Some(TwoFactorError::TooManySends {
            minutes: (ages_out + 59) / 60,
        })
    } else {
        None
    }
}

impl UserService {
    /// Whether a code is waiting for the holder (sent, unspent, unexpired,
    /// tries left), and when none is, why another cannot be sent yet: the
    /// sign-in page must never say a code was sent when the throttle held it.
    pub(super) async fn sms_waiting(&self, scope: &Scope) -> Result<(bool, Option<String>)> {
        let client = self.db.get().await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        if live_code(&**client, tenant, user).await?.is_some() {
            return Ok((true, None));
        }
        if let Some(seconds) = locked_for(&**client, tenant, user).await? {
            let minutes = (seconds + 59) / 60;
            return Ok((false, Some(TwoFactorError::Locked { minutes }.to_string())));
        }
        let (wait, sent, ages_out) = throttle(&**client, tenant, user).await?;
        Ok((
            false,
            throttled(wait, sent, ages_out).map(|refusal| refusal.to_string()),
        ))
    }

    /// Seconds before the account may ask for another code (0 when it may
    /// now), for a page's resend countdown.
    pub async fn sms_resend_in(&self, scope: &Scope) -> Result<i64> {
        let client = self.db.get().await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let (wait, sent, ages_out) = throttle(&**client, tenant, user).await?;
        let wait = if sent >= SENDS_PER_HOUR {
            wait.max(ages_out)
        } else {
            wait
        };
        Ok(wait.max(locked_for(&**client, tenant, user).await?.unwrap_or(0)))
    }

    /// Sends a fresh code to the account's phone, superseding any earlier
    /// one. A check needs a verified phone; verifying needs only a phone.
    /// Audited as `user.two_factor.sms_sent` with the masked number, never
    /// the code. A failed delivery voids the code it sent.
    pub async fn send_sms_code(&self, scope: &Scope, purpose: SmsPurpose) -> Outcome<SmsSent> {
        refuse_impersonation(scope)?;
        if !self.sms.available().await? {
            return Err(TwoFactorError::SmsUnavailable);
        }
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let (number, verified) = phone(&**client, tenant, user)
            .await?
            .ok_or(TwoFactorError::NoPhone)?;
        if purpose == SmsPurpose::Check && !verified {
            return Err(TwoFactorError::PhoneNotVerified);
        }
        let (wait, sent, ages_out) = throttle(&**client, tenant, user).await?;
        if let Some(refusal) = throttled(wait, sent, ages_out) {
            return Err(refusal);
        }
        let issuer = self.issuer(&**client).await?;
        drop(client);
        let code = generate()?;
        let message = Message::new(
            number.clone(),
            &format!("{issuer} verification code: {code}. It expires in 5 minutes. Never share this code."),
        )
        .map_err(|_| Error::Internal)?;
        // Hashed before the transaction opens, so no row lock waits on the hasher.
        let hash = password::hash(&code).await?;
        drop(code);
        let masked = number.masked();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.sms_sent"),
            )
            .await?;
        let (stored, kind, shown) = (number.as_str().to_owned(), purpose.as_str(), masked.clone());
        let inserted = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // The account row serializes two sends racing past the
                    // first throttle read.
                    sql.execute(
                        "SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                        &[&tenant, &user],
                    )
                    .await?;
                    let (wait, sent, ages_out) = throttle(sql, tenant, user).await?;
                    if throttled(wait, sent, ages_out).is_some() {
                        return Err(Error::Conflict);
                    }
                    sql.execute(
                        "DELETE FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND sent_at<now()-interval '1 day'",
                        &[&tenant, &user],
                    )
                    .await?;
                    sql.execute(
                        "UPDATE user_sms_codes SET spent_at=now() WHERE tenant_id=$1 AND user_id=$2 AND spent_at IS NULL",
                        &[&tenant, &user],
                    )
                    .await?;
                    let id: i64 = sql
                        .query_one(
                            "INSERT INTO user_sms_codes(tenant_id,user_id,purpose,phone_number,code_hash,expires_at) VALUES($1,$2,$3,$4,$5,now()+make_interval(secs => $6::double precision)) RETURNING id",
                            &[&tenant, &user, &kind, &stored, &hash, &CODE_SECONDS],
                        )
                        .await?
                        .get(0);
                    Ok((
                        id,
                        vec![Change {
                            kind: KIND.into(),
                            id,
                            old: json!({}),
                            new: json!({"purpose": kind, "phone": shown, "expires_in_seconds": CODE_SECONDS as i64}),
                        }],
                    ))
                })
            })
            .await;
        let id = match inserted {
            Ok(id) => {
                tx.commit().await?;
                id
            }
            Err(Error::Conflict) => {
                tx.rollback().await?;
                let client = self.db.get().await.map_err(Error::from)?;
                let (wait, sent, ages_out) = throttle(&**client, tenant, user).await?;
                return Err(throttled(wait, sent, ages_out)
                    .unwrap_or(TwoFactorError::ResendTooSoon { seconds: 1 }));
            }
            Err(error) => return Err(error.into()),
        };
        match self.sms.send(&message).await {
            Ok(()) => Ok(SmsSent { phone: masked }),
            Err(error) => {
                // The detail is for operators; the holder sees a generic failure.
                eprintln!("warning: sms delivery failed: {error}");
                self.void_sms_code(scope, id).await?;
                Err(match error {
                    bilibi_sms::Error::Unavailable => TwoFactorError::SmsUnavailable,
                    _ => TwoFactorError::SmsFailed,
                })
            }
        }
    }

    /// Voids a code whose message never left, audited as
    /// `user.two_factor.sms_failed`.
    async fn void_sms_code(&self, scope: &Scope, id: i64) -> Result<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.sms_failed"),
            )
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE user_sms_codes SET spent_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND spent_at IS NULL",
                    &[&id, &tenant, &user],
                )
                .await?;
                Ok((
                    (),
                    vec![Change {
                        kind: KIND.into(),
                        id,
                        old: json!({}),
                        new: json!({"status": "void", "reason": "delivery_failed"}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await
    }

    /// Proves the administrator-set phone with a code sent to it, marking it
    /// verified (`user.two_factor.phone_verified`). Already verified is `Ok`.
    pub async fn verify_phone(&self, scope: &Scope, code: &str) -> Outcome<()> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let (number, verified) = phone(&**client, tenant, user)
            .await?
            .ok_or(TwoFactorError::NoPhone)?;
        if verified {
            return Ok(());
        }
        let live = live_code(&**client, tenant, user).await?;
        drop(client);
        let digits = super::totp::normalize(code);
        let matched = match (&digits, &live) {
            (Some(digits), Some((id, hash))) => {
                password::verify(digits, Some(hash)).await?.then_some(*id)
            }
            _ => None,
        };
        let Some(id) = matched else {
            self.failed_with(scope, "sms", live.map(|(id, _)| id))
                .await?;
            return Err(TwoFactorError::InvalidCode);
        };
        let (stored, masked) = (number.as_str().to_owned(), number.masked());
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.phone_verified"),
            )
            .await?;
        let proved = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    if !spend(sql, tenant, user, id).await? {
                        return Err(Error::Conflict);
                    }
                    let marked = sql
                        .execute(
                            "UPDATE users SET phone_verified_at=now(),updated_at=now() WHERE tenant_id=$1 AND id=$2 AND phone_number=$3 AND phone_verified_at IS NULL",
                            &[&tenant, &user, &stored],
                        )
                        .await?;
                    if marked != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    Ok((
                        (),
                        vec![Change {
                            kind: "user".into(),
                            id: user,
                            old: json!({"phone_verified": false}),
                            new: json!({"phone_verified": true, "phone": masked}),
                        }],
                    ))
                })
            })
            .await;
        match proved {
            Ok(()) => Ok(tx.commit().await?),
            Err(Error::Conflict) => {
                tx.rollback().await?;
                Err(TwoFactorError::InvalidCode)
            }
            Err(error) => Err(error.into()),
        }
    }
}
