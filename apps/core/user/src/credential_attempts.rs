//! Failed credential checks, one counter per account and kind
//! (`user_credential_attempts`), with the one window/lock rule both kinds
//! share: failures are counted in a window, and the one that reaches the
//! limit locks that kind for that account. Counts are kept apart per kind,
//! so guessing at the sign-in form cannot block the signed-in owner's own
//! two-factor checks. A failure while locked is not counted and never extends
//! the lock. Sign-in's lock grows with each lock in a row (one minute, then
//! two, four, ... up to its limit); two-factor keeps its fixed lock. Callers
//! write inside their own audited transaction and record the change.
use bilibi_database::{Change, Result};
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

/// What is being checked.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Kind {
    /// A password on the sign-in form.
    SignIn,
    /// A two-factor check: password re-entry or an authenticator code.
    StepUp,
}
impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Self::SignIn => "sign_in",
            Self::StepUp => "step_up",
        }
    }
}

/// The window/lock rule's numbers for one kind.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    pub max_failures: i64,
    pub window_seconds: f64,
    /// The lock, or with `growing` its longest step.
    pub lock_seconds: f64,
    /// Lock for one minute, doubling with each lock in a row, up to
    /// `lock_seconds`.
    pub growing: bool,
}

/// Seconds left on the account's lock for `kind`, if it is locked.
pub(crate) async fn locked(
    client: &impl GenericClient,
    tenant: i64,
    user: i64,
    kind: Kind,
) -> Result<Option<i64>> {
    Ok(client
        .query_opt(
            "SELECT ceil(extract(epoch FROM a.locked_until-now()))::bigint FROM user_credential_attempts a JOIN users u ON u.identity_id=a.identity_id WHERE u.tenant_id=$1 AND u.id=$2 AND a.kind=$3 AND a.locked_until>now()",
            &[&tenant, &user, &kind.as_str()],
        )
        .await?
        .map(|row| row.get(0)))
}

/// Counts one failure in the window. `None` while locked: nothing is counted
/// and the lock stays as it was.
pub(crate) async fn count(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    kind: Kind,
    limits: Limits,
) -> Result<Option<i64>> {
    Ok(sql
        .query_opt(
            "INSERT INTO user_credential_attempts AS a(identity_id,kind,failures) SELECT identity_id,$3,1 FROM users WHERE tenant_id=$1 AND id=$2 ON CONFLICT ON CONSTRAINT user_credential_attempts_pkey DO UPDATE SET failures=CASE WHEN a.window_started_at<=now()-make_interval(secs => $4::double precision) THEN 1 ELSE a.failures+1 END, window_started_at=CASE WHEN a.window_started_at<=now()-make_interval(secs => $4::double precision) THEN now() ELSE a.window_started_at END WHERE a.locked_until IS NULL OR a.locked_until<=now() RETURNING failures",
            &[&tenant, &user, &kind.as_str(), &limits.window_seconds],
        )
        .await?
        .map(|row| i64::from(row.get::<_, i32>(0))))
}

/// Locks `kind` for the account and starts a new count; returns when the
/// lock ends and how many whole minutes it lasts.
pub(crate) async fn lock(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    kind: Kind,
    limits: Limits,
) -> Result<(String, i64)> {
    let row = sql
        .query_one(
            "UPDATE user_credential_attempts SET failures=0,window_started_at=now(),locks=locks+1,locked_until=now()+make_interval(secs => CASE WHEN $5 THEN least($4::double precision, 60*power(2::double precision, least(locks, 20))) ELSE $4::double precision END) WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND kind=$3 RETURNING locked_until::text, ceil(extract(epoch FROM locked_until-now())/60)::bigint",
            &[&tenant, &user, &kind.as_str(), &limits.lock_seconds, &limits.growing],
        )
        .await?;
    Ok((row.get(0), row.get(1)))
}

/// After a passed check: the count starts again. A lock still running stays.
pub(crate) async fn reset_count(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    kind: Kind,
) -> Result<()> {
    sql.execute(
        "UPDATE user_credential_attempts SET failures=0,window_started_at=now() WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND kind=$3 AND failures>0",
        &[&tenant, &user, &kind.as_str()],
    )
    .await?;
    Ok(())
}

/// Forgets the count, any lock and the run of locks (a successful sign-in, a
/// password reset, an administrator's unlock). True when there was a row.
pub(crate) async fn forget(
    sql: &impl GenericClient,
    tenant: i64,
    user: i64,
    kind: Kind,
) -> Result<bool> {
    Ok(sql
        .execute(
            "DELETE FROM user_credential_attempts WHERE identity_id=(SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2) AND kind=$3",
            &[&tenant, &user, &kind.as_str()],
        )
        .await?
        > 0)
}

/// The audited change for the account's `kind` counter.
pub(crate) fn change(user: i64, kind: Kind, old: Value, mut new: Value) -> Change {
    if let Some(object) = new.as_object_mut() {
        object.insert("kind".into(), json!(kind.as_str()));
    }
    Change {
        kind: "user_credential_attempts".into(),
        id: user,
        old,
        new,
    }
}
