//! SMS one-time codes, the second method beside the authenticator: a code
//! sent through Base SMS (`bilibi_sms`) to the account's proved number. A
//! code is 6 random digits, kept only as an Argon2id hash, works once for its
//! purpose, expires after 5 minutes and dies after 5 wrong attempts. Sending
//! is bounded per account: once a minute, five an hour. A verified number is
//! SMS being on (`users.phone_verified_at`); only the holder proves it, with
//! a code sent to it. An admin's 2FA reset turns SMS off too; for an
//! SMS-only account, an administrator's new number is the way back.
//! No Bilimbi counterpart.
use super::{clear_failures, refuse_impersonation, Outcome, Spend, TwoFactorError};
use crate::{password, UserService};
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::Scope;
use serde_json::json;
use tokio_postgres::GenericClient;

const CODE_SECONDS: f64 = 300.0;
const MAX_ATTEMPTS: i32 = 5;
const RESEND_SECONDS: i64 = 60;
const HOURLY: i64 = 5;

/// What a code proves: a number before SMS is turned on, or the holder at a
/// sign-in or step-up check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Purpose {
    Verify,
    Check,
}
impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::Verify => "verify",
            Self::Check => "check",
        }
    }
}

/// What a page says after a code went out: where (masked) and how long
/// before another may be asked for.
#[derive(Clone, Debug)]
pub struct SmsSent {
    pub phone: String,
    pub resend_in: i64,
}

/// The account's proved number, when SMS codes are on.
pub(super) async fn verified_phone(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Option<String>> {
    Ok(client
        .query_opt(
            "SELECT phone_number FROM users WHERE tenant_id=$1 AND id=$2 AND phone_verified_at IS NOT NULL",
            &[&tenant, &user],
        )
        .await?
        .and_then(|row| row.get(0)))
}

/// The live code `presented` matches, for `purpose` and `phone`. A miss is
/// the caller's to record (`UserService::failed`), which also counts one
/// attempt against every live code, so each dies after five. Every candidate
/// hash is checked, so the time taken does not say which.
pub(super) async fn matching(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
    phone: &str,
    purpose: Purpose,
    presented: &str,
) -> Result<Option<i64>> {
    let digits: String = presented.chars().filter(|c| !c.is_whitespace()).collect();
    if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Ok(None);
    }
    let rows = client
        .query(
            "SELECT id,code_hash FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND purpose=$3 AND phone_number=$4 AND used_at IS NULL AND expires_at>now() AND attempts<$5 ORDER BY id DESC",
            &[&tenant, &user, &purpose.as_str(), &phone, &MAX_ATTEMPTS],
        )
        .await?;
    let mut matched = None;
    for row in &rows {
        let hash: String = row.get(1);
        if password::verify(&digits, Some(&hash)).await? && matched.is_none() {
            matched = Some(row.get::<_, i64>(0));
        }
    }
    Ok(matched)
}

/// Uses a matched code inside the spending transaction: still live, so a
/// racing second use of the same code finds nothing (`1` row, or `0`).
pub(super) async fn spend(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<u64> {
    Ok(sql
        .execute(
            "UPDATE user_sms_codes SET used_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL AND expires_at>now() AND attempts<$4",
            &[&id, &tenant, &user, &MAX_ATTEMPTS],
        )
        .await?)
}

/// Six uniformly random digits from the OS CSPRNG (rejection sampling, so
/// no digit string is likelier than another).
fn generate() -> Result<String> {
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
        let value = u32::from_le_bytes(bytes);
        if value < 4_000_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

impl UserService {
    /// Seconds before this account may be sent another code (0: now). The
    /// hourly bound shows as the wait until the oldest of the hour ages out.
    pub async fn sms_wait(&self, scope: &Scope) -> Result<i64> {
        let client = self.db.get().await?;
        let (soon, hourly) = waits(&**client, scope.tenant(), scope.user()).await?;
        Ok(soon.max(hourly))
    }

    /// Another code to the number being proved, for a holder who passed the
    /// password step of turning SMS on within the last 15 minutes (a verify
    /// code went out since), so a resend needs no second password.
    pub async fn resend_sms_verify(&self, scope: &Scope) -> Outcome<SmsSent> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT u.phone_number FROM users u WHERE u.tenant_id=$1 AND u.id=$2 AND u.phone_verified_at IS NULL AND EXISTS (SELECT 1 FROM user_sms_codes c WHERE c.tenant_id=u.tenant_id AND c.user_id=u.id AND c.purpose='verify' AND c.phone_number=u.phone_number AND c.sent_at>now()-interval '15 minutes')",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?;
        let phone: String = row
            .and_then(|row| row.get(0))
            .ok_or(TwoFactorError::NoPendingEnrollment)?;
        drop(client);
        self.send_code(scope, Purpose::Verify, &phone).await
    }

    /// A sign-in or step-up code to the account's proved number.
    pub async fn send_sms_check(&self, scope: &Scope) -> Outcome<SmsSent> {
        let client = self.db.get().await.map_err(Error::from)?;
        let phone = verified_phone(&**client, scope.tenant(), scope.user())
            .await?
            .ok_or(TwoFactorError::SmsNotEnabled)?;
        drop(client);
        self.send_code(scope, Purpose::Check, &phone).await
    }

    /// Turning SMS on, step one: the password again (as for the
    /// authenticator), then a code to the number on the account. A holder
    /// may first enter or replace their own number here, while SMS is off;
    /// an empty `phone` keeps the number an administrator entered.
    pub async fn begin_sms(&self, scope: &Scope, current: &str, phone: &str) -> Outcome<SmsSent> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT password_hash,phone_number,phone_verified_at IS NOT NULL FROM users WHERE tenant_id=$1 AND id=$2 AND status='active' AND deleted_at IS NULL",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?;
        let (stored, on_record, verified): (String, Option<String>, bool) =
            (row.get(0), row.get(1), row.get(2));
        drop(client);
        if verified {
            return Err(TwoFactorError::SmsAlreadyEnabled);
        }
        let matched = current.len() <= password::MAX_LENGTH
            && password::verify(current, Some(&stored)).await?;
        if !matched {
            self.failed(scope, "password").await?;
            return Err(TwoFactorError::IncorrectPassword);
        }
        let phone = if phone.trim().is_empty() {
            on_record.ok_or(TwoFactorError::NoPhone)?
        } else {
            let phone = bilibi_sms::normalize_phone(phone).ok_or(TwoFactorError::PhoneFormat)?;
            if on_record.as_deref() != Some(phone.as_str()) {
                self.own_phone(scope, on_record, &phone).await?;
            }
            phone
        };
        self.send_code(scope, Purpose::Verify, &phone).await
    }

    /// Turning SMS on, step two: the code sent to the number proves it, and
    /// SMS codes are on. Opens the session's step-up window, as confirming
    /// the authenticator does; at sign-in it lets the session act instead.
    pub async fn confirm_sms(&self, scope: &Scope, code: &str) -> Outcome<()> {
        self.confirm_sms_for(scope, code, false).await
    }

    pub(super) async fn confirm_sms_for(
        &self,
        scope: &Scope,
        code: &str,
        sign_in: bool,
    ) -> Outcome<()> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let row = client
            .query_opt(
                "SELECT phone_number,phone_verified_at IS NOT NULL FROM users WHERE tenant_id=$1 AND id=$2",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?
            .ok_or(Error::Unauthorized)?;
        let (phone, verified): (Option<String>, bool) = (row.get(0), row.get(1));
        if verified {
            return Err(TwoFactorError::SmsAlreadyEnabled);
        }
        let phone = phone.ok_or(TwoFactorError::NoPhone)?;
        let matched = matching(&**client, tenant, user, &phone, Purpose::Verify, code).await?;
        drop(client);
        let Some(id) = matched else {
            self.failed(scope, "sms").await?;
            return Err(TwoFactorError::InvalidCode);
        };
        let sessions = self.sessions.clone();
        let session = scope.clone();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.sms_enabled"),
            )
            .await?;
        let enabled = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    if spend(sql, tenant, user, id).await? != 1 {
                        return Err(Error::Conflict);
                    }
                    let proved = sql
                        .execute(
                            "UPDATE users SET phone_verified_at=now(),version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND phone_number=$3 AND phone_verified_at IS NULL",
                            &[&tenant, &user, &phone],
                        )
                        .await?;
                    if proved != 1 {
                        return Err(Error::Conflict);
                    }
                    clear_failures(sql, tenant, user).await?;
                    if sign_in {
                        sessions.finish_sign_in(sql, &session, true).await?;
                    } else {
                        sessions.step_up(sql, &session).await?;
                    }
                    Ok((
                        (),
                        vec![Change {
                            kind: "user".into(),
                            id: user,
                            old: json!({"phone_verified": false}),
                            new: json!({"phone_verified": true, "phone": bilibi_sms::mask(&phone)}),
                        }],
                    ))
                })
            })
            .await;
        match enabled {
            Ok(()) => Ok(tx.commit().await?),
            // Used or changed concurrently: this request proved nothing new.
            Err(Error::Conflict) => {
                tx.rollback().await?;
                Err(TwoFactorError::InvalidCode)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Turns SMS codes off after a current code (authenticator or SMS).
    /// The number stays on the account, unproved.
    pub async fn disable_sms(&self, scope: &Scope, code: &str) -> Outcome<()> {
        let proof = self.prove(scope, code).await?;
        self.consume(scope, proof, Spend::DisableSms).await
    }

    /// The holder enters or replaces their own number while SMS is off:
    /// audited like an administrator's edit, and unproved until a code.
    async fn own_phone(&self, scope: &Scope, old: Option<String>, phone: &str) -> Outcome<()> {
        let (tenant, user) = (scope.tenant(), scope.user());
        let new = phone.to_owned();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.update"))
            .await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let changed = sql
                    .execute(
                        "UPDATE users SET phone_number=$3,phone_verified_at=NULL,version=version+1,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND phone_verified_at IS NULL",
                        &[&tenant, &user, &new],
                    )
                    .await?;
                if changed != 1 {
                    return Err(Error::Conflict);
                }
                Ok((
                    (),
                    vec![Change {
                        kind: "user".into(),
                        id: user,
                        old: json!({"phone_number": old}),
                        new: json!({"phone_number": new}),
                    }],
                ))
            })
        })
        .await
        .map_err(|error| match error {
            Error::Conflict => TwoFactorError::SmsAlreadyEnabled,
            error => error.into(),
        })?;
        Ok(tx.commit().await?)
    }

    /// Sends one code for `purpose` to `phone`, inside the account's sending
    /// bounds. The row (hash only) is written and audited first; earlier live
    /// codes of the same purpose stop working, so the newest is the one.
    pub(super) async fn send_code(
        &self,
        scope: &Scope,
        purpose: Purpose,
        phone: &str,
    ) -> Outcome<SmsSent> {
        refuse_impersonation(scope)?;
        if !self.sms.available() {
            return Err(TwoFactorError::SmsUnavailable);
        }
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        {
            let client = self.db.get().await.map_err(Error::from)?;
            let (soon, hourly) = waits(&**client, tenant, user).await?;
            if hourly > 0 {
                return Err(TwoFactorError::SmsTooMany {
                    minutes: (hourly + 59) / 60,
                });
            }
            if soon > 0 {
                return Err(TwoFactorError::SmsTooSoon { seconds: soon });
            }
        }
        let code = generate()?;
        let hash = password::hash(&code).await?;
        let message = bilibi_sms::Message::new(
            phone,
            &format!("Your Bilibi code is {code}. It expires in 5 minutes. Never share it."),
        )
        .map_err(|_| TwoFactorError::NoPhone)?;
        let to = phone.to_owned();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.sms_sent"),
            )
            .await?;
        let sent = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    // One sender at a time per account, so the bounds hold
                    // against two racing requests.
                    sql.query_one(
                        "SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                        &[&tenant, &user],
                    )
                    .await?;
                    sql.execute(
                        "UPDATE user_sms_codes SET expires_at=now() WHERE tenant_id=$1 AND user_id=$2 AND purpose=$3 AND used_at IS NULL AND expires_at>now()",
                        &[&tenant, &user, &purpose.as_str()],
                    )
                    .await?;
                    let row = sql
                        .query_opt(
                            "INSERT INTO user_sms_codes(tenant_id,user_id,purpose,phone_number,code_hash,expires_at) SELECT $1,$2,$3,$4,$5,now()+make_interval(secs => $6::double precision) WHERE NOT EXISTS (SELECT 1 FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND sent_at>now()-make_interval(secs => $7::double precision)) AND (SELECT count(*) FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND sent_at>now()-interval '1 hour')<$8 RETURNING id",
                            &[&tenant, &user, &purpose.as_str(), &to, &hash, &CODE_SECONDS, &(RESEND_SECONDS as f64), &HOURLY],
                        )
                        .await?
                        .ok_or(Error::Conflict)?;
                    let id: i64 = row.get(0);
                    Ok((
                        (),
                        vec![Change {
                            kind: "user_sms_codes".into(),
                            id,
                            old: json!({}),
                            new: json!({"purpose": purpose.as_str(), "phone": bilibi_sms::mask(&to)}),
                        }],
                    ))
                })
            })
            .await;
        match sent {
            Ok(()) => tx.commit().await?,
            Err(Error::Conflict) => {
                tx.rollback().await?;
                return Err(TwoFactorError::SmsTooSoon {
                    seconds: RESEND_SECONDS,
                });
            }
            Err(error) => return Err(error.into()),
        }
        if let Err(error) = self.sms.deliver(&message).await {
            eprintln!("warning: SMS delivery failed: {error}");
            return Err(TwoFactorError::SmsDelivery);
        }
        Ok(SmsSent {
            phone: bilibi_sms::mask(phone),
            resend_in: RESEND_SECONDS,
        })
    }
}

/// Whether a check code sent to the proved number is still usable, so a page
/// that opens again does not send another.
pub(super) async fn live_check(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<bool> {
    Ok(client
        .query_opt(
            "SELECT 1 FROM user_sms_codes c JOIN users u ON u.tenant_id=c.tenant_id AND u.id=c.user_id AND u.phone_number=c.phone_number WHERE c.tenant_id=$1 AND c.user_id=$2 AND c.purpose='check' AND c.used_at IS NULL AND c.expires_at>now() AND c.attempts<$3",
            &[&tenant, &user, &MAX_ATTEMPTS],
        )
        .await?
        .is_some())
}

/// Seconds until the per-minute bound and the hourly bound allow a send.
async fn waits(client: &impl GenericClient, tenant: i64, user: i64) -> Result<(i64, i64)> {
    let row = client
        .query_one(
            "SELECT coalesce(ceil(extract(epoch FROM max(sent_at)+make_interval(secs => $3::double precision)-now())),0)::bigint, CASE WHEN count(*) FILTER (WHERE sent_at>now()-interval '1 hour')>=$4 THEN coalesce(ceil(extract(epoch FROM min(sent_at) FILTER (WHERE sent_at>now()-interval '1 hour')+interval '1 hour'-now())),0)::bigint ELSE 0 END FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2",
            &[&tenant, &user, &(RESEND_SECONDS as f64), &HOURLY],
        )
        .await?;
    Ok((row.get::<_, i64>(0).max(0), row.get::<_, i64>(1).max(0)))
}
