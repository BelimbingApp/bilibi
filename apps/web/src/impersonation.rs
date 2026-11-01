//! Impersonation enter/leave (routes
//! `POST /admin/impersonate/:id` and `POST /admin/impersonate/leave`). Both are
//! method links: `ui.js` posts them with the session's CSRF secret in
//! `X-CSRF-Token`, follows the redirect and navigates to where it lands. Core
//! User decides and switches the session; this adapter proves Origin, reads
//! the request facts the audit row keeps, and maps each refusal to a
//! flash and landing page.
use crate::{api, auth, origin, token, App, HttpError};
use axum::{
    extract::{ConnectInfo, Extension, Path, State},
    http::{header, HeaderMap, HeaderValue},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_base::audit::RequestInfo;
use bilibi_database::Error;
use bilibi_user::impersonation::{Left, Refusal};
use std::net::SocketAddr;

pub const ENTER: &str = "/admin/impersonate/{id}";
pub const LEAVE: &str = "/admin/impersonate/leave";
/// Where a switch lands.
const DASHBOARD: &str = "/dashboard";
const USERS: &str = "/users";

fn request(
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    headers: &HeaderMap,
    path: &str,
) -> RequestInfo {
    RequestInfo {
        ip: api::client_address(peer.map(|Extension(ConnectInfo(peer))| peer), headers),
        url: Some(path.to_owned()),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    }
}

/// The CSRF secret a method link carries; a missing one never matches.
fn csrf(headers: &HeaderMap) -> &str {
    headers
        .get("x-csrf-token")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
}

/// An ended session answers like any page guard: back to sign-in, saying the
/// session expired.
fn signed_out(app: &App, error: Error) -> std::result::Result<Response, HttpError> {
    match error {
        Error::Unauthorized => Ok(auth::sign_in_redirect(app, true)),
        error => Err(error.into()),
    }
}

fn refused(app: &App, key: &'static str, to: &str) -> Response {
    let mut response = Redirect::to(to).into_response();
    auth::set_flash(app, &mut response, key);
    response
}

pub async fn enter(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
    Path(id): Path<String>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let credential = match token(&headers) {
        Ok(credential) => credential,
        Err(error) => return signed_out(&app, error),
    };
    // An id that is not a whole number takes the "yourself" branch.
    let Some(target) = id.parse::<i64>().ok().filter(|id| *id > 0) else {
        return Ok(refused(&app, auth::IMPERSONATE_SELF, USERS));
    };
    let info = request(peer, &headers, &format!("/admin/impersonate/{target}"));
    match app
        .user
        .impersonate(&credential, csrf(&headers), target, &info)
        .await
    {
        Ok(_) => Ok(Redirect::to(DASHBOARD).into_response()),
        Err(Refusal::Forbidden) => Ok(refused(&app, auth::IMPERSONATE_FORBIDDEN, DASHBOARD)),
        Err(Refusal::AlreadyImpersonating) => {
            Ok(refused(&app, auth::IMPERSONATE_NESTED, DASHBOARD))
        }
        Err(Refusal::OwnAccount) => Ok(refused(&app, auth::IMPERSONATE_SELF, USERS)),
        Err(Refusal::NotFound) => Ok(refused(&app, auth::IMPERSONATE_MISSING, USERS)),
        Err(Refusal::Unable) => Ok(refused(&app, auth::IMPERSONATE_UNABLE, USERS)),
        Err(Refusal::Other(error)) => signed_out(&app, error),
    }
}

pub async fn leave(
    State(app): State<App>,
    headers: HeaderMap,
    peer: Option<Extension<ConnectInfo<SocketAddr>>>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let credential = match token(&headers) {
        Ok(credential) => credential,
        Err(error) => return signed_out(&app, error),
    };
    let info = request(peer, &headers, LEAVE);
    match app
        .user
        .leave_impersonation(&credential, csrf(&headers), &info)
        .await
    {
        Ok(Left::Restored | Left::NotImpersonating) => Ok(Redirect::to(DASHBOARD).into_response()),
        Ok(Left::SignedOut) => {
            let mut response = Redirect::to("/").into_response();
            response.headers_mut().insert(
                header::SET_COOKIE,
                HeaderValue::from_static(
                    "bilibi_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0",
                ),
            );
            Ok(response)
        }
        Err(error) => signed_out(&app, error),
    }
}
