//! `/mcp`: stateless Streamable HTTP with JSON replies, no SSE and no session
//! store (Bilimbi `MCPController`). The host negotiates: Origin, protocol
//! version, POST only, Accept, then the per-sender limit from
//! `agent_api.mcp_requests_per_minute`. Public messages (initialize, ping,
//! tools/list, notifications) need no credential. Private messages pass the
//! same bearer edge as `/api/v1` (`api::authenticate`) with the MCP resource as
//! audience, and plans reprove the grant with the same `Grant` authority. The
//! protocol itself is `bilibi_base::agent_api::mcp`; this file adds nothing a
//! domain or permission decision depends on.
use crate::{
    api::{self, Exchange, Grant},
    App,
};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap, HeaderValue, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Router,
};
use bilibi_base::agent_api::mcp::{self as protocol, Outcome, Session};
use bilibi_database::Error;
use bilibi_tenancy::Scope;
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
    let trace = api::trace(&request);
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
        });
        return response;
    };
    let operation = protocol::operation(&message);
    let edge = &app.api;
    if protocol::public(&message) {
        let outcome = protocol::dispatch(&edge.registry, &edge.plans, None, &message).await;
        return answer(&app, outcome, None, operation);
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
    let outcome = match app.db.get().await {
        Ok(client) => {
            let authority = Grant {
                access: &edge.access,
                claims: &claims,
                trace: &trace,
            };
            let session = Session {
                client: &client,
                scope: &scope,
                authority: &authority,
            };
            protocol::dispatch(&edge.registry, &edge.plans, Some(session), &message).await
        }
        Err(error) => Outcome::Unavailable(error.into()),
    };
    answer(&app, outcome, Some(scope), operation)
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

fn answer(app: &App, outcome: Outcome, scope: Option<Scope>, operation: String) -> Response {
    let mut response = match outcome {
        Outcome::Accepted => StatusCode::ACCEPTED.into_response(),
        Outcome::Reply(reply) => api::reply(StatusCode::OK, record_urls(app, reply)),
        Outcome::Unauthorized => challenge(app, api::refused().into_response()),
        Outcome::Unavailable(error) => api::unavailable(&error).into_response(),
    };
    response.extensions_mut().insert(Exchange {
        scope,
        operation: Some(operation),
    });
    response
}

/// Absolute result links belong to the host; domain operations return paths.
fn record_urls(app: &App, mut reply: Value) -> Value {
    let Some(results) = reply
        .pointer_mut("/result/structuredContent/results")
        .and_then(Value::as_array_mut)
    else {
        return reply;
    };
    for item in results.iter_mut() {
        if let Some(path) = item.get("path").and_then(Value::as_str) {
            let url = format!("{}{path}", api::origin(app));
            item["url"] = json!(url);
        }
    }
    let content = reply["result"]["structuredContent"].clone();
    reply["result"]["content"] = json!([{"type": "text", "text": content.to_string()}]);
    reply
}
