//! A person's OAuth grants, never a second identity. Codes and tokens are stored
//! only as SHA-256 digests. Row locks serialize approval, redemption, rotation,
//! replay and revocation; spent refresh digests remain so a replay ends the
//! grant. Authentication is proven afresh at the Web edge: this module returns
//! claims, not an authenticated scope. A stated agent ID labels a request and
//! never authenticates it. The granting account and its party facets are
//! reproved through the injected Tenancy providers on every issuance
//! (`base/tenancy.authentication`, `base/tenancy.parties`); this module runs no
//! SQL against Core account, company or person tables.
mod contributions;
mod oauth_client;
mod secrets;
mod throttle;
pub mod web;
pub use contributions::MANAGE;
pub use oauth_client::{proof, valid_clients, AuthorizationRequest, AuthorizeParams, PublicClient};
/// `code_challenge` is the S256 form a public client sends for its verifier.
pub use secrets::{digest_base64 as code_challenge, normalize_user_code};
pub use throttle::Throttle;

use bilibi_database::{
    Change, Context, Db, Error, MutationBoundary, MutationTransaction, Principal, Result,
};
use bilibi_party_ids::{OrganisationId, PersonId};
use bilibi_settings::Settings;
use bilibi_tenancy::{Authentication, PartyProofs, ProofClient, Scope};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::Arc, time::SystemTime};
use tokio_postgres::{Row, Transaction};

/// The built-in public client: no secret and no installation registration.
pub const CLIENT_ID: &str = "bilibi-native";
pub const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";
pub const OAUTH_SCOPE: &str = "bilibi";
/// Protocol contracts, not operator settings.
pub const DEVICE_SECONDS: i64 = 600;
pub const POLL_INTERVAL_SECONDS: i32 = 5;
pub const SLOW_DOWN_SECONDS: i32 = 5;
pub const ACCESS_TOKEN_SECONDS: i64 = 3600;
pub const SEEN_LIMIT: usize = 20;
pub const UNSTATED: &str = "(not stated)";
pub const CONNECTION_DAYS: &str = "agent_access.connection_days";
pub const OAUTH_CLIENTS: &str = "agent_access.oauth_clients";
pub const AUTHORIZATION_CODE_SECONDS: &str = "agent_access.authorization_code_seconds";
pub const OAUTH_REQUESTS_PER_MINUTE: &str = "agent_access.oauth_requests_per_minute";
pub const MIGRATION: (&str, &str) = (
    "core-agent-access-001",
    include_str!("../migrations/001-agent-access.sql"),
);

/// OAuth error codes (RFC 6749, RFC 8628, RFC 6750) plus the consent-page
/// refusals. `Display` is the wire code; `Other` keeps the storage failure.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    #[error("invalid_request")]
    InvalidRequest,
    #[error("invalid_client")]
    InvalidClient,
    #[error("invalid_grant")]
    InvalidGrant,
    #[error("authorization_pending")]
    AuthorizationPending,
    #[error("slow_down")]
    SlowDown,
    #[error("expired_token")]
    ExpiredToken,
    #[error("access_denied")]
    AccessDenied,
    #[error("invalid_redirect_uri")]
    InvalidRedirectUri,
    #[error("unsupported_response_type")]
    UnsupportedResponseType,
    #[error("invalid_target")]
    InvalidTarget,
    #[error("invalid_scope")]
    InvalidScope,
    #[error("invalid_token")]
    InvalidToken,
    #[error("invalid_user_code")]
    InvalidUserCode,
    #[error("invalid_expiry")]
    InvalidExpiry,
    #[error("person_required")]
    PersonRequired,
    #[error("not_found")]
    NotFound,
    #[error(transparent)]
    Other(#[from] Error),
}
impl From<tokio_postgres::Error> for OAuthError {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::from(error).into()
    }
}
impl From<deadpool_postgres::PoolError> for OAuthError {
    fn from(error: deadpool_postgres::PoolError) -> Self {
        Error::from(error).into()
    }
}
/// A refused identity proof is an invalid grant; storage failures stay visible.
fn refused(error: Error) -> OAuthError {
    match error {
        Error::Unauthorized | Error::Forbidden | Error::NotFound | Error::Invalid => {
            OAuthError::InvalidGrant
        }
        other => OAuthError::Other(other),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Decision {
    Approve,
    Deny,
}
/// The raw codes leave the service only here.
#[derive(Clone, Debug)]
pub struct DeviceStart {
    pub device_code: String,
    pub user_code: String,
    pub expires_in: i64,
    pub interval: i32,
}
/// What the consent page shows for a pending user code.
#[derive(Clone, Debug)]
pub struct DeviceReview {
    pub agent_id: String,
    pub expires_at: SystemTime,
}
/// The raw tokens leave the service only here.
#[derive(Clone, Debug)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: &'static str,
    pub expires_in: i64,
    pub scope: Option<String>,
}
/// Stable claims of a live access token, before any tenant scope exists. The
/// host's agent authentication (E5) seals these into a scope with an agent
/// facet; they are never a scope themselves.
#[derive(Clone, Debug)]
pub struct AgentClaims {
    pub connection: i64,
    pub tenant: i64,
    pub user: i64,
    pub organisation: Option<OrganisationId>,
    pub person: Option<PersonId>,
}
#[derive(Clone, Debug)]
pub struct ConnectionSummary {
    pub id: i64,
    pub agent_id: String,
    /// Other IDs this connection stated, with the UTC time each was first seen.
    pub seen_agent_ids: BTreeMap<String, String>,
    pub expires_at: SystemTime,
    pub revoked_at: Option<SystemTime>,
}
/// Form fields of a PKCE code redemption, as the host decodes them.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default)]
pub struct CodeExchange {
    pub code: String,
    pub client_id: String,
    pub redirect_uri: String,
    pub code_verifier: String,
    pub resource: String,
}

/// `core/agent_access.grants`: the owner API behind the device, consent,
/// token and connection pages and the host OAuth endpoints.
pub struct AgentAccess {
    db: Db,
    mutations: Arc<MutationBoundary>,
    settings: Arc<Settings>,
    authentication: Arc<Authentication>,
    parties: Arc<PartyProofs>,
}

struct Connection {
    id: i64,
    tenant: i64,
    user: i64,
    organisation: Option<i64>,
    person: Option<i64>,
    agent_id: String,
    client_id: String,
    resource: Option<String>,
    oauth_scope: Option<String>,
    seen: Value,
    expires_at: SystemTime,
    live: bool,
}
const CONNECTION_COLUMNS: &str = "id,tenant_id,user_id,organisation_id,person_id,agent_id,client_id,resource,oauth_scope,seen_agent_ids,expires_at,revoked_at IS NULL AND expires_at>clock_timestamp() AS live";
impl Connection {
    fn from_row(row: &Row) -> Self {
        Self {
            id: row.get("id"),
            tenant: row.get("tenant_id"),
            user: row.get("user_id"),
            organisation: row.get("organisation_id"),
            person: row.get("person_id"),
            agent_id: row.get("agent_id"),
            client_id: row.get("client_id"),
            resource: row.get("resource"),
            oauth_scope: row.get("oauth_scope"),
            seen: row.get("seen_agent_ids"),
            expires_at: row.get("expires_at"),
            live: row.get("live"),
        }
    }
    /// Token requests have no session: the account acts through its grant.
    fn agent_context(&self, action: &str) -> Context {
        Context {
            tenant: self.tenant,
            actor: Principal {
                kind: "agent".into(),
                id: self.user.to_string(),
            },
            impersonator: None,
            action: action.into(),
            trace_id: uuid::Uuid::new_v4().to_string(),
        }
    }
    fn seen_map(&self) -> BTreeMap<String, String> {
        serde_json::from_value(self.seen.clone()).unwrap_or_default()
    }
    fn knows(&self, label: &str) -> bool {
        label == self.agent_id || self.seen.get(label).is_some()
    }
}
fn change(kind: &str, id: i64, old: Value, new: Value) -> Change {
    Change {
        kind: kind.into(),
        id,
        old,
        new,
    }
}
/// A label is self-reported and grants no authority; it only has to be showable.
pub fn valid_label(label: &str) -> bool {
    (1..=255).contains(&label.len())
        && !label.trim().is_empty()
        && !label.chars().any(char::is_control)
}
/// Bilimbi refuses impersonated sessions and agent facets from managing
/// connections: a bearer request carries the agent facet, an impersonated
/// browser session its operator. Refused here once, not at each caller.
fn self_service(scope: &Scope) -> std::result::Result<(), OAuthError> {
    if scope.agent().is_some() || scope.impersonator().is_some() {
        return Err(OAuthError::AccessDenied);
    }
    Ok(())
}
fn future(expires_at: SystemTime) -> std::result::Result<(), OAuthError> {
    if expires_at > SystemTime::now() {
        Ok(())
    } else {
        Err(OAuthError::InvalidExpiry)
    }
}
fn secret_shaped(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

impl AgentAccess {
    /// End time displayed for a new connection, from the global operator setting.
    pub async fn connection_expiry(&self) -> Result<SystemTime> {
        let client = self.db.get().await?;
        let days = self
            .settings
            .get_global(&**client, CONNECTION_DAYS)
            .await?
            .as_i64()
            .ok_or(Error::Invalid)?;
        Ok(client
            .query_one(
                "SELECT date_trunc('second', now()+make_interval(days => $1::int))",
                &[&(days as i32)],
            )
            .await?
            .get(0))
    }
    /// Validates an operator-registered authorization-code request before
    /// displaying consent.
    pub async fn authorization_request(
        &self,
        params: &AuthorizeParams,
        resource: &str,
    ) -> std::result::Result<AuthorizationRequest, OAuthError> {
        let client = self.db.get().await?;
        let clients = self.settings.get_global(&**client, OAUTH_CLIENTS).await?;
        oauth_client::request(&clients, params, resource)
    }
    /// Creates a person-owned MCP grant only after explicit browser consent,
    /// returning the one-time authorization code for the registered callback.
    pub async fn authorize(
        &self,
        scope: &Scope,
        params: &AuthorizeParams,
        resource: &str,
        expires_at: SystemTime,
    ) -> std::result::Result<String, OAuthError> {
        self_service(scope)?;
        let request = self.authorization_request(params, resource).await?;
        future(expires_at)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("agent_access.connection.authorize"),
            )
            .await?;
        let code_seconds = self
            .settings
            .get_global(tx.sql(), AUTHORIZATION_CODE_SECONDS)
            .await?
            .as_f64()
            .ok_or(Error::Invalid)?;
        let code = secrets::secret()?;
        let code_hash = secrets::digest(&code);
        let (tenant, user) = (scope.tenant(), scope.user());
        let organisation = scope.actor().organisation().map(OrganisationId::get);
        let person = scope.actor().person().map(PersonId::get);
        let resource = resource.to_owned();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let row = sql.query_one(
                    "INSERT INTO core_agent_connections(tenant_id,user_id,organisation_id,person_id,agent_id,client_id,resource,oauth_scope,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,date_trunc('second',$9::timestamptz)) RETURNING id,expires_at::text",
                    &[&tenant,&user,&organisation,&person,&request.client_name,&request.client_id,&resource,&OAUTH_SCOPE,&expires_at],
                ).await?;
                let connection: i64 = row.get(0);
                let auth = sql.query_one(
                    "INSERT INTO core_agent_authorization_codes(connection_id,code_hash,code_challenge,redirect_uri,expires_at) VALUES($1,$2,$3,$4,now()+make_interval(secs => $5::double precision)) RETURNING id,expires_at::text",
                    &[&connection,&code_hash,&request.code_challenge,&request.redirect_uri,&code_seconds],
                ).await?;
                Ok(((), vec![
                    change("agent_connection", connection, json!({}), json!({"agent_id":request.client_name,"client_id":request.client_id,"resource":resource,"oauth_scope":OAUTH_SCOPE,"expires_at":row.get::<_,String>(1)})),
                    change("agent_authorization_code", auth.get(0), json!({}), json!({"connection_id":connection,"redirect_uri":request.redirect_uri,"expires_at":auth.get::<_,String>(1)})),
                ]))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(code)
    }
    /// Redeems a short-lived code once, bound to client, callback, resource and
    /// the S256 verifier. A second redemption revokes the whole connection.
    pub async fn exchange_code(
        &self,
        params: &CodeExchange,
        resource: &str,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        if !secret_shaped(&params.code) {
            return Err(OAuthError::InvalidGrant);
        }
        let mut client = self.db.get().await?;
        let found = client
            .query_opt(
                "SELECT id,connection_id FROM core_agent_authorization_codes WHERE code_hash=$1",
                &[&secrets::digest(&params.code)],
            )
            .await?
            .ok_or(OAuthError::InvalidGrant)?;
        let (code_id, connection_id): (i64, i64) = (found.get(0), found.get(1));
        let unlocked = self.connection(&**client, connection_id, false).await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                unlocked.agent_context("agent_access.code.exchange"),
            )
            .await?;
        let connection = self.connection(tx.sql(), connection_id, true).await?;
        let auth = tx
            .sql()
            .query_one(
                "SELECT code_challenge,redirect_uri,expires_at>now() AS live,used_at IS NOT NULL AS used FROM core_agent_authorization_codes WHERE id=$1 FOR UPDATE",
                &[&code_id],
            )
            .await?;
        let clients = self.settings.get_global(tx.sql(), OAUTH_CLIENTS).await?;
        let redirect: String = auth.get("redirect_uri");
        let bound = oauth_client::registered_redirect(&clients, &connection.client_id, &redirect)
            && connection.client_id == params.client_id
            && connection.resource.as_deref() == Some(resource)
            && params.resource == resource
            && redirect == params.redirect_uri
            && oauth_client::proof(&params.code_verifier, auth.get("code_challenge"))
            && auth.get::<_, bool>("live");
        if !bound {
            tx.rollback().await?;
            return Err(OAuthError::InvalidGrant);
        }
        if auth.get::<_, bool>("used") {
            Self::revoke_connection(&mut tx, &connection, "authorization code replayed").await?;
            tx.commit().await?;
            return Err(OAuthError::InvalidGrant);
        }
        self.live(tx.sql(), &connection).await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE core_agent_authorization_codes SET used_at=now() WHERE id=$1",
                    &[&code_id],
                )
                .await?;
                Ok((
                    (),
                    vec![change(
                        "agent_authorization_code",
                        code_id,
                        json!({"used":false}),
                        json!({"used":true}),
                    )],
                ))
            })
        })
        .await?;
        let tokens = self.issue(&mut tx, &connection).await?;
        tx.commit().await?;
        Ok(tokens)
    }

    /// Starts device authorization for the built-in client. The receipt knows
    /// no tenant yet; it is a pre-authentication credential like a reset token.
    pub async fn start_device(
        &self,
        client_id: &str,
        agent_id: &str,
    ) -> std::result::Result<DeviceStart, OAuthError> {
        if client_id != CLIENT_ID {
            return Err(OAuthError::InvalidClient);
        }
        if !valid_label(agent_id) {
            return Err(OAuthError::InvalidRequest);
        }
        let device_code = secrets::secret()?;
        let user_code = secrets::user_code()?;
        let client = self.db.get().await?;
        client.execute(
            "INSERT INTO core_agent_devices(device_hash,user_hash,client_id,agent_id,expires_at,next_poll_at,interval,status) VALUES($1,$2,$3,$4,now()+make_interval(secs => $5::double precision),now(),$6,'pending')",
            &[&secrets::digest(&device_code),&secrets::digest(&user_code),&CLIENT_ID,&agent_id,&(DEVICE_SECONDS as f64),&POLL_INTERVAL_SECONDS],
        ).await?;
        Ok(DeviceStart {
            device_code,
            user_code,
            expires_in: DEVICE_SECONDS,
            interval: POLL_INTERVAL_SECONDS,
        })
    }
    /// What a signed-in person reviews for a pending user code.
    pub async fn device(
        &self,
        scope: &Scope,
        user_code: &str,
    ) -> std::result::Result<DeviceReview, OAuthError> {
        self_service(scope)?;
        let client = self.db.get().await?;
        let row = client
            .query_opt(
                "SELECT agent_id,expires_at FROM core_agent_devices WHERE user_hash=$1 AND status='pending' AND expires_at>now()",
                &[&secrets::digest(user_code)],
            )
            .await?
            .ok_or(OAuthError::InvalidUserCode)?;
        Ok(DeviceReview {
            agent_id: row.get(0),
            expires_at: row.get(1),
        })
    }
    /// Explicit approval creates the connection with the server-computed end
    /// time; refusal ends the receipt. Both are the person's audited action.
    pub async fn decide(
        &self,
        scope: &Scope,
        user_code: &str,
        decision: Decision,
        expires_at: SystemTime,
    ) -> std::result::Result<(), OAuthError> {
        self_service(scope)?;
        future(expires_at)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("agent_access.device.decide"),
            )
            .await?;
        let device = tx
            .sql()
            .query_opt(
                "SELECT id,agent_id,expires_at>now() AS live FROM core_agent_devices WHERE user_hash=$1 AND status='pending' FOR UPDATE",
                &[&secrets::digest(user_code)],
            )
            .await?;
        let Some(device) = device else {
            tx.rollback().await?;
            return Err(OAuthError::InvalidUserCode);
        };
        let (device_id, agent_id): (i64, String) = (device.get(0), device.get(1));
        if !device.get::<_, bool>("live") {
            tx.rollback().await?;
            return Err(OAuthError::ExpiredToken);
        }
        let (tenant, user) = (scope.tenant(), scope.user());
        let organisation = scope.actor().organisation().map(OrganisationId::get);
        let person = scope.actor().person().map(PersonId::get);
        tx.mutate(move |sql| {
            Box::pin(async move {
                if decision == Decision::Deny {
                    sql.execute(
                        "UPDATE core_agent_devices SET status='denied',updated_at=now() WHERE id=$1",
                        &[&device_id],
                    )
                    .await?;
                    return Ok((
                        (),
                        vec![change(
                            "agent_device",
                            device_id,
                            json!({"status":"pending"}),
                            json!({"status":"denied"}),
                        )],
                    ));
                }
                let row = sql.query_one(
                    "INSERT INTO core_agent_connections(tenant_id,user_id,organisation_id,person_id,agent_id,expires_at) VALUES($1,$2,$3,$4,$5,date_trunc('second',$6::timestamptz)) RETURNING id,expires_at::text",
                    &[&tenant,&user,&organisation,&person,&agent_id,&expires_at],
                ).await?;
                let connection: i64 = row.get(0);
                sql.execute(
                    "UPDATE core_agent_devices SET status='approved',connection_id=$2,updated_at=now() WHERE id=$1",
                    &[&device_id, &connection],
                )
                .await?;
                Ok((
                    (),
                    vec![
                        change("agent_connection", connection, json!({}), json!({"agent_id":agent_id,"client_id":CLIENT_ID,"expires_at":row.get::<_,String>(1)})),
                        change("agent_device", device_id, json!({"status":"pending"}), json!({"status":"approved","connection_id":connection})),
                    ],
                ))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(())
    }
    /// The client's poll. Pending, slow_down and refusals commit their state;
    /// an approved receipt is spent once and tokens are issued under audit.
    pub async fn exchange(
        &self,
        client_id: &str,
        device_code: &str,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        if client_id != CLIENT_ID {
            return Err(OAuthError::InvalidClient);
        }
        if !secret_shaped(device_code) {
            return Err(OAuthError::InvalidRequest);
        }
        let mut client = self.db.get().await?;
        // Receipt state is pre-authentication housekeeping; the lock serializes
        // concurrent polls so slow_down counts every early one.
        let tx = client.transaction().await?;
        let device = tx
            .query_opt(
                "SELECT id,status,expires_at>now() AS live,next_poll_at>now() AS early,interval,connection_id FROM core_agent_devices WHERE device_hash=$1 FOR UPDATE",
                &[&secrets::digest(device_code)],
            )
            .await?;
        let Some(device) = device else {
            tx.rollback().await?;
            return Err(OAuthError::InvalidGrant);
        };
        let (id, status): (i64, String) = (device.get(0), device.get(1));
        let connection_id: Option<i64> = device.get(5);
        let refusal = if !device.get::<_, bool>("live") {
            Some(OAuthError::ExpiredToken)
        } else if status == "denied" {
            Some(OAuthError::AccessDenied)
        } else if status == "used" {
            Some(OAuthError::InvalidGrant)
        } else if device.get::<_, bool>("early") {
            let interval = device.get::<_, i32>("interval") + SLOW_DOWN_SECONDS;
            tx.execute(
                "UPDATE core_agent_devices SET interval=$2::int,next_poll_at=now()+make_interval(secs => ($2::int)::double precision),updated_at=now() WHERE id=$1",
                &[&id, &interval],
            )
            .await?;
            Some(OAuthError::SlowDown)
        } else if status == "pending" {
            tx.execute(
                "UPDATE core_agent_devices SET next_poll_at=now()+make_interval(secs => interval::double precision),updated_at=now() WHERE id=$1",
                &[&id],
            )
            .await?;
            Some(OAuthError::AuthorizationPending)
        } else {
            None
        };
        tx.commit().await?;
        if let Some(error) = refusal {
            return Err(error);
        }
        let connection_id = connection_id.ok_or(OAuthError::InvalidGrant)?;
        let unlocked = self.connection(&**client, connection_id, false).await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                unlocked.agent_context("agent_access.device.exchange"),
            )
            .await?;
        // Lock the parent before the receipt everywhere, so refresh and
        // redemption never invert lock order.
        let connection = self.connection(tx.sql(), connection_id, true).await?;
        self.live(tx.sql(), &connection).await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                // Spending the receipt is the single-use claim; a racing poll loses.
                let spent = sql
                    .execute(
                        "UPDATE core_agent_devices SET status='used',updated_at=now() WHERE id=$1 AND status='approved' AND expires_at>now()",
                        &[&id],
                    )
                    .await?;
                if spent != 1 {
                    return Err(Error::NotFound);
                }
                Ok((
                    (),
                    vec![change(
                        "agent_device",
                        id,
                        json!({"status":"approved"}),
                        json!({"status":"used"}),
                    )],
                ))
            })
        })
        .await
        .map_err(refused)?;
        let tokens = self.issue(&mut tx, &connection).await?;
        tx.commit().await?;
        Ok(tokens)
    }
    /// Rotates a refresh token. A spent token presented again revokes the
    /// whole connection, and that revocation commits before the refusal.
    pub async fn refresh(
        &self,
        client_id: &str,
        refresh_token: &str,
        resource: Option<&str>,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        if !secret_shaped(refresh_token) {
            return Err(if client_id == CLIENT_ID {
                OAuthError::InvalidRequest
            } else {
                OAuthError::InvalidClient
            });
        }
        let mut client = self.db.get().await?;
        // Locate the immutable parent first, then lock parent before token.
        let found = client
            .query_opt(
                "SELECT id,connection_id FROM core_agent_tokens WHERE token_hash=$1 AND kind='refresh'",
                &[&secrets::digest(refresh_token)],
            )
            .await?
            .ok_or(OAuthError::InvalidGrant)?;
        let (token_id, connection_id): (i64, i64) = (found.get(0), found.get(1));
        let unlocked = self.connection(&**client, connection_id, false).await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                unlocked.agent_context("agent_access.token.refresh"),
            )
            .await?;
        let connection = self.connection(tx.sql(), connection_id, true).await?;
        let token = tx
            .sql()
            .query_one(
                "SELECT used_at IS NOT NULL AS used,expires_at>now() AS live FROM core_agent_tokens WHERE id=$1 FOR UPDATE",
                &[&token_id],
            )
            .await?;
        if connection.client_id != client_id || connection.resource.as_deref() != resource {
            tx.rollback().await?;
            return Err(OAuthError::InvalidGrant);
        }
        if client_id != CLIENT_ID {
            let clients = self.settings.get_global(tx.sql(), OAUTH_CLIENTS).await?;
            if oauth_client::client(&clients, client_id).is_none() {
                tx.rollback().await?;
                return Err(OAuthError::InvalidClient);
            }
        }
        if token.get::<_, bool>("used") {
            Self::revoke_connection(&mut tx, &connection, "refresh token replayed").await?;
            tx.commit().await?;
            return Err(OAuthError::InvalidGrant);
        }
        if !token.get::<_, bool>("live") {
            tx.rollback().await?;
            return Err(OAuthError::InvalidGrant);
        }
        self.live(tx.sql(), &connection).await?;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE core_agent_tokens SET used_at=now() WHERE id=$1",
                    &[&token_id],
                )
                .await?;
                Ok((
                    (),
                    vec![change(
                        "agent_token",
                        token_id,
                        json!({"used":false}),
                        json!({"used":true}),
                    )],
                ))
            })
        })
        .await?;
        let tokens = self.issue(&mut tx, &connection).await?;
        tx.commit().await?;
        Ok(tokens)
    }
    /// Opaque credential lookup before a tenant is known: the one unscoped
    /// connection read. Returns stable claims only, never a scope.
    pub async fn authenticate(
        &self,
        access_token: &str,
        resource: Option<&str>,
    ) -> std::result::Result<AgentClaims, OAuthError> {
        if !secret_shaped(access_token) {
            return Err(OAuthError::InvalidToken);
        }
        let client = self.db.get().await?;
        let row = client
            .query_opt(
                "SELECT c.id,c.tenant_id,c.user_id,c.organisation_id,c.person_id,c.resource,t.expires_at>now() AND c.revoked_at IS NULL AND c.expires_at>now() AS live FROM core_agent_tokens t JOIN core_agent_connections c ON c.id=t.connection_id WHERE t.token_hash=$1 AND t.kind='access'",
                &[&secrets::digest(access_token)],
            )
            .await?
            .ok_or(OAuthError::InvalidToken)?;
        if !row.get::<_, bool>("live")
            || row.get::<_, Option<String>>("resource").as_deref() != resource
        {
            return Err(OAuthError::InvalidToken);
        }
        let organisation = row
            .get::<_, Option<i64>>("organisation_id")
            .map(|id| OrganisationId::new(id).map_err(|_| Error::Internal))
            .transpose()?;
        let person = row
            .get::<_, Option<i64>>("person_id")
            .map(|id| PersonId::new(id).map_err(|_| Error::Internal))
            .transpose()?;
        Ok(AgentClaims {
            connection: row.get("id"),
            tenant: row.get("tenant_id"),
            user: row.get("user_id"),
            organisation,
            person,
        })
    }
    /// Live actor proof for one bearer request: the grant named by the claims
    /// must still be live in their tenant and account, the account must still
    /// authenticate and each recorded party facet must still be proven, all on
    /// the caller's transaction. Returns that fresh scope sealed with the agent
    /// facet. `lock` holds the grant row until the caller's commit, so a plan
    /// commit cannot race revocation. Expiry is read against the wall clock.
    pub async fn prove(
        &self,
        tx: &Transaction<'_>,
        claims: &AgentClaims,
        lock: bool,
        trace: &str,
    ) -> std::result::Result<Scope, OAuthError> {
        let unproven = |error| match error {
            OAuthError::InvalidGrant => OAuthError::InvalidToken,
            other => other,
        };
        let connection = self
            .own_connection(tx, claims, lock)
            .await
            .map_err(unproven)?;
        let scope = self.live(tx, &connection).await.map_err(unproven)?;
        Ok(scope.for_agent(connection.id, trace.to_owned()))
    }
    /// `prove` in its own read transaction, for a request with no other work
    /// to share it with.
    pub async fn seal(
        &self,
        claims: &AgentClaims,
        trace: &str,
    ) -> std::result::Result<Scope, OAuthError> {
        let mut client = self.db.get().await?;
        let tx = client.transaction().await?;
        let scope = self.prove(&tx, claims, false, trace).await?;
        tx.commit().await?;
        Ok(scope)
    }
    /// Records the identity an authenticated agent request stated for its
    /// claims. A request that stated none is recorded as `(not stated)`. A
    /// changed identity is flagged, never refused; only the newest
    /// `SEEN_LIMIT` distinct ones are kept.
    pub async fn observe(
        &self,
        claims: &AgentClaims,
        stated: Option<&str>,
    ) -> std::result::Result<(), OAuthError> {
        let label = match stated {
            Some(value) if valid_label(value) => value.to_owned(),
            Some(_) => return Err(OAuthError::InvalidRequest),
            None => UNSTATED.to_owned(),
        };
        let mut client = self.db.get().await?;
        let connection = self.own_connection(&**client, claims, false).await?;
        if connection.knows(&label) {
            return Ok(());
        }
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                connection.agent_context("agent_access.connection.observe"),
            )
            .await?;
        let connection = self.own_connection(tx.sql(), claims, true).await?;
        if connection.knows(&label) {
            tx.rollback().await?;
            return Ok(());
        }
        let old = connection.seen.clone();
        let mut entries: Vec<(String, String)> = connection.seen_map().into_iter().collect();
        entries.sort_by(|a, b| (&b.1, &b.0).cmp(&(&a.1, &a.0)));
        entries.truncate(SEEN_LIMIT - 1);
        let id = connection.id;
        tx.mutate(move |sql| {
            Box::pin(async move {
                let now: String = sql
                    .query_one("SELECT to_char(now() AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')", &[])
                    .await?
                    .get(0);
                entries.push((label, now));
                let seen = Value::Object(entries.into_iter().map(|(name, at)| (name, Value::String(at))).collect());
                sql.execute(
                    "UPDATE core_agent_connections SET seen_agent_ids=$2,updated_at=now() WHERE id=$1",
                    &[&id, &seen],
                )
                .await?;
                Ok(((), vec![change("agent_connection", id, json!({"seen_agent_ids":old}), json!({"seen_agent_ids":seen}))]))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(())
    }
    /// The operator-registered public clients: global, non-secret
    /// configuration for an administrative exporter. Registering a client
    /// is the only way onto this list; listing grants nothing.
    pub async fn public_clients(&self) -> Result<Vec<PublicClient>> {
        let client = self.db.get().await?;
        let clients = self.settings.get_global(&**client, OAUTH_CLIENTS).await?;
        Ok(oauth_client::public_clients(&clients))
    }
    /// The same list for a person setting up their own agent. Impersonation
    /// and agent facets are refused, as for every self-service decision.
    pub async fn clients_for(
        &self,
        scope: &Scope,
    ) -> std::result::Result<Vec<PublicClient>, OAuthError> {
        self_service(scope)?;
        Ok(self.public_clients().await?)
    }
    /// The person's own connections, newest first, including ended ones.
    pub async fn list_connections(
        &self,
        scope: &Scope,
    ) -> std::result::Result<Vec<ConnectionSummary>, OAuthError> {
        self_service(scope)?;
        let client = self.db.get().await?;
        let rows = client
            .query(
                "SELECT id,agent_id,seen_agent_ids,expires_at,revoked_at FROM core_agent_connections WHERE tenant_id=$1 AND user_id=$2 ORDER BY id DESC",
                &[&scope.tenant(), &scope.user()],
            )
            .await?;
        Ok(rows
            .iter()
            .map(|row| ConnectionSummary {
                id: row.get(0),
                agent_id: row.get(1),
                seen_agent_ids: serde_json::from_value(row.get(2)).unwrap_or_default(),
                expires_at: row.get(3),
                revoked_at: row.get(4),
            })
            .collect())
    }
    /// Ends one of the person's live connections and every token under it.
    pub async fn revoke(&self, scope: &Scope, id: i64) -> std::result::Result<(), OAuthError> {
        self_service(scope)?;
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                scope.mutation_context("agent_access.connection.revoke"),
            )
            .await?;
        let (tenant, user) = (scope.tenant(), scope.user());
        let revoked = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let revoked = sql
                        .execute(
                            "UPDATE core_agent_connections SET revoked_at=now(),updated_at=now() WHERE tenant_id=$1 AND user_id=$2 AND id=$3 AND revoked_at IS NULL",
                            &[&tenant, &user, &id],
                        )
                        .await?;
                    if revoked != 1 {
                        return Err(Error::NotFound);
                    }
                    Ok(((), vec![change("agent_connection", id, json!({"revoked":false}), json!({"revoked":true,"reason":"revoked by owner"}))]))
                })
            })
            .await;
        match revoked {
            Ok(()) => Ok(tx.commit().await?),
            Err(Error::NotFound) => {
                tx.rollback().await?;
                Err(OAuthError::NotFound)
            }
            Err(error) => Err(error.into()),
        }
    }

    async fn connection(
        &self,
        client: &impl tokio_postgres::GenericClient,
        id: i64,
        lock: bool,
    ) -> std::result::Result<Connection, OAuthError> {
        let sql = format!(
            "SELECT {CONNECTION_COLUMNS} FROM core_agent_connections WHERE id=$1{}",
            if lock { " FOR UPDATE" } else { "" }
        );
        client
            .query_opt(&sql, &[&id])
            .await?
            .map(|row| Connection::from_row(&row))
            .ok_or(OAuthError::InvalidGrant)
    }
    /// The grant named by sealed claims, in their tenant and account only.
    async fn own_connection(
        &self,
        client: &impl tokio_postgres::GenericClient,
        claims: &AgentClaims,
        lock: bool,
    ) -> std::result::Result<Connection, OAuthError> {
        let sql = format!(
            "SELECT {CONNECTION_COLUMNS} FROM core_agent_connections WHERE id=$1 AND tenant_id=$2 AND user_id=$3{}",
            if lock { " FOR UPDATE" } else { "" }
        );
        let connection = client
            .query_opt(&sql, &[&claims.connection, &claims.tenant, &claims.user])
            .await?
            .map(|row| Connection::from_row(&row))
            .ok_or(OAuthError::InvalidGrant)?;
        if !connection.live {
            return Err(OAuthError::InvalidGrant);
        }
        Ok(connection)
    }
    /// Token issuance has no caller scope. The grant must be live and its
    /// account must still authenticate as the same identity it consented
    /// with: the fresh actor's organisation and person facets must equal the
    /// recorded ones exactly, absent included, as Bilimbi binds a connection
    /// to the actor's company. Linking, unlinking or relinking the account
    /// therefore ends the grant's use. Each recorded facet must still be
    /// proven by its injected provider.
    async fn live(
        &self,
        client: &Transaction<'_>,
        connection: &Connection,
    ) -> std::result::Result<Scope, OAuthError> {
        if !connection.live {
            return Err(OAuthError::InvalidGrant);
        }
        let scope = self
            .authentication
            .authenticate(client, connection.tenant, connection.user, String::new())
            .await
            .map_err(refused)?;
        if scope.actor().organisation().map(OrganisationId::get) != connection.organisation
            || scope.actor().person().map(PersonId::get) != connection.person
        {
            return Err(OAuthError::InvalidGrant);
        }
        let proofs: &dyn ProofClient = client;
        if let Some(id) = connection.organisation {
            let id = OrganisationId::new(id).map_err(|_| OAuthError::InvalidGrant)?;
            self.parties
                .organisation(proofs, &scope, id)
                .await
                .map_err(refused)?;
        }
        if let Some(id) = connection.person {
            let id = PersonId::new(id).map_err(|_| OAuthError::InvalidGrant)?;
            self.parties
                .person(proofs, &scope, id)
                .await
                .map_err(refused)?;
        }
        Ok(scope)
    }
    async fn revoke_connection(
        tx: &mut MutationTransaction<'_>,
        connection: &Connection,
        reason: &'static str,
    ) -> std::result::Result<(), OAuthError> {
        let id = connection.id;
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "UPDATE core_agent_connections SET revoked_at=COALESCE(revoked_at,now()),updated_at=now() WHERE id=$1",
                    &[&id],
                )
                .await?;
                Ok(((), vec![change("agent_connection", id, json!({"revoked":false}), json!({"revoked":true,"reason":reason}))]))
            })
        })
        .await?;
        Ok(())
    }
    /// One access token bounded by an hour and the grant, one refresh token
    /// bounded by the grant. Digests are stored; the raw values are returned.
    async fn issue(
        &self,
        tx: &mut MutationTransaction<'_>,
        connection: &Connection,
    ) -> std::result::Result<TokenResponse, OAuthError> {
        let access_token = secrets::secret()?;
        let refresh_token = secrets::secret()?;
        let (access_hash, refresh_hash) = (
            secrets::digest(&access_token),
            secrets::digest(&refresh_token),
        );
        let (id, expires_at) = (connection.id, connection.expires_at);
        let expires_in = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let access = sql.query_one(
                        "INSERT INTO core_agent_tokens(connection_id,token_hash,kind,expires_at) VALUES($1,$2,'access',LEAST($3::timestamptz,date_trunc('second',now()+make_interval(secs => $4::double precision)))) RETURNING id,expires_at::text,GREATEST(0,floor(EXTRACT(EPOCH FROM expires_at-now())))::bigint",
                        &[&id,&access_hash,&expires_at,&(ACCESS_TOKEN_SECONDS as f64)],
                    ).await?;
                    let refresh = sql.query_one(
                        "INSERT INTO core_agent_tokens(connection_id,token_hash,kind,expires_at) VALUES($1,$2,'refresh',$3) RETURNING id,expires_at::text",
                        &[&id,&refresh_hash,&expires_at],
                    ).await?;
                    Ok((access.get::<_, i64>(2), vec![
                        change("agent_token", access.get(0), json!({}), json!({"connection_id":id,"kind":"access","expires_at":access.get::<_,String>(1)})),
                        change("agent_token", refresh.get(0), json!({}), json!({"connection_id":id,"kind":"refresh","expires_at":refresh.get::<_,String>(1)})),
                    ]))
                })
            })
            .await?;
        Ok(TokenResponse {
            access_token,
            refresh_token,
            token_type: "Bearer",
            expires_in,
            scope: connection.oauth_scope.clone(),
        })
    }
}

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/agent_access", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/database", "base/settings", "base/tenancy", "base/audit", "base/authz", "base/party_ids", "base/ui", "base/datetime", "base/locale", "core/user", "core/company"],
        migrations: &[MIGRATION],
        services: &[bilibi_module_registry::Service { id: "core/agent_access.grants", factory: |services| Ok(Arc::new(AgentAccess {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            mutations: services.get("base/database.mutations")?,
            settings: services.get("base/settings.store")?,
            authentication: services.get("base/tenancy.authentication")?,
            parties: services.get("base/tenancy.parties")?,
        })) }, bilibi_module_registry::Service { id: "core/agent_access.pages", factory: |services| Ok(Arc::new(bilibi_ui::page::PageService(Arc::new(web::Pages {
            access: services.get("core/agent_access.grants")?,
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            display: services.get("base/datetime.display")?,
            locales: services.get("base/locale.locales")?,
            origin: std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),
        })))) }],
    }
}
