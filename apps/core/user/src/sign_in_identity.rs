//! Platform sign-ins, mirroring Bilimbi's account credential ownership while
//! keeping Bilibi's tenant account as the actor. A staff identity may later
//! own several tenant memberships; a customer identity remains tenant-bound.
use bilibi_database::{Change, Error, Result};
use bilibi_tenancy::AccountKind;
use serde_json::json;
use tokio_postgres::GenericClient;

pub(crate) struct Created {
    pub identity: i64,
    pub user: i64,
    pub change: Change,
}

/// Reserves the home account id and creates its credential identity. The
/// caller inserts the account in the same transaction; both circular foreign
/// keys are deferred until commit, so neither half can survive alone.
pub(crate) async fn create(
    sql: &(impl GenericClient + Sync),
    tenant: i64,
    kind: AccountKind,
    email: &str,
    password_hash: &str,
    verified: bool,
) -> Result<Created> {
    let user: i64 = sql
        .query_one(
            "SELECT nextval(pg_get_serial_sequence('users','id'))::bigint",
            &[],
        )
        .await?
        .get(0);
    let identity: i64 = sql
        .query_one(
            "INSERT INTO user_identities(kind,tenant_id,email,password_hash,email_verified_at,home_tenant_id,home_user_id,last_tenant_id) VALUES($1,CASE WHEN $1='customer' THEN $2::bigint END,$3,$4,CASE WHEN $5 THEN now() END,$2,$6,$2) RETURNING id",
            &[&kind.as_str(), &tenant, &email, &password_hash, &verified, &user],
        )
        .await?
        .get(0);
    Ok(Created {
        identity,
        user,
        change: Change {
            kind: "user_identity".into(),
            id: identity,
            old: json!(null),
            new: json!({
                "kind": kind.as_str(),
                "email": email,
                "password_hash": "set",
                "home_tenant_id": tenant,
                "home_user_id": user,
            }),
        },
    })
}

pub(crate) async fn id(sql: &(impl GenericClient + Sync), tenant: i64, user: i64) -> Result<i64> {
    sql.query_opt(
        "SELECT identity_id FROM users WHERE tenant_id=$1 AND id=$2",
        &[&tenant, &user],
    )
    .await?
    .map(|row| row.get(0))
    .ok_or(Error::NotFound)
}
