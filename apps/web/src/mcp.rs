//! `/mcp`: stateless Streamable HTTP with JSON replies, no SSE and no session
//! store (Bilimbi `MCPController`). The host negotiates: Origin, protocol
//! version, POST only, Accept, then the per-sender limit from
//! `agent_api.mcp_requests_per_minute`. Public messages (initialize, ping,
//! tools/list, notifications) need no credential. Private messages pass the
//! same bearer edge as `/api/v1` (`api::authenticate`) with the MCP resource as
//! audience, and plans reprove the grant with the same `Grant` authority. The
//! protocol itself is `bilibi_base::agent_api::mcp`; this file adds nothing a
//! domain or permission decision depends on. The agent's self-report may come
//! in `params._meta` under `SelfReport::MCP_META` as well as in the header; the
//! body's value wins, and both are evidence only.
use crate::{
    agent_targets,
    api::{self, Exchange, Grant, Reported},
    App,
};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use bilibi_base::{
    agent_api::{
        mcp::{self as protocol, Outcome, Session},
        plans::PlannedOperation,
        AppSource,
    },
    integration::Target,
};
use bilibi_database::Error;
use bilibi_tenancy::{ClientIdentity, Scope, SelfReport};
use serde_json::{json, Value};

pub fn router() -> Router<App> {
    Router::new().route("/mcp", any(handle))
}

/// True when the header is absent or has exactly one acceptable value.
fn single_or_absent(headers: &HeaderMap, name: &str, accept: impl Fn(&str) -> bool) -> bool {
    let mut values = headers.get_all(name).iter();
    match (values.next(), values.next()) {
        (None, _) => true,
        (Some(value), None) => value.to_str().is_ok_and(accept),
        _ => false,
    }
}

async fn handle(State(app): State<App>, request: Request) -> Response {
    let headers = request.headers().clone();
    if !single_or_absent(&headers, "origin", |value| value == api::origin(&app)) {
        return api::reply(StatusCode::FORBIDDEN, json!({"error": "invalid_origin"}));
    }
    if !single_or_absent(&headers, "mcp-protocol-version", |value| {
        protocol::VERSIONS.contains(&value)
    }) {
        return api::reply(
            StatusCode::BAD_REQUEST,
            json!({"error": "unsupported_protocol_version"}),
        );
    }
    if request.method() != Method::POST {
        return (
            StatusCode::METHOD_NOT_ALLOWED,
            [(header::ALLOW, HeaderValue::from_static("POST"))],
        )
            .into_response();
    }
    let accept: Vec<&str> = headers
        .get_all(header::ACCEPT)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .collect();
    let accept = accept.join(",");
    if !(accept.contains("application/json") && accept.contains("text/event-stream")) {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    match allowed(&app, &api::sender(&request)).await {
        Ok(true) => {}
        Ok(false) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, HeaderValue::from_static("60"))],
            )
                .into_response()
        }
        Err(error) => return api::unavailable(&error).into_response(),
    }
    let mut trace = api::trace(&request);
    let Ok(bytes) = axum::body::to_bytes(request.into_body(), api::API_BODY_LIMIT).await else {
        return api::error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large").into_response();
    };
    let Ok(message) = serde_json::from_slice::<Value>(&bytes) else {
        let mut response = api::reply(
            StatusCode::BAD_REQUEST,
            json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32_700, "message": "Parse error"}}),
        );
        response.extensions_mut().insert(Exchange {
            scope: None,
            operation: Some("mcp.invalid_request".into()),
            reported: None,
            client_identity: None,
            targets: Vec::new(),
        });
        return response;
    };
    let meta = message
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| meta.get(SelfReport::MCP_META));
    match Reported::from_meta(meta) {
        Reported::Absent => {}
        reported => trace.reported = reported,
    }
    // Generic arguments are model-accessible; they override _meta and headers.
    if message.get("method").and_then(Value::as_str) == Some("tools/call")
        && message
            .pointer("/params/name")
            .and_then(Value::as_str)
            .is_some_and(|name| protocol::GENERIC_TOOLS.contains(&name))
    {
        match Reported::from_meta(
            message.pointer(&format!("/params/arguments/{}", SelfReport::MCP_ARGUMENT)),
        ) {
            Reported::Absent => {}
            reported => trace.reported = reported,
        }
    }
    // Stateless: initialize's clientInfo is evidence for that exchange only.
    let initialize_client = (message.get("method").and_then(Value::as_str) == Some("initialize"))
        .then(|| {
            let name = message.pointer("/params/clientInfo/name")?.as_str()?;
            let version = message.pointer("/params/clientInfo/version")?.as_str()?;
            ClientIdentity::parse(&format!("MCP clientInfo: {name}/{version}"))
        })
        .flatten();
    let operation = protocol::operation(&message);
    let edge = &app.api;
    if protocol::public(&message) {
        let outcome = protocol::dispatch(&edge.registry, &edge.plans, None, &message).await;
        let mut response = answer(&app, outcome, None, operation, &trace.reported, Vec::new());
        if let Some(exchange) = response.extensions_mut().get_mut::<Exchange>() {
            exchange.client_identity = initialize_client;
        }
        return response;
    }
    let agent =
        match api::authenticate(&app, &headers, &trace, Some(&api::mcp_resource(&app))).await {
            Ok(agent) => agent,
            Err(refusal) => {
                let mut response = refusal.into_response();
                if response.status() == StatusCode::UNAUTHORIZED {
                    response = challenge(&app, response);
                }
                response.extensions_mut().insert(Exchange {
                    scope: None,
                    operation: Some(operation),
                    reported: Some(trace.reported),
                    client_identity: None,
                    targets: Vec::new(),
                });
                return response;
            }
        };
    let api::Agent {
        scope,
        claims,
        trace,
    } = agent;
    let scope = scope.over_mcp();
    let authority = Grant {
        access: &edge.access,
        claims: &claims,
        trace: &trace,
    };
    let session = Session {
        db: &app.db,
        scope: &scope,
        authority: &authority,
        step_up: &app.step_up,
    };
    let outcome = protocol::dispatch(&edge.registry, &edge.plans, Some(session), &message).await;
    let targets = targets(&app, &message, &outcome);
    answer(
        &app,
        outcome,
        Some(scope),
        operation,
        &trace.reported,
        targets,
    )
}

async fn allowed(app: &App, sender: &str) -> Result<bool, Error> {
    let client = app.db.get().await?;
    let limit = app
        .api
        .settings
        .get_global(&**client, protocol::REQUESTS_PER_MINUTE)
        .await?
        .as_u64()
        .and_then(|limit| u32::try_from(limit).ok())
        .ok_or(Error::Invalid)?;
    Ok(app.api.throttle.allow(&format!("mcp:{sender}"), limit))
}

/// The bearer challenge names the protected-resource document, so a client
/// can discover the authorization server (RFC 9728).
fn challenge(app: &App, mut response: Response) -> Response {
    let value = format!(
        "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource/mcp\"",
        api::origin(app)
    );
    response.headers_mut().insert(
        header::WWW_AUTHENTICATE,
        HeaderValue::from_str(&value).unwrap_or(HeaderValue::from_static("Bearer")),
    );
    response
}

fn answer(
    app: &App,
    outcome: Outcome,
    scope: Option<Scope>,
    operation: String,
    reported: &Reported,
    targets: Vec<Target>,
) -> Response {
    let mut response = match outcome {
        Outcome::Accepted => StatusCode::ACCEPTED.into_response(),
        Outcome::Reply(reply) => api::reply(StatusCode::OK, record_urls(app, reply)),
        Outcome::Unauthorized => challenge(app, api::refused().into_response()),
        Outcome::Unavailable(error) => api::unavailable(&error).into_response(),
    };
    response.extensions_mut().insert(Exchange {
        scope,
        operation: Some(operation),
        reported: Some(reported.clone()),
        client_identity: None,
        targets,
    });
    response
}

/// What a tool call operated on, for the exchange row (`agent_targets.rs`): a
/// `read` or an app's card names one operation, a `plan` or `commit` each
/// operation of its list. A failed call names nothing it did not do: only
/// reads and committed writes record their answer's records.
fn targets(app: &App, message: &Value, outcome: &Outcome) -> Vec<Target> {
    let Outcome::Reply(reply) = outcome else {
        return Vec::new();
    };
    let (Some(name), Some(arguments)) = (
        message.pointer("/params/name").and_then(Value::as_str),
        message.pointer("/params/arguments"),
    ) else {
        return Vec::new();
    };
    let failed = reply
        .pointer("/result/isError")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    let answer = reply.pointer("/result/structuredContent");
    let records = &app.api.records;
    let planned = || -> Vec<PlannedOperation> {
        arguments
            .get("operations")
            .cloned()
            .and_then(|list| serde_json::from_value(list).ok())
            .unwrap_or_default()
    };
    match name {
        "read" if !failed => {
            let (Some(key), Some(input)) = (
                arguments.get("key").and_then(Value::as_str),
                arguments.get("input"),
            ) else {
                return Vec::new();
            };
            vec![agent_targets::read(records, key, input, answer)]
        }
        "plan" => api::planned_targets(app, &planned(), None),
        "commit" if !failed => api::planned_targets(app, &planned(), answer),
        _ => match app
            .api
            .registry
            .apps
            .get(name)
            .map(|app| &app.definition.source)
        {
            Some(AppSource::Operation(key)) if !failed => {
                vec![agent_targets::read(records, key, arguments, answer)]
            }
            _ => Vec::new(),
        },
    }
}

/// Absolute result links belong to the host; domain operations return paths.
/// Search results carry a `path`, and record references their `kind`, `id`
/// and `path` (`api::record_urls`).
fn record_urls(app: &App, mut reply: Value) -> Value {
    let Some(structured) = reply.pointer_mut("/result/structuredContent") else {
        return reply;
    };
    let before = structured.clone();
    api::record_urls(api::origin(app), structured);
    if let Some(results) = structured.get_mut("results").and_then(Value::as_array_mut) {
        for item in results.iter_mut() {
            if let Some(path) = item.get("path").and_then(Value::as_str) {
                let url = format!("{}{path}", api::origin(app));
                item["url"] = json!(url);
            }
        }
    }
    if *structured == before {
        return reply;
    }
    let content = reply["result"]["structuredContent"].clone();
    reply["result"]["content"] = json!([{"type": "text", "text": content.to_string()}]);
    reply
}
