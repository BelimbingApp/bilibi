//! Host agent edge: OAuth discovery, device authorization and token endpoints,
//! the bearer `AgentAuth` boundary and the `/api/v1` operation and plan
//! endpoints; `/mcp` (`mcp.rs`) shares this boundary and its recorder. Answers
//! are JSON only, errors included. Every request here is one incoming
//! Integration exchange, recorded after dispatch. Bodies, query strings,
//! authorization headers and token values never reach the recorder.
//!
//! A bearer token is a claim, never a grant: each request reproves the grant,
//! the account and its party facets (`AgentAccess::seal`), then seals a fresh
//! scope carrying the agent facet. Nothing is cached across requests.
use crate::{agent_targets, App};
use axum::{
    body::Body,
    extract::{ConnectInfo, FromRequest, Path, Query, Request, State},
    http::{header, HeaderMap, HeaderValue, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
    Json, Router,
};
use bilibi_base::{
    agent_api::{
        plans::{PlanAuthority, PlanIdentity, PlannedOperation, Plans},
        Kind, Registry, SearchOptions,
    },
    integration::{Incoming, Integration, Target},
    settings::Settings,
    tenancy::{ClientIdentity, ProofFuture, Scope, SelfReport},
};
use bilibi_core::agent_access::{
    AgentAccess, AgentClaims, ClientRegistration, CodeExchange, OAuthError, Throttle, CLIENT_ID,
    DEVICE_GRANT, OAUTH_REQUESTS_PER_MINUTE, OAUTH_SCOPE,
};
use bilibi_database::Error;
use serde_json::{json, Map, Value};
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::Instant,
};
use tokio_postgres::Transaction;

pub const AGENT_ID_HEADER: &str = "x-bilibi-agent-id";
const RUN_ID_HEADER: &str = "x-bilibi-run-id";
const TRACE_HEADER: &str = "x-bilibi-trace-id";
/// A plan may encode up to 1 MiB canonically; leave room for whitespace.
pub(crate) const API_BODY_LIMIT: usize = 2 << 20;

/// Host-owned paths no discovered page may claim.
pub fn reserved(path: &str) -> bool {
    path == "/api/v1"
        || path.starts_with("/api/v1/")
        || path.starts_with("/.well-known/")
        || path == "/scim"
        || path.starts_with("/scim/")
        || path == "/webhooks"
        || path.starts_with("/webhooks/")
        || [
            "/oauth/device_authorization",
            "/oauth/token",
            "/oauth/register",
            "/mcp",
        ]
        .contains(&path)
}

/// Services this edge composes; built once at boot from the registry.
pub struct Edge {
    pub(crate) access: Arc<AgentAccess>,
    pub(crate) registry: Arc<Registry>,
    pub(crate) plans: Arc<Plans>,
    /// Names the record kind an operation acts on, for the exchange row.
    pub(crate) records: Arc<bilibi_base::records::Records>,
    integration: Arc<Integration>,
    pub(crate) settings: Arc<Settings>,
    pub(crate) throttle: Throttle,
}
impl Edge {
    pub fn new(services: &bilibi_module_registry::Services) -> bilibi_database::Result<Self> {
        Ok(Self {
            access: services.get("core/agent_access.grants")?,
            registry: services.get("base/agent_api.registry")?,
            plans: services.get("base/agent_api.plans")?,
            records: services.get(bilibi_base::records::SERVICE)?,
            integration: services.get(bilibi_base::integration::SERVICE)?,
            settings: services.get("base/settings.store")?,
            throttle: Throttle::default(),
        })
    }
}

pub fn router(app: App) -> Router<App> {
    let api = Router::new()
        .route("/api/v1/search", get(search))
        .route("/api/v1/operations/{key}", get(describe))
        .route("/api/v1/guides/{key}", get(guide))
        .route("/api/v1/operations/{key}/call", post(call))
        .route("/api/v1/plans", post(plan))
        .route("/api/v1/plans/commit", post(commit))
        .route("/api/v1", any(not_found))
        .route("/api/v1/{*rest}", any(not_found));
    Router::new()
        .route("/.well-known/oauth-authorization-server", get(metadata))
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource),
        )
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route("/.well-known/bilibi", get(descriptor))
        .route("/oauth/device_authorization", post(device))
        .route("/oauth/token", post(token))
        .route("/oauth/register", post(register))
        .merge(api)
        .merge(crate::mcp::router())
        .method_not_allowed_fallback(method_not_allowed)
        .layer(middleware::from_fn_with_state(app, record))
}

/// One request's correlation ID and what it said about its agent, set before
/// dispatch.
#[derive(Clone, Default)]
pub(crate) struct Trace {
    pub(crate) id: String,
    pub(crate) reported: Reported,
    pub(crate) client_identity: Option<ClientIdentity>,
}
/// The self-report a request carried (ADR 01, "Agent identity at runtime").
/// A malformed value is dropped and only the fact is recorded; neither case
/// refuses the request, and nothing here is ever authority.
#[derive(Clone, Default)]
pub(crate) enum Reported {
    #[default]
    Absent,
    Valid(SelfReport),
    Malformed,
}
impl Reported {
    /// One header value; a repeated or non-text header is malformed.
    fn from_headers(headers: &HeaderMap) -> Self {
        let mut values = headers.get_all(SelfReport::HEADER).iter();
        match (values.next(), values.next()) {
            (None, _) => Self::Absent,
            (Some(value), None) => value.to_str().map_or(Self::Malformed, Self::parse),
            _ => Self::Malformed,
        }
    }
    /// An MCP request's `params._meta` entry; absent when not sent.
    pub(crate) fn from_meta(value: Option<&Value>) -> Self {
        match value {
            None => Self::Absent,
            Some(Value::String(value)) => Self::parse(value),
            Some(_) => Self::Malformed,
        }
    }
    fn parse(value: &str) -> Self {
        SelfReport::parse(value).map_or(Self::Malformed, Self::Valid)
    }
    pub(crate) fn report(&self) -> Option<&SelfReport> {
        match self {
            Self::Valid(report) => Some(report),
            Self::Absent | Self::Malformed => None,
        }
    }
}
/// What dispatch learned for the exchange row. The scope is the sealed bearer
/// scope; without one the row is a system row the operator alone can see.
/// `reported` replaces the header's self-report when the body carried one.
#[derive(Clone, Default)]
pub(crate) struct Exchange {
    pub(crate) scope: Option<Scope>,
    pub(crate) operation: Option<String>,
    pub(crate) reported: Option<Reported>,
    pub(crate) client_identity: Option<ClientIdentity>,
    /// What the request operated on (`agent_targets.rs`).
    pub(crate) targets: Vec<Target>,
}

async fn record(State(app): State<App>, mut request: Request, next: Next) -> Response {
    let started = Instant::now();
    let trace = uuid::Uuid::new_v4().simple().to_string();
    let method = request.method().to_string();
    let endpoint = request.uri().path().to_owned();
    let agent_id = label(request.headers(), AGENT_ID_HEADER);
    let run_id = label(request.headers(), RUN_ID_HEADER);
    let reported = Reported::from_headers(request.headers());
    let client_identity = client_identity(request.headers(), endpoint == "/mcp");
    request.extensions_mut().insert(Trace {
        id: trace.clone(),
        reported: reported.clone(),
        client_identity: client_identity.clone(),
    });
    let mut response = next.run(request).await;
    let exchange = response
        .extensions_mut()
        .remove::<Exchange>()
        .unwrap_or_default();
    let reported = exchange.reported.unwrap_or(reported);
    let incoming = Incoming {
        method: Some(method),
        operation: Some(
            exchange
                .operation
                .unwrap_or_else(|| endpoint.clone())
                .chars()
                .take(255)
                .collect(),
        ),
        endpoint: Some(endpoint),
        response_status: i16::try_from(response.status().as_u16()).ok(),
        duration_ms: Some(i32::try_from(started.elapsed().as_millis()).unwrap_or(i32::MAX)),
        connection_id: exchange
            .scope
            .as_ref()
            .and_then(Scope::agent)
            .map(|agent| agent.connection().to_string()),
        agent_id,
        run_id,
        agent_self_report: reported.report().map(|report| report.as_str().to_owned()),
        agent_self_report_malformed: matches!(reported, Reported::Malformed),
        client_identity: exchange
            .client_identity
            .or(client_identity)
            .map(|client| client.as_str().to_owned()),
        correlation_id: Some(trace.clone()),
        targets: exchange.targets,
        ..Incoming::default()
    };
    let integration = &app.api.integration;
    let recorded = match &exchange.scope {
        Some(scope) => integration.record_incoming(scope, &incoming).await,
        None => integration.record_incoming_system(&incoming).await,
    };
    if let Err(error) = recorded {
        eprintln!("warning: API exchange {trace} was not recorded: {error}");
    }
    if let Ok(value) = HeaderValue::from_str(&trace) {
        response.headers_mut().insert(TRACE_HEADER, value);
    }
    response
}

/// Transport evidence only: never infer a model from a harness User-Agent.
/// An omitted/malformed label is stated explicitly for every MCP exchange.
pub(crate) fn client_identity(headers: &HeaderMap, mcp: bool) -> Option<ClientIdentity> {
    let mut values = headers.get_all(header::USER_AGENT).iter();
    let client = match (values.next(), values.next()) {
        (Some(value), None) => value.to_str().ok().and_then(|value| {
            let value: String = value.chars().take(ClientIdentity::MAX_LEN - 12).collect();
            ClientIdentity::parse(&format!("User-Agent: {value}"))
        }),
        _ => None,
    };
    client.or_else(|| {
        mcp.then(|| ClientIdentity::parse("MCP: client identity not supplied"))
            .flatten()
    })
}

/// A self-reported label; an unshowable value is treated as not stated.
pub(crate) fn label(headers: &HeaderMap, name: &str) -> Option<String> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    (values.next().is_none() && bilibi_core::agent_access::valid_label(value))
        .then(|| value.to_owned())
}

/// Absolute links belong to the host; operations return paths. Every record
/// reference in an answer (an object with `kind`, `id` and a page `path`,
/// as `base/records` returns them) gains its `url`.
pub(crate) fn record_urls(origin: &str, value: &mut Value) {
    match value {
        Value::Object(map) => {
            let url = (map.contains_key("kind") && map.contains_key("id"))
                .then(|| map.get("path").and_then(Value::as_str))
                .flatten()
                .filter(|path| path.starts_with('/'))
                .map(|path| format!("{origin}{path}"));
            if let Some(url) = url {
                map.insert("url".into(), json!(url));
            }
            for value in map.values_mut() {
                record_urls(origin, value);
            }
        }
        Value::Array(items) => {
            for value in items {
                record_urls(origin, value);
            }
        }
        _ => {}
    }
}

/// What a plan or commit operated on: each operation with its input and, for
/// a commit, its committed entry (`results[i]`).
pub(crate) fn planned_targets(
    app: &App,
    operations: &[PlannedOperation],
    committed: Option<&Value>,
) -> Vec<Target> {
    operations
        .iter()
        .take(Target::MAX)
        .enumerate()
        .map(|(index, op)| {
            let done = committed.and_then(|answer| answer.pointer(&format!("/results/{index}")));
            agent_targets::write(&app.api.records, &op.key, &op.input, done)
        })
        .collect()
}

/// An operation, plan or commit answer with `record_urls` applied to its
/// body, a refusal's too (a rejected plan carries the previews of its other
/// operations), as `/mcp` does for every tool result.
fn with_record_urls(app: &App, result: Result<Value, Refusal>) -> Result<Value, Refusal> {
    match result {
        Ok(mut answer) => {
            record_urls(origin(app), &mut answer);
            Ok(answer)
        }
        Err(mut refusal) => {
            record_urls(origin(app), &mut refusal.body);
            Err(refusal)
        }
    }
}

pub(crate) fn reply(status: StatusCode, body: Value) -> Response {
    (status, Json(body)).into_response()
}
/// A JSON refusal; `challenge` adds the bearer `WWW-Authenticate` header.
pub(crate) struct Refusal {
    status: StatusCode,
    body: Value,
    challenge: bool,
}
impl IntoResponse for Refusal {
    fn into_response(self) -> Response {
        let mut response = reply(self.status, self.body);
        if self.challenge {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        response
    }
}
fn refusal(status: StatusCode, body: Value) -> Refusal {
    Refusal {
        status,
        body,
        challenge: false,
    }
}
pub(crate) fn error(status: StatusCode, code: &str) -> Refusal {
    refusal(status, json!({"error": code}))
}
async fn not_found() -> Refusal {
    error(StatusCode::NOT_FOUND, "not_found")
}
async fn method_not_allowed() -> Refusal {
    error(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed")
}
pub(crate) fn unavailable(error: &Error) -> Refusal {
    eprintln!("warning: agent edge storage failure: {error}");
    self::error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable")
}

pub(crate) fn origin(app: &App) -> &str {
    app.origin.trim_end_matches('/')
}
/// The MCP endpoint's protected-resource identifier, from `APP_ORIGIN`.
pub fn mcp_resource(app: &App) -> String {
    format!("{}/mcp", origin(app))
}

// --- OAuth discovery ---------------------------------------------------------

async fn metadata(State(app): State<App>) -> Response {
    let base = origin(&app);
    reply(
        StatusCode::OK,
        json!({
            "issuer": base,
            "device_authorization_endpoint": format!("{base}/oauth/device_authorization"),
            "authorization_endpoint": format!("{base}/oauth/authorize"),
            "token_endpoint": format!("{base}/oauth/token"),
            "registration_endpoint": format!("{base}/oauth/register"),
            "client_id_metadata_document_supported": true,
            "response_types_supported": ["code"],
            "grant_types_supported": [DEVICE_GRANT, "authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "scopes_supported": [OAUTH_SCOPE],
            "authorization_response_iss_parameter_supported": true,
            "token_endpoint_auth_methods_supported": ["none"],
        }),
    )
}
async fn protected_resource(State(app): State<App>) -> Response {
    reply(
        StatusCode::OK,
        json!({
            "resource": mcp_resource(&app),
            "authorization_servers": [origin(&app)],
            "scopes_supported": [OAUTH_SCOPE],
            "bearer_methods_supported": ["header"],
        }),
    )
}
async fn descriptor(State(app): State<App>) -> Response {
    let instance = match app.instance().await {
        Ok(instance) => instance,
        Err(error) => return unavailable(&error).into_response(),
    };
    let base = origin(&app);
    let host = base
        .split_once("://")
        .map_or(base, |(_, rest)| rest)
        .split(['/', ':'])
        .next()
        .unwrap_or_default();
    reply(
        StatusCode::OK,
        json!({
            "api_base": format!("{base}/api/v1"),
            "api_version": "1",
            "plan_endpoint": format!("{base}/api/v1/plans"),
            "commit_endpoint": format!("{base}/api/v1/plans/commit"),
            "server_name": host,
            "instance": instance.agent_json(),
            "client_id": CLIENT_ID,
        }),
    )
}

// --- OAuth device authorization and token ------------------------------------

/// OAuth form fields, urlencoded as RFC 6749 requires or as a JSON object.
/// Non-string values count as absent.
async fn params(request: Request) -> Option<HashMap<String, String>> {
    let json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    if json {
        let Json(fields) = Json::<Map<String, Value>>::from_request(request, &())
            .await
            .ok()?;
        Some(
            fields
                .into_iter()
                .filter_map(|(key, value)| match value {
                    Value::String(value) => Some((key, value)),
                    _ => None,
                })
                .collect(),
        )
    } else {
        let axum::Form(fields) = axum::Form::<HashMap<String, String>>::from_request(request, &())
            .await
            .ok()?;
        Some(fields)
    }
}

/// The direct peer, or the client a loopback reverse proxy reports: read
/// `X-Forwarded-For` right to left, skipping loopback hops, and stop at the
/// first other address. An unparseable hop leaves the peer address.
pub(crate) fn sender(request: &Request) -> String {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(peer)| *peer);
    client_address(peer, request.headers()).map_or_else(|| "unknown".into(), |ip| ip.to_string())
}

/// The address behind [`sender`], for callers that hold the peer and the
/// headers apart (a form handler that cannot take the whole request). `None`
/// only when the server was not given connection info.
pub(crate) fn client_address(peer: Option<SocketAddr>, headers: &HeaderMap) -> Option<IpAddr> {
    let peer = peer?.ip();
    if !peer.is_loopback() {
        return Some(peer);
    }
    let hops: Vec<&str> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .collect();
    for hop in hops.into_iter().rev() {
        match hop.trim().parse::<IpAddr>() {
            Ok(address) if address.is_loopback() => continue,
            Ok(address) => return Some(address),
            Err(_) => break,
        }
    }
    Some(peer)
}

pub(crate) async fn allowed(app: &App, kind: &str, sender: &str) -> Result<bool, Error> {
    let client = app.db.get().await?;
    let limit = app
        .api
        .settings
        .get_global(&**client, OAUTH_REQUESTS_PER_MINUTE)
        .await?
        .as_u64()
        .and_then(|limit| u32::try_from(limit).ok())
        .ok_or(Error::Invalid)?;
    Ok(app.api.throttle.allow(&format!("{kind}:{sender}"), limit))
}

/// Token-endpoint answers are never cached (RFC 6749 section 5.1).
fn oauth(result: Result<Value, OAuthError>) -> Response {
    let mut response = match result {
        Ok(body) => reply(StatusCode::OK, body),
        Err(OAuthError::Other(error)) => unavailable(&error).into_response(),
        Err(code) => error(StatusCode::BAD_REQUEST, &code.to_string()).into_response(),
    };
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

/// RFC 7591 registration is intentionally unauthenticated. The same direct
/// sender attribution and operator-set limiter as the token edge apply.
async fn register(State(app): State<App>, request: Request) -> Response {
    let sender = sender(&request);
    match allowed(&app, "register", &sender).await {
        Ok(true) => {}
        Ok(false) => return oauth(Err(OAuthError::SlowDown)),
        Err(error) => return oauth(Err(error.into())),
    }
    let Ok(Json(value)) = Json::<Value>::from_request(request, &app).await else {
        return oauth(Err(OAuthError::InvalidClientMetadata));
    };
    if value.get("client_secret").is_some() || value.get("client_secret_expires_at").is_some() {
        return oauth(Err(OAuthError::InvalidClientMetadata));
    }
    let Ok(registration) = serde_json::from_value::<ClientRegistration>(value) else {
        return oauth(Err(OAuthError::InvalidClientMetadata));
    };
    let result = app.api.access.register_client(&registration).await;
    let success = result.is_ok();
    let mut response = oauth(result);
    if success {
        *response.status_mut() = StatusCode::CREATED;
    }
    response
}

async fn device(State(app): State<App>, request: Request) -> Response {
    let sender = sender(&request);
    match allowed(&app, "device", &sender).await {
        Ok(true) => {}
        Ok(false) => return oauth(Err(OAuthError::SlowDown)),
        Err(error) => return oauth(Err(error.into())),
    }
    let Some(fields) = params(request).await else {
        return oauth(Err(OAuthError::InvalidRequest));
    };
    let field = |key: &str| fields.get(key).map(String::as_str).unwrap_or_default();
    let started = app
        .api
        .access
        .start_device(field("client_id"), field("agent_id"))
        .await;
    oauth(started.map(|start| {
        let uri = format!("{}/oauth/device", origin(&app));
        json!({
            "device_code": start.device_code,
            "user_code": start.user_code,
            "expires_in": start.expires_in,
            "interval": start.interval,
            "verification_uri_complete": format!("{uri}?user_code={}", query_value(&start.user_code)),
            "verification_uri": uri,
        })
    }))
}

fn query_value(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                char::from(byte).to_string()
            }
            other => format!("%{other:02X}"),
        })
        .collect()
}

async fn token(State(app): State<App>, request: Request) -> Response {
    match app.secure_origin().await {
        Ok(true) => {}
        Ok(false) => {
            return reply(
                StatusCode::BAD_REQUEST,
                json!({"error":"invalid_request", "error_description":"APP_ORIGIN must use HTTPS. Only a Dev copy on this computer may use HTTP on a loopback address."}),
            )
        }
        Err(error) => return unavailable(&error).into_response(),
    }
    let client_identity = client_identity(request.headers(), false);
    let sender = sender(&request);
    match allowed(&app, "token", &sender).await {
        Ok(true) => {}
        Ok(false) => return oauth(Err(OAuthError::SlowDown)),
        Err(error) => return oauth(Err(error.into())),
    }
    let Some(fields) = params(request).await else {
        return oauth(Err(OAuthError::InvalidRequest));
    };
    let field = |key: &str| fields.get(key).map(String::as_str).unwrap_or_default();
    let access = &app.api.access;
    let issued = match fields.get("grant_type").map(String::as_str) {
        None => Err(OAuthError::InvalidRequest),
        Some(DEVICE_GRANT) => {
            access
                .exchange(field("client_id"), field("device_code"))
                .await
        }
        Some("refresh_token") => {
            access
                .refresh(
                    field("client_id"),
                    field("refresh_token"),
                    fields.get("resource").map(String::as_str),
                )
                .await
        }
        Some("authorization_code") => {
            let exchange = CodeExchange {
                code: field("code").into(),
                client_id: field("client_id").into(),
                redirect_uri: field("redirect_uri").into(),
                code_verifier: field("code_verifier").into(),
                resource: field("resource").into(),
            };
            access
                .exchange_code_from(&exchange, &mcp_resource(&app), client_identity.as_ref())
                .await
        }
        Some(_) => {
            return oauth_code("unsupported_grant_type");
        }
    };
    oauth(issued.map(|tokens| {
        let mut body = json!({
            "access_token": tokens.access_token,
            "refresh_token": tokens.refresh_token,
            "token_type": tokens.token_type,
            "expires_in": tokens.expires_in,
        });
        if let Some(scope) = tokens.scope {
            body["scope"] = json!(scope);
        }
        body
    }))
}
fn oauth_code(code: &str) -> Response {
    let mut response = error(StatusCode::BAD_REQUEST, code).into_response();
    response
        .headers_mut()
        .insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    response
}

// --- AgentAuth ---------------------------------------------------------------

/// A bearer request whose grant, account and party facets were proven live
/// for this request alone.
pub(crate) struct Agent {
    pub(crate) scope: Scope,
    pub(crate) claims: AgentClaims,
    pub(crate) trace: Trace,
}

/// HTTP authentication scheme names are case-insensitive; token bytes are not.
fn bearer(headers: &HeaderMap) -> Option<&str> {
    let mut values = headers.get_all(header::AUTHORIZATION).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    let mut parts = value.split_whitespace();
    let (scheme, token) = (parts.next()?, parts.next()?);
    (scheme.eq_ignore_ascii_case("bearer") && parts.next().is_none()).then_some(token)
}

pub(crate) fn refused() -> Refusal {
    Refusal {
        challenge: true,
        ..error(StatusCode::UNAUTHORIZED, "invalid_token")
    }
}

/// Cookies and caller-supplied identity are unused. `resource` is the audience
/// the token must have been issued for: none for `/api/v1`, the MCP resource for
/// `/mcp`, so neither edge accepts the other's tokens.
pub(crate) async fn authenticate(
    app: &App,
    headers: &HeaderMap,
    trace: &Trace,
    resource: Option<&str>,
) -> Result<Agent, Refusal> {
    let access = &app.api.access;
    let fail = |error: OAuthError| match error {
        OAuthError::Other(error) if error.is_retryable_lock_contention() => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":"rejected","reason":"record_changed"}),
        ),
        OAuthError::Other(error) => unavailable(&error),
        _ => refused(),
    };
    let token = bearer(headers).ok_or_else(refused)?;
    let providers = headers.get_all("x-bilibi-identity-provider");
    let mut providers = providers.iter();
    let claims = if let Some(public) = providers.next() {
        if providers.next().is_some() {
            return Err(refused());
        }
        access
            .authenticate_provider(public.to_str().map_err(|_| refused())?, token, resource)
            .await
    } else {
        access.authenticate(token, resource).await
    }
    .map_err(fail)?;
    let report = trace.reported.report();
    let scope = access
        .seal(&claims, &trace.id, report)
        .await
        .map_err(fail)?;
    access
        .observe(
            &claims,
            label(headers, AGENT_ID_HEADER).as_deref(),
            report,
            trace.client_identity.as_ref(),
        )
        .await
        .map_err(fail)?;
    Ok(Agent {
        scope: scope.with_client_identity(trace.client_identity.clone()),
        claims,
        trace: trace.clone(),
    })
}

impl Agent {
    /// Attaches the sealed scope so the exchange row belongs to its tenant.
    fn answer(
        self,
        operation: Option<String>,
        targets: Vec<Target>,
        result: Result<Value, Refusal>,
    ) -> Response {
        let mut response = match result {
            Ok(body) => reply(StatusCode::OK, body),
            Err(refusal) => refusal.into_response(),
        };
        response.extensions_mut().insert(Exchange {
            scope: Some(self.scope),
            operation,
            reported: None,
            client_identity: None,
            targets,
        });
        response
    }
}

/// The plan service's credential adapter: reproves and locks the grant on the
/// plan's own transaction. The binding names the connection, not the rotating
/// access token, so a refreshed token can still commit its connection's plan.
pub(crate) struct Grant<'a> {
    pub(crate) access: &'a AgentAccess,
    pub(crate) claims: &'a AgentClaims,
    pub(crate) trace: &'a Trace,
}
impl PlanAuthority for Grant<'_> {
    fn verify<'a>(&'a self, tx: &'a Transaction<'_>) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            let scope = self
                .access
                .prove(
                    tx,
                    self.claims,
                    true,
                    &self.trace.id,
                    self.trace.reported.report(),
                )
                .await
                .map_err(|error| match error {
                    OAuthError::Other(error) => error,
                    _ => Error::Unauthorized,
                })?;
            Ok(PlanIdentity {
                credential_binding: format!("agent-connection:{}", self.claims.connection),
                scope: scope.with_client_identity(self.trace.client_identity.clone()),
            })
        })
    }
}

/// `/api/v1` wire errors.
fn api_error(error: bilibi_base::agent_api::Error) -> Refusal {
    use bilibi_base::agent_api::Error as E;
    match error {
        E::InvalidInput(fields) => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":"invalid_input","fields":fields}),
        ),
        E::Forbidden | E::Backend(Error::Forbidden) => refusal(
            StatusCode::FORBIDDEN,
            json!({"error":"forbidden","reason":"missing_capability"}),
        ),
        E::UnknownOperation | E::UnknownGuide | E::NotFound | E::Backend(Error::NotFound) => {
            let code = match error {
                E::UnknownOperation => "unknown_operation",
                E::UnknownGuide => "unknown_guide",
                _ => "not_found",
            };
            self::error(StatusCode::NOT_FOUND, code)
        }
        E::InvalidPlanCode
        | E::PlanMismatch
        | E::PlanAlreadyUsed
        | E::PlanExpired
        | E::StalePlan
        | E::WritesNotEnabled => self::error(StatusCode::CONFLICT, &error.to_string()),
        E::ChannelNotAllowed => self::error(StatusCode::FORBIDDEN, "channel_not_allowed"),
        E::PlanRejected(results) => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":"plan_rejected","results":results}),
        ),
        E::Backend(Error::StepUpRequired { operation, reason }) => refusal(
            StatusCode::FORBIDDEN,
            bilibi_base::step_up::refusal(operation, reason),
        ),
        // The grant stopped proving live between the edge and the commit.
        E::Backend(Error::Unauthorized) => refused(),
        E::Backend(Error::Conflict) => refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":"rejected","reason":"record_changed"}),
        ),
        E::Backend(Error::Invalid) => self::error(StatusCode::UNPROCESSABLE_ENTITY, "rejected"),
        E::Backend(other) => unavailable(&other),
    }
}

fn invalid_request() -> Refusal {
    error(StatusCode::BAD_REQUEST, "invalid_request")
}

/// A JSON object body within the API limit, or the JSON refusal to send.
async fn body(request: Request<Body>) -> Result<Map<String, Value>, Refusal> {
    let bytes = axum::body::to_bytes(request.into_body(), API_BODY_LIMIT)
        .await
        .map_err(|_| error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large"))?;
    match serde_json::from_slice(&bytes) {
        Ok(Value::Object(fields)) => Ok(fields),
        _ => Err(invalid_request()),
    }
}

// --- /api/v1 -----------------------------------------------------------------

async fn search(State(app): State<App>, uri: Uri, request: Request) -> Response {
    let trace = trace(&request);
    let agent = match authenticate(&app, request.headers(), &trace, None).await {
        Ok(agent) => agent,
        Err(refusal) => return refusal.into_response(),
    };
    let result = async {
        let Query(query) =
            Query::<HashMap<String, String>>::try_from_uri(&uri).map_err(|_| invalid_request())?;
        let limit = match query.get("limit") {
            None => 10,
            Some(value) => value
                .parse::<usize>()
                .ok()
                .filter(|limit| (1..=50).contains(limit))
                .ok_or_else(invalid_request)?,
        };
        let kind = match query.get("kind").map(String::as_str) {
            None => None,
            Some("read") => Some(Kind::Read),
            Some("write") => Some(Kind::Write),
            Some(_) => return Err(invalid_request()),
        };
        let include_unavailable = match query.get("include_unavailable").map(String::as_str) {
            None | Some("false") => false,
            Some("true") => true,
            Some(_) => return Err(invalid_request()),
        };
        let client = app.db.get().await.map_err(|e| unavailable(&e.into()))?;
        let words = query.get("q").map(String::as_str).unwrap_or_default();
        let options = SearchOptions {
            kind,
            limit,
            include_unavailable,
        };
        app.api
            .registry
            .search(&**client, &agent.scope, words, options)
            .await
            .map(|found| json!({"result": found}))
            .map_err(api_error)
    }
    .await;
    agent.answer(None, Vec::new(), result)
}

pub(crate) fn trace(request: &Request) -> Trace {
    request
        .extensions()
        .get::<Trace>()
        .cloned()
        .unwrap_or_default()
}

async fn describe(State(app): State<App>, Path(key): Path<String>, request: Request) -> Response {
    let trace = trace(&request);
    let agent = match authenticate(&app, request.headers(), &trace, None).await {
        Ok(agent) => agent,
        Err(refusal) => return refusal.into_response(),
    };
    let result = match app.db.get().await {
        Ok(client) => app
            .api
            .registry
            .describe(&**client, &agent.scope, &key)
            .await
            .map_err(api_error),
        Err(error) => Err(unavailable(&error.into())),
    };
    agent.answer(None, Vec::new(), result)
}

async fn guide(State(app): State<App>, Path(key): Path<String>, request: Request) -> Response {
    let trace = trace(&request);
    let agent = match authenticate(&app, request.headers(), &trace, None).await {
        Ok(agent) => agent,
        Err(refusal) => return refusal.into_response(),
    };
    let result = match app.db.get().await {
        Ok(client) => app
            .api
            .registry
            .guide(&**client, &agent.scope, &key)
            .await
            .map_err(api_error),
        Err(error) => Err(unavailable(&error.into())),
    };
    agent.answer(None, Vec::new(), result)
}

async fn call(State(app): State<App>, Path(key): Path<String>, request: Request) -> Response {
    let trace = trace(&request);
    let agent = match authenticate(&app, request.headers(), &trace, None).await {
        Ok(agent) => agent,
        Err(refusal) => return refusal.into_response(),
    };
    let mut targets = Vec::new();
    let result = async {
        let mut fields = body(request).await?;
        let input = match fields.remove("input") {
            Some(input @ Value::Object(_)) => input,
            _ => return Err(invalid_request()),
        };
        let answer = app
            .api
            .registry
            .read(&app.db, &agent.scope, &key, &input)
            .await;
        if let Ok(answer) = &answer {
            targets.push(agent_targets::read(
                &app.api.records,
                &key,
                &input,
                Some(answer),
            ));
        }
        answer.map_err(api_error)
    }
    .await;
    let result = with_record_urls(&app, result);
    agent.answer(Some(key), targets, result)
}

/// The ordered list exactly as sent; a malformed list is a reviewable input
/// error, as the plan service reports its own bounds.
fn operations(value: Value) -> Result<Vec<PlannedOperation>, Refusal> {
    serde_json::from_value(value).map_err(|_| {
        refusal(
            StatusCode::UNPROCESSABLE_ENTITY,
            json!({"error":"invalid_input","fields":{"operations":["must be a bounded non-empty list of key/input objects"]}}),
        )
    })
}

async fn plan(State(app): State<App>, request: Request) -> Response {
    let trace = trace(&request);
    let agent = match authenticate(&app, request.headers(), &trace, None).await {
        Ok(agent) => agent,
        Err(refusal) => return refusal.into_response(),
    };
    let mut targets = Vec::new();
    let result = async {
        let mut fields = body(request).await?;
        let operations = operations(fields.remove("operations").ok_or_else(invalid_request)?)?;
        targets = planned_targets(&app, &operations, None);
        let authority = Grant {
            access: &app.api.access,
            claims: &agent.claims,
            trace: &agent.trace,
        };
        app.api
            .plans
            .preview(&authority, &operations)
            .await
            .map_err(api_error)
    }
    .await;
    let result = with_record_urls(&app, result);
    agent.answer(Some("agent_api.plan".into()), targets, result)
}

async fn commit(State(app): State<App>, request: Request) -> Response {
    let trace = trace(&request);
    let agent = match authenticate(&app, request.headers(), &trace, None).await {
        Ok(agent) => agent,
        Err(refusal) => return refusal.into_response(),
    };
    let mut targets = Vec::new();
    let result = async {
        let mut fields = body(request).await?;
        let Some(Value::String(code)) = fields.remove("plan_code") else {
            return Err(invalid_request());
        };
        let operations = operations(fields.remove("operations").ok_or_else(invalid_request)?)?;
        // The account holder's step-up code, sent after a `step_up_required`.
        let scope = match fields.remove("step_up_code") {
            None => agent.scope.clone(),
            Some(Value::String(step_up)) => agent.scope.clone().presenting_step_up(&step_up),
            Some(_) => return Err(invalid_request()),
        };
        let authority = Grant {
            access: &app.api.access,
            claims: &agent.claims,
            trace: &agent.trace,
        };
        let committed = app
            .api
            .plans
            .commit(&scope, &authority, &code, &operations)
            .await;
        if let Ok(answer) = &committed {
            targets = planned_targets(&app, &operations, Some(answer));
        }
        committed.map_err(api_error)
    }
    .await;
    let result = with_record_urls(&app, result);
    agent.answer(Some("agent_api.commit".into()), targets, result)
}
