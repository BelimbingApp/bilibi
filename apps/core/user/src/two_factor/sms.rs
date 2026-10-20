//! Text-message codes, the second factor of an account with a phone number
//! and no authenticator app. A code is 6 digits from the operating system's
//! CSPRNG, sent through Base SMS to the number the account has when it is
//! sent, and stored only as an Argon2id hash with that number (`sent_to`).
//! It works for the operator's [`Limits`]: minutes, wrong attempts, the wait
//! before another and the sends an hour per account. It is used once, voided
//! by a newer code or a change of number, and counts only while the account
//! still has the number it went to. A 6-digit code behind Argon2id, the
//! expiry and these limits is out of reach of guessing.
use bilibi_database::{Error, Result};
use bilibi_settings::Settings;
use tokio_postgres::GenericClient;

pub const DIGITS: usize = 6;
/// Installation settings (Core User's contributions), bounded there.
pub const CODE_MINUTES_KEY: &str = "two_factor.text_code_minutes";
pub const CODE_ATTEMPTS_KEY: &str = "two_factor.text_code_attempts";
pub const RESEND_SECONDS_KEY: &str = "two_factor.text_resend_seconds";
pub const SENDS_PER_HOUR_KEY: &str = "two_factor.text_sends_per_hour";

/// The operator's limits on text-message codes, read on each use so a
/// change applies to the next code.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub code_minutes: i64,
    pub attempts: i32,
    pub resend_seconds: i64,
    pub sends_per_hour: i64,
}

impl Limits {
    pub async fn read(settings: &Settings, client: &(impl GenericClient + Sync)) -> Result<Self> {
        let mut values = [0i64; 4];
        for (value, key) in values.iter_mut().zip([
            CODE_MINUTES_KEY,
            CODE_ATTEMPTS_KEY,
            RESEND_SECONDS_KEY,
            SENDS_PER_HOUR_KEY,
        ]) {
            *value = settings
                .get_global(client, key)
                .await?
                .as_i64()
                .ok_or(Error::Internal)?;
        }
        let [code_minutes, attempts, resend_seconds, sends_per_hour] = values;
        Ok(Self {
            code_minutes,
            attempts: i32::try_from(attempts).map_err(|_| Error::Internal)?,
            resend_seconds,
            sends_per_hour,
        })
    }
}

/// A fresh code, uniform over all 10^6 values: a draw at or above the
/// largest multiple of 10^6 below 2^32 is thrown away and drawn again.
pub fn generate() -> Result<String> {
    const RANGE: u32 = 1_000_000;
    const LIMIT: u32 = u32::MAX - u32::MAX % RANGE;
    loop {
        let mut bytes = [0u8; 4];
        getrandom::fill(&mut bytes).map_err(|_| Error::Internal)?;
        let draw = u32::from_le_bytes(bytes);
        bytes.fill(0);
        if draw < LIMIT {
            return Ok(format!("{:0DIGITS$}", draw % RANGE));
        }
    }
}

/// The digits a person typed, without the spaces or hyphen they may add.
pub fn normalize(presented: &str) -> Option<String> {
    let code: String = presented
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '-')
        .collect();
    (code.len() == DIGITS && code.bytes().all(|b| b.is_ascii_digit())).then_some(code)
}

/// How the account's sending stands right now.
pub struct Sending {
    /// A code that can still be entered exists.
    pub active: bool,
    /// Seconds until another code may be sent (0: now).
    pub wait_seconds: i64,
    /// Codes sent within the last hour.
    pub sent_last_hour: i64,
    /// Seconds until the oldest of those leaves the hour.
    pub hour_frees_in: i64,
}

impl Sending {
    pub async fn read(
        sql: &impl GenericClient,
        tenant: i64,
        user: i64,
        limits: Limits,
    ) -> Result<Self> {
        let row = sql
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM user_two_factor_sms_codes c JOIN users u ON u.tenant_id=c.tenant_id AND u.id=c.user_id WHERE c.tenant_id=$1 AND c.user_id=$2 AND c.sent_to=u.phone_number AND c.used_at IS NULL AND c.voided_at IS NULL AND c.expires_at>now() AND c.failures<$3), \
                 COALESCE(ceil(extract(epoch FROM max(sent_at)+make_interval(secs => $4::double precision)-now()))::bigint,0), \
                 count(*) FILTER (WHERE sent_at>now()-interval '1 hour'), \
                 COALESCE(ceil(extract(epoch FROM min(sent_at) FILTER (WHERE sent_at>now()-interval '1 hour')+interval '1 hour'-now()))::bigint,0) \
                 FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2",
                &[&tenant, &user, &limits.attempts, &(limits.resend_seconds as f64)],
            )
            .await?;
        Ok(Self {
            active: row.get(0),
            wait_seconds: row.get::<_, i64>(1).max(0),
            sent_last_hour: row.get(2),
            hour_frees_in: row.get::<_, i64>(3).max(0),
        })
    }
}

/// The newest code that can still be entered, as `(id, hash)`: sent to the
/// number the account has now.
pub async fn active(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    attempts: i32,
) -> Result<Option<(i64, String)>> {
    Ok(sql
        .query_opt(
            "SELECT c.id,c.code_hash FROM user_two_factor_sms_codes c JOIN users u ON u.tenant_id=c.tenant_id AND u.id=c.user_id WHERE c.tenant_id=$1 AND c.user_id=$2 AND c.sent_to=u.phone_number AND c.used_at IS NULL AND c.voided_at IS NULL AND c.expires_at>now() AND c.failures<$3 ORDER BY c.id DESC LIMIT 1",
            &[&tenant, &user, &attempts],
        )
        .await?
        .map(|row| (row.get(0), row.get(1))))
}

/// Whether the newest code sent ran out of time unused (to say "expired"
/// rather than "wrong").
pub async fn newest_expired(sql: &impl GenericClient, tenant: i64, user: i64) -> Result<bool> {
    Ok(sql
        .query_opt(
            "SELECT expires_at<=now() AND used_at IS NULL AND voided_at IS NULL FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2 ORDER BY id DESC LIMIT 1",
            &[&tenant, &user],
        )
        .await?
        .is_some_and(|row| row.get(0)))
}

/// Voids every code still outstanding (a newer code, a new number, a code
/// that was not delivered); returns their ids for the caller's changes.
pub async fn void_outstanding(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Vec<i64>> {
    Ok(sql
        .query(
            "UPDATE user_two_factor_sms_codes SET voided_at=now() WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL AND voided_at IS NULL RETURNING id",
            &[&tenant, &user],
        )
        .await?
        .iter()
        .map(|row| row.get(0))
        .collect())
}

/// Stores a sent code's hash and the number it goes to, voiding the codes
/// before it and forgetting rows older than a day. Returns the new row's id
/// and the voided ids.
pub async fn store(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    hash: &str,
    sent_to: &str,
    limits: Limits,
) -> Result<(i64, Vec<i64>)> {
    let voided = void_outstanding(sql, tenant, user).await?;
    sql.execute(
        "DELETE FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND sent_at<now()-interval '1 day'",
        &[&tenant, &user],
    )
    .await?;
    let id = sql
        .query_one(
            "INSERT INTO user_two_factor_sms_codes(tenant_id,user_id,code_hash,sent_to,expires_at) VALUES($1,$2,$3,$4,now()+make_interval(mins => $5::integer)) RETURNING id",
            &[&tenant, &user, &hash, &sent_to, &(limits.code_minutes as i32)],
        )
        .await?
        .get(0);
    Ok((id, voided))
}

/// Counts one wrong attempt on a code; returns its failures so far.
pub async fn count_failure(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<Option<i32>> {
    Ok(sql
        .query_opt(
            "UPDATE user_two_factor_sms_codes SET failures=failures+1 WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL AND voided_at IS NULL RETURNING failures",
            &[&id, &tenant, &user],
        )
        .await?
        .map(|row| row.get(0)))
}

/// What spending a proven code changed.
pub struct Spent {
    /// The code row was used (compare-and-set: a racing reuse loses).
    pub used: bool,
    /// This was the first correct code sent to the account's number.
    pub verified: bool,
}

/// Uses a proven code, only while the account still has the number it went
/// to: the account row is share-locked first, so a change of number either
/// commits before (and the code no longer counts) or waits for this
/// transaction. The first correct code sent to a number verifies it.
pub async fn spend(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
    attempts: i32,
) -> Result<Spent> {
    sql.query_one(
        "SELECT id FROM users WHERE tenant_id=$1 AND id=$2 FOR SHARE",
        &[&tenant, &user],
    )
    .await?;
    let sent_to: Option<String> = sql
        .query_opt(
            "UPDATE user_two_factor_sms_codes c SET used_at=now() FROM users u WHERE c.id=$1 AND c.tenant_id=$2 AND c.user_id=$3 AND u.tenant_id=c.tenant_id AND u.id=c.user_id AND c.sent_to=u.phone_number AND c.used_at IS NULL AND c.voided_at IS NULL AND c.expires_at>now() AND c.failures<$4 RETURNING c.sent_to",
            &[&id, &tenant, &user, &attempts],
        )
        .await?
        .map(|row| row.get(0));
    let Some(sent_to) = sent_to else {
        return Ok(Spent {
            used: false,
            verified: false,
        });
    };
    let verified = sql
        .execute(
            "UPDATE users SET phone_verified_at=now() WHERE tenant_id=$1 AND id=$2 AND phone_number=$3 AND phone_verified_at IS NULL",
            &[&tenant, &user, &sent_to],
        )
        .await?
        == 1;
    Ok(Spent {
        used: true,
        verified,
    })
}

/// The bare host of the public origin, for the origin-bound code line
/// (`https://erp.example.com:8443/` gives `erp.example.com`).
pub fn origin_host(origin: &str) -> &str {
    let rest = origin.split_once("://").map_or(origin, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or(rest);
    match authority.rsplit_once(':') {
        Some((host, port)) if port.bytes().all(|b| b.is_ascii_digit()) => host,
        _ => authority,
    }
}
