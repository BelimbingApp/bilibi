//! Text-message (SMS) codes, the second method beside the authenticator app.
//! Bilimbi has no counterpart. A code is six random digits sent through Base
//! SMS, stored only as an Argon2id hash, for one purpose and one number. It
//! works once, for five minutes, and five wrong tries end it. A new code ends
//! the earlier ones. Sending is limited per account: one code a minute, five
//! an hour. The account-wide failure lock in `mod.rs` applies on top.
use super::{refuse_impersonation, Outcome, TwoFactorError};
use crate::{password, UserService};
use bilibi_database::{Change, Error, Result};
use bilibi_sms::PhoneNumber;
use bilibi_tenancy::Scope;
use serde_json::json;
use tokio_postgres::GenericClient;

pub const CODE_MINUTES: i64 = 5;
const RESEND_SECONDS: i64 = 60;
const HOURLY_LIMIT: i64 = 5;
pub(super) const MAX_TRIES: i32 = 5;

/// What a code is for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Purpose {
    /// Proving a number the holder is adding.
    VerifyPhone,
    /// A sign-in or step-up check against the verified number.
    Check,
}
impl Purpose {
    fn as_str(self) -> &'static str {
        match self {
            Self::VerifyPhone => "verify_phone",
            Self::Check => "check",
        }
    }
}

/// What one presented code matched.
pub(super) enum Match {
    /// The live code of that purpose, by id, with the number it went to.
    Hit(i64, String),
    /// A live code exists and this was not it; its id takes the failure.
    Miss(i64),
    /// Nothing to compare with (none sent, expired, used or worn out).
    None,
}

/// The account's phone as pages show it.
#[derive(Clone, Debug)]
pub struct PhoneStatus {
    /// The stored number, for the holder's own form only.
    pub number: String,
    pub masked: String,
    pub verified: bool,
}

/// Six digits, uniform, from the operating system's CSPRNG.
fn random_code() -> Result<String> {
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
        let value = u32::from_be_bytes(bytes);
        // Reject the top sliver so every code is equally likely.
        if value < u32::MAX - u32::MAX % 1_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

pub(super) async fn phone(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Option<PhoneStatus>> {
    let row = sql
        .query_opt(
            "SELECT phone_number,verified_at IS NOT NULL FROM user_phones WHERE tenant_id=$1 AND user_id=$2",
            &[&tenant, &user],
        )
        .await?;
    Ok(row.and_then(|row| {
        let number: String = row.get(0);
        let parsed = PhoneNumber::parse(&number)?;
        Some(PhoneStatus {
            masked: parsed.masked(),
            number,
            verified: row.get(1),
        })
    }))
}

/// The live code of `purpose` against `code`, without spending it.
pub(super) async fn check(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    purpose: Purpose,
    code: &str,
) -> Result<Match> {
    let row = sql
        .query_opt(
            "SELECT id,code_hash,phone_number FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND purpose=$3 AND used_at IS NULL AND expires_at>now() AND failures<$4 ORDER BY id DESC LIMIT 1",
            &[&tenant, &user, &purpose.as_str(), &MAX_TRIES],
        )
        .await?;
    let Some(row) = row else {
        return Ok(Match::None);
    };
    let digits: String = code.chars().filter(|c| !c.is_whitespace()).collect();
    let hash: String = row.get(1);
    Ok(if password::verify(&digits, Some(&hash)).await? {
        Match::Hit(row.get(0), row.get(2))
    } else {
        Match::Miss(row.get(0))
    })
}

/// One wrong try on a code; the fifth ends it. Inside the caller's write.
pub(super) async fn wrong_try(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<()> {
    sql.execute(
        "UPDATE user_sms_codes SET failures=failures+1,expires_at=CASE WHEN failures+1>=$4 THEN now() ELSE expires_at END WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND failures<$4",
        &[&id, &tenant, &user, &MAX_TRIES],
    )
    .await?;
    Ok(())
}

/// Spends a matched code; `false` when it was spent or ended meanwhile.
pub(super) async fn spend(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<bool> {
    Ok(sql
        .execute(
            "UPDATE user_sms_codes SET used_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL AND expires_at>now() AND failures<$4",
            &[&id, &tenant, &user, &MAX_TRIES],
        )
        .await?
        == 1)
}

impl UserService {
    /// The account's own phone, verified or not.
    pub async fn phone_status(&self, scope: &Scope) -> Result<Option<PhoneStatus>> {
        let client = self.db.get().await?;
        phone(&**client, scope.tenant(), scope.user()).await
    }

    /// Another account's methods, for its administration page: the app is
    /// on, and its phone if any. The caller has already read the account.
    pub(crate) async fn two_factor_of(
        &self,
        scope: &Scope,
        user: i64,
    ) -> Result<(bool, Option<PhoneStatus>)> {
        let client = self.db.get().await?;
        let app = client
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM user_two_factor WHERE tenant_id=$1 AND user_id=$2 AND confirmed_at IS NOT NULL)",
                &[&scope.tenant(), &user],
            )
            .await?
            .get(0);
        Ok((app, phone(&**client, scope.tenant(), user).await?))
    }

    /// Whether this server can send text messages now (its role).
    pub async fn sms_available(&self) -> bool {
        self.sms.available().await
    }

    /// Seconds until another code may be sent to this account (0: now).
    pub async fn sms_wait_seconds(&self, scope: &Scope) -> Result<i64> {
        let client = self.db.get().await?;
        Ok(client
            .query_one(
                "SELECT coalesce(ceil(extract(epoch FROM max(sent_at)+make_interval(secs => $3::double precision)-now()))::bigint,0) FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2",
                &[&scope.tenant(), &scope.user(), &(RESEND_SECONDS as f64)],
            )
            .await?
            .get::<_, i64>(0)
            .max(0))
    }

    /// Sends a check code to the verified number (sign-in, step-up, an
    /// agent's request). Returns the masked number it went to.
    pub async fn send_check_code(&self, scope: &Scope) -> Outcome<String> {
        refuse_impersonation(scope)?;
        let client = self.db.get().await.map_err(Error::from)?;
        let phone = phone(&**client, scope.tenant(), scope.user())
            .await?
            .filter(|phone| phone.verified)
            .ok_or(TwoFactorError::NoPhone)?;
        drop(client);
        let number = PhoneNumber::parse(&phone.number).ok_or(Error::Internal)?;
        self.issue(scope, Purpose::Check, &number).await?;
        Ok(phone.masked)
    }

    /// Adding a number, step one: the password again, then a code to the new
    /// number. Nothing is stored about the number until the code comes back.
    pub async fn begin_phone(&self, scope: &Scope, current: &str, number: &str) -> Outcome<String> {
        refuse_impersonation(scope)?;
        if !self.sms.available().await {
            return Err(TwoFactorError::SmsUnavailable);
        }
        let number = PhoneNumber::parse(number).ok_or(TwoFactorError::InvalidPhone)?;
        self.confirm_password(scope, current).await?;
        self.issue(scope, Purpose::VerifyPhone, &number).await?;
        Ok(number.masked())
    }

    /// Step two: the code sent to the new number makes it the account's
    /// verified phone and counts as this session's step-up.
    pub async fn confirm_phone(&self, scope: &Scope, code: &str) -> Outcome<()> {
        refuse_impersonation(scope)?;
        self.unlocked(scope).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let client = self.db.get().await.map_err(Error::from)?;
        let matched = check(&**client, tenant, user, Purpose::VerifyPhone, code).await?;
        drop(client);
        let (id, number) = match matched {
            Match::Hit(id, number) => (id, number),
            Match::Miss(id) => {
                self.failed_sms(scope, Some(id)).await?;
                return Err(TwoFactorError::InvalidCode);
            }
            Match::None => {
                self.failed_sms(scope, None).await?;
                return Err(TwoFactorError::InvalidCode);
            }
        };
        let masked = PhoneNumber::parse(&number).ok_or(Error::Internal)?.masked();
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.phone_verified"),
            )
            .await?;
        let sessions = self.sessions.clone();
        let session = scope.clone();
        let verified = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    if !spend(sql, tenant, user, id).await? {
                        return Err(Error::Conflict);
                    }
                    let old = phone(sql, tenant, user).await?;
                    sql.execute(
                        "INSERT INTO user_phones(tenant_id,user_id,phone_number,verified_at) VALUES($1,$2,$3,now()) ON CONFLICT ON CONSTRAINT user_phones_pkey DO UPDATE SET phone_number=EXCLUDED.phone_number,verified_at=now(),updated_at=now()",
                        &[&tenant, &user, &number],
                    )
                    .await?;
                    super::clear_failures(sql, tenant, user).await?;
                    sessions.step_up(sql, &session).await?;
                    Ok((
                        (),
                        vec![phone_change(
                            user,
                            old.map_or(json!({}), |old| json!({"phone": old.masked, "verified": old.verified})),
                            json!({"phone": masked, "verified": true}),
                        )],
                    ))
                })
            })
            .await;
        match verified {
            Ok(()) => Ok(tx.commit().await?),
            Err(Error::Conflict) => {
                tx.rollback().await?;
                Err(TwoFactorError::InvalidCode)
            }
            Err(error) => Err(error.into()),
        }
    }

    /// Removes the holder's phone after a current code (either method),
    /// only while the authenticator app stays on: 2FA is never turned off.
    pub async fn remove_phone(&self, scope: &Scope, code: &str) -> Outcome<()> {
        if !self.two_factor_status(scope).await?.authenticator {
            return Err(TwoFactorError::LastMethod);
        }
        let proof = self.prove(scope, code).await?;
        self.consume(scope, proof, super::Spend::RemovePhone).await
    }

    /// Sends one code: the per-account limits, then a new row that ends the
    /// earlier codes, audited as `user.two_factor.sms_sent` (the masked
    /// number and purpose, never the code), then the gateway.
    pub(super) async fn issue(
        &self,
        scope: &Scope,
        purpose: Purpose,
        number: &PhoneNumber,
    ) -> Outcome<()> {
        if !self.sms.available().await {
            return Err(TwoFactorError::SmsUnavailable);
        }
        self.unlocked(scope).await?;
        let code = random_code()?;
        let hash = password::hash(&code).await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("user.two_factor.sms_sent"),
            )
            .await?;
        // One sender at a time per account, so the limits hold under races.
        tx.sql()
            .query_one(
                "SELECT 1 FROM users WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                &[&tenant, &user],
            )
            .await
            .map_err(Error::from)?;
        let limits = tx
            .sql()
            .query_one(
                "SELECT coalesce(ceil(extract(epoch FROM max(sent_at)+make_interval(secs => $3::double precision)-now()))::bigint,0), count(*) FILTER (WHERE sent_at>now()-interval '1 hour') FROM user_sms_codes WHERE tenant_id=$1 AND user_id=$2",
                &[&tenant, &user, &(RESEND_SECONDS as f64)],
            )
            .await
            .map_err(Error::from)?;
        let (wait, last_hour): (i64, i64) = (limits.get(0), limits.get(1));
        if wait > 0 {
            tx.rollback().await?;
            return Err(TwoFactorError::ResendTooSoon { seconds: wait });
        }
        if last_hour >= HOURLY_LIMIT {
            tx.rollback().await?;
            return Err(TwoFactorError::ResendLimit);
        }
        let (stored, masked) = (number.as_str().to_owned(), number.masked());
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE user_sms_codes SET expires_at=now() WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL AND expires_at>now()",
                    &[&tenant, &user],
                )
                .await?;
                let id: i64 = sql
                    .query_one(
                        "INSERT INTO user_sms_codes(tenant_id,user_id,purpose,phone_number,code_hash,expires_at) VALUES($1,$2,$3,$4,$5,now()+make_interval(mins => $6::int)) RETURNING id",
                        &[&tenant, &user, &purpose.as_str(), &stored, &hash, &(CODE_MINUTES as i32)],
                    )
                    .await?
                    .get(0);
                Ok((
                    (),
                    vec![Change {
                        kind: "user_sms_code".into(),
                        id,
                        old: json!({}),
                        new: json!({"purpose": purpose.as_str(), "phone": masked, "expires_in_minutes": CODE_MINUTES}),
                    }],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        let issuer = {
            let client = self.db.get().await.map_err(Error::from)?;
            self.issuer(&**client).await?
        };
        let text =
            format!("{issuer} code: {code}. It expires in {CODE_MINUTES} minutes. Never share it.");
        self.sms.send(number, &text).await.map_err(|error| {
            eprintln!("warning: sms delivery failed: {error}");
            TwoFactorError::Delivery
        })
    }

    /// An administrator sets, changes or clears (`""`) an account's number.
    /// A set number is unverified: it is a second factor only after its
    /// holder accepts a code sent to it. Needs `admin.user.update` and the
    /// step-up operation [`super::PHONE_UPDATE`].
    pub async fn set_phone(
        &self,
        scope: &Scope,
        user: i64,
        number: &str,
    ) -> std::result::Result<(), crate::AccountError> {
        let number = if number.trim().is_empty() {
            None
        } else {
            Some(PhoneNumber::parse(number).ok_or_else(|| {
                crate::AccountError::Validation(vec![crate::FieldError {
                    field: "phone",
                    message: "must be in international format, such as +94771234567".into(),
                }])
            })?)
        };
        self.step_up
            .require(scope, super::PHONE_UPDATE, &format!("user:{user}"))
            .await?;
        let mut client = self.db.get().await.map_err(Error::from)?;
        let mut tx = self
            .mutations
            .begin(&mut client, scope.mutation_context("user.phone.update"))
            .await?;
        let scope = self.fresh(tx.sql(), scope).await?;
        self.open_account_write(&tx, &scope, user, None, crate::contributions::UPDATE)
            .await?;
        let tenant = scope.tenant();
        let old = phone(tx.sql(), tenant, user).await?;
        if old.as_ref().map(|old| old.number.as_str()) == number.as_ref().map(PhoneNumber::as_str) {
            tx.rollback().await?;
            return Ok(());
        }
        tx.mutate(move |sql| {
            Box::pin(async move {
                let new = match &number {
                    Some(number) => {
                        sql.execute(
                            "INSERT INTO user_phones(tenant_id,user_id,phone_number) VALUES($1,$2,$3) ON CONFLICT ON CONSTRAINT user_phones_pkey DO UPDATE SET phone_number=EXCLUDED.phone_number,verified_at=NULL,updated_at=now()",
                            &[&tenant, &user, &number.as_str()],
                        )
                        .await?;
                        json!({"phone": number.masked(), "verified": false})
                    }
                    None => {
                        sql.execute(
                            "DELETE FROM user_phones WHERE tenant_id=$1 AND user_id=$2",
                            &[&tenant, &user],
                        )
                        .await?;
                        json!({})
                    }
                };
                // Codes sent to the old number stop working.
                sql.execute(
                    "UPDATE user_sms_codes SET expires_at=now() WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL AND expires_at>now()",
                    &[&tenant, &user],
                )
                .await?;
                Ok((
                    (),
                    vec![phone_change(
                        user,
                        old.map_or(json!({}), |old| json!({"phone": old.masked, "verified": old.verified})),
                        new,
                    )],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(())
    }
}

fn phone_change(user: i64, old: serde_json::Value, new: serde_json::Value) -> Change {
    Change {
        kind: "user_phone".into(),
        id: user,
        old,
        new,
    }
}
