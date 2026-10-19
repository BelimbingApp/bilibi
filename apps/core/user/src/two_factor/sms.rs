//! Text-message codes, the second factor of an account with a phone number
//! and no authenticator app. A code is 6 digits from the operating system's
//! CSPRNG, sent through Base SMS, stored only as an Argon2id hash, good for
//! [`CODE_SECONDS`], used once and voided by its [`CODE_FAILURES`]th wrong
//! attempt or by a newer code. Sends are throttled per account: one per
//! [`RESEND_SECONDS`], at most [`SENDS_PER_HOUR`] an hour. A 6-digit code
//! behind Argon2id, the expiry and these limits is out of reach of guessing.
use bilibi_database::{Error, Result};
use tokio_postgres::GenericClient;

pub const DIGITS: usize = 6;
pub const CODE_SECONDS: i64 = 300;
pub const CODE_FAILURES: i32 = 5;
pub const RESEND_SECONDS: i64 = 60;
pub const SENDS_PER_HOUR: i64 = 5;

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
    pub async fn read(sql: &impl GenericClient, tenant: i64, user: i64) -> Result<Self> {
        let row = sql
            .query_one(
                "SELECT EXISTS(SELECT 1 FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL AND voided_at IS NULL AND expires_at>now() AND failures<$3), \
                 COALESCE(ceil(extract(epoch FROM max(sent_at)+make_interval(secs => $4::double precision)-now()))::bigint,0), \
                 count(*) FILTER (WHERE sent_at>now()-interval '1 hour'), \
                 COALESCE(ceil(extract(epoch FROM min(sent_at) FILTER (WHERE sent_at>now()-interval '1 hour')+interval '1 hour'-now()))::bigint,0) \
                 FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2",
                &[&tenant, &user, &CODE_FAILURES, &(RESEND_SECONDS as f64)],
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

/// The newest code that can still be entered, as `(id, hash)`.
pub async fn active(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
) -> Result<Option<(i64, String)>> {
    Ok(sql
        .query_opt(
            "SELECT id,code_hash FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL AND voided_at IS NULL AND expires_at>now() AND failures<$3 ORDER BY id DESC LIMIT 1",
            &[&tenant, &user, &CODE_FAILURES],
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

/// Voids every code still outstanding (a newer code, a new number).
pub async fn void_outstanding(sql: &impl GenericClient, tenant: i64, user: i64) -> Result<()> {
    sql.execute(
        "UPDATE user_two_factor_sms_codes SET voided_at=now() WHERE tenant_id=$1 AND user_id=$2 AND used_at IS NULL AND voided_at IS NULL",
        &[&tenant, &user],
    )
    .await?;
    Ok(())
}

/// Stores a sent code's hash, voiding the ones before it and forgetting
/// rows older than a day. Returns the new row's id.
pub async fn store(sql: &impl GenericClient, tenant: i64, user: i64, hash: &str) -> Result<i64> {
    void_outstanding(sql, tenant, user).await?;
    sql.execute(
        "DELETE FROM user_two_factor_sms_codes WHERE tenant_id=$1 AND user_id=$2 AND sent_at<now()-interval '1 day'",
        &[&tenant, &user],
    )
    .await?;
    Ok(sql
        .query_one(
            "INSERT INTO user_two_factor_sms_codes(tenant_id,user_id,code_hash,expires_at) VALUES($1,$2,$3,now()+make_interval(secs => $4::double precision)) RETURNING id",
            &[&tenant, &user, &hash, &(CODE_SECONDS as f64)],
        )
        .await?
        .get(0))
}

/// Counts one wrong attempt on a code; returns its failures so far.
pub async fn count_failure(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    id: i64,
) -> Result<i32> {
    Ok(sql
        .query_opt(
            "UPDATE user_two_factor_sms_codes SET failures=failures+1 WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL AND voided_at IS NULL RETURNING failures",
            &[&id, &tenant, &user],
        )
        .await?
        .map_or(CODE_FAILURES, |row| row.get(0)))
}

/// Uses a proven code (compare-and-set, so a racing reuse loses) and marks
/// the number verified the first time a code sent to it is entered.
pub async fn spend(sql: &impl GenericClient, tenant: i64, user: i64, id: i64) -> Result<u64> {
    let spent = sql
        .execute(
            "UPDATE user_two_factor_sms_codes SET used_at=now() WHERE id=$1 AND tenant_id=$2 AND user_id=$3 AND used_at IS NULL AND voided_at IS NULL AND expires_at>now() AND failures<$4",
            &[&id, &tenant, &user, &CODE_FAILURES],
        )
        .await?;
    if spent == 1 {
        sql.execute(
            "UPDATE users SET phone_verified_at=now() WHERE tenant_id=$1 AND id=$2 AND phone_number IS NOT NULL AND phone_verified_at IS NULL",
            &[&tenant, &user],
        )
        .await?;
    }
    Ok(spent)
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
