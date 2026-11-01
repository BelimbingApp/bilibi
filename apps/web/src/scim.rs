//! SCIM identity lifecycle adapter; extends sign-in with no SCIM peer.
//! The company credential and every business decision belong to Core User.
use crate::{api, App};
use axum::{
    body::to_bytes,
    extract::{ConnectInfo, Path, Query, Request, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
    Json, Router,
};
use std::{collections::HashMap, net::SocketAddr};

pub fn router() -> Router<App> {
    Router::new()
        .route("/scim/v2/{provider}/{resource}", any(handle))
        .route("/scim/v2/{provider}/{resource}/{id}", any(handle))
}
async fn handle(
    State(app): State<App>,
    Path(path): Path<HashMap<String, String>>,
    Query(query): Query<HashMap<String, String>>,
    request: Request,
) -> Response {
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let sender = api::client_address(peer, request.headers())
        .map_or_else(|| "unknown".into(), |ip| ip.to_string());
    match api::allowed(&app, "scim", &sender).await {
        Ok(true) => {}
        Ok(false) => return failure(429, "Too many provisioning requests."),
        Err(_) => return failure(503, "Provisioning is temporarily unavailable."),
    }
    let method = request.method().to_string();
    let bearer = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("Bearer"))
        .map(|(_, v)| v.to_owned())
        .unwrap_or_default();
    if !matches!(method.as_str(), "GET" | "DELETE")
        && !request
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .is_some_and(|h| {
                matches!(
                    h.split(';').next().unwrap_or("").trim(),
                    "application/scim+json" | "application/json"
                )
            })
    {
        return failure(415, "Use application/scim+json.");
    }
    let bytes = match to_bytes(request.into_body(), 2 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => return failure(413, "The provisioning request is too large."),
    };
    let body = if bytes.is_empty() {
        serde_json::Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return failure(400, "The provisioning document is invalid JSON."),
        }
    };
    match app
        .user
        .scim_request(bilibi_user::ScimRequest {
            connection: path.get("provider").map(String::as_str).unwrap_or(""),
            bearer: &bearer,
            method: &method,
            resource: path.get("resource").map(String::as_str).unwrap_or(""),
            id: path.get("id").map(String::as_str),
            query: &query,
            body,
        })
        .await
    {
        Ok(result) => {
            let status =
                StatusCode::from_u16(result.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            let mut response = if status == StatusCode::NO_CONTENT {
                status.into_response()
            } else {
                (status, Json(result.body)).into_response()
            };
            if let Some(location) = result.location.and_then(|s| s.parse().ok()) {
                response.headers_mut().insert(header::LOCATION, location);
            }
            headers(response)
        }
        Err(e) => headers(
            (
                StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(e.document()),
            )
                .into_response(),
        ),
    }
}
fn failure(status: u16, detail: &'static str) -> Response {
    let e = bilibi_user::ScimError {
        status,
        kind: "",
        detail,
    };
    headers(
        (
            StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(e.document()),
        )
            .into_response(),
    )
}
fn headers(mut response: Response) -> Response {
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        "application/scim+json".parse().unwrap(),
    );
    h.insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    if response.status() == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
    }
    response
}
