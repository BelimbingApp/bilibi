//! Portal pages and downloads (ADR 16): a tenant's customers on `/portal/...`.
//! The host reads only the portal cookie here, refuses a staff session, seals
//! a `PortalScope` and asks Authz for the route's portal capability before
//! the owner renders. Bilimbi has no customer portal; this mirrors the staff
//! dispatch in `routes.rs`.
use crate::{
    origin,
    routes::{BoundPortalDownload, BoundPortalRoute, PORTAL_SIGN_IN},
    App, HttpError,
};
use axum::{
    body::Bytes,
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_database::{Error, Result};
use bilibi_module_registry::contributions::{Access, Method};
use bilibi_tenancy::PortalScope;
use bilibi_ui::{download::DownloadRequest, page::PageRequest};
use std::collections::HashMap;

/// The customer's session cookie. `Path=/portal`, so the browser never sends
/// it to a staff path, where a customer is therefore anonymous.
pub const COOKIE: &str = "bilibi_portal";

/// The portal cookie's token. The staff cookie is never read here.
pub fn token(headers: &HeaderMap) -> Result<String> {
    headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix(COOKIE)
                    .and_then(|rest| rest.strip_prefix('='))
                    .map(str::to_owned)
            })
        })
        .filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or(Error::Unauthorized)
}

/// The portal session, then the route's portal capability, then its
/// step-up, as `routes::admit` does for staff. A staff session, a missing
/// one and a customer of a closed account are all `Unauthorized`. The portal
/// has no step-up prompt of its own yet, so a step-up still owed is refused.
async fn admit(
    app: &App,
    access: Access,
    credential: Option<&str>,
    target: &str,
) -> Result<PortalScope> {
    let Access::Portal(key) = access.base() else {
        return Err(Error::Internal);
    };
    let portal = app
        .security
        .guard_portal(credential.ok_or(Error::Unauthorized)?)
        .await?;
    let client = app.db.get().await.map_err(Error::from)?;
    bilibi_authz::authorize_key(&**client, portal.scope(), key).await?;
    drop(client);
    if let Some(operation) = access.step_up() {
        match app.step_up.require(portal.scope(), operation, target).await {
            Ok(()) => {}
            Err(Error::StepUpRequired { .. }) => return Err(Error::Forbidden),
            Err(error) => return Err(error),
        }
    }
    Ok(portal)
}

/// A visitor with no customer session goes to the portal sign-in, which
/// knows the site the customer last signed in to.
fn sign_in() -> Response {
    Redirect::to(PORTAL_SIGN_IN).into_response()
}

pub async fn dispatch(
    app: App,
    bound: BoundPortalRoute,
    uri: Uri,
    headers: HeaderMap,
    path: HashMap<String, String>,
    query: HashMap<String, String>,
    body: Bytes,
) -> std::result::Result<Response, HttpError> {
    let credential = token(&headers).ok();
    let get = bound.route.method == Method::Get;
    // A write from another site is refused before any authorization
    // decision is made or logged.
    if !get {
        origin(&app, &headers)?;
    }
    let portal = match admit(&app, bound.route.access, credential.as_deref(), uri.path()).await {
        Err(Error::Unauthorized) if get => return Ok(sign_in()),
        Ok(portal) => portal,
        Err(error) => return Err(error.into()),
    };
    if !get
        && headers.get("x-csrf-token").and_then(|h| h.to_str().ok())
            != Some(portal.scope().csrf.as_str())
    {
        return Err(Error::Forbidden.into());
    }
    // Boot refuses a framed portal route; a framed request is refused too.
    if matches!(
        headers.get("sec-fetch-dest").and_then(|h| h.to_str().ok()),
        Some("iframe" | "frame")
    ) {
        return Err(Error::Forbidden.into());
    }
    let page = bound
        .page
        .0
        .render(PageRequest {
            operation: bound.route.operation,
            scope: Some(&portal),
            path: &path,
            query: &query,
            body: &body,
            framed: false,
        })
        .await?;
    if let Some(destination) = bound.page.0.redirect(bound.route.operation) {
        return Ok(Redirect::to(destination).into_response());
    }
    if let Some(credential) = &credential {
        let client = app.db.get().await.map_err(Error::from)?;
        app.security
            .sessions
            .activity_portal(&**client, credential)
            .await?;
    }
    let title = page
        .title
        .as_deref()
        .or_else(|| bound.page.0.title(bound.route.operation))
        .unwrap_or("Bilibi");
    let mut response = crate::document_titled(title, &page.html)?.into_response();
    *response.status_mut() = StatusCode::OK;
    Ok(response)
}

/// A portal download: the customer's session and portal capability are
/// proven for this request, then the owner serves the file through its
/// facade, which checks the proof's ids again.
pub async fn download(
    app: App,
    bound: BoundPortalDownload,
    uri: Uri,
    headers: HeaderMap,
    path: HashMap<String, String>,
    query: HashMap<String, String>,
) -> std::result::Result<Response, HttpError> {
    let credential = token(&headers).ok();
    let portal = match admit(
        &app,
        bound.download.access,
        credential.as_deref(),
        uri.path(),
    )
    .await
    {
        Err(Error::Unauthorized) => return Ok(sign_in()),
        Ok(portal) => portal,
        Err(error) => return Err(error.into()),
    };
    let credential = credential.ok_or(Error::Forbidden)?;
    let file = bound
        .service
        .0
        .serve(DownloadRequest {
            operation: bound.download.operation,
            scope: &portal,
            path: &path,
            query: &query,
        })
        .await?;
    let client = app.db.get().await.map_err(Error::from)?;
    app.security
        .sessions
        .activity_portal(&**client, &credential)
        .await?;
    let delivery = file.headers();
    let mut response = (StatusCode::OK, file.bytes).into_response();
    for (name, value) in delivery {
        let value = HeaderValue::from_str(&value).map_err(|_| Error::Internal)?;
        response
            .headers_mut()
            .insert(HeaderName::from_static(name), value);
    }
    Ok(response)
}
