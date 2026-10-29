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
    extract::{Form, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_base::menu;
use bilibi_database::{Error, Result};
use bilibi_module_registry::contributions::{Access, Method};
use bilibi_tenancy::PortalScope;
use bilibi_ui::{download::DownloadRequest, page::PageRequest, Html, PortalLayout, PortalLink};
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
    // The portal has no live socket yet (`/portal/live` is reserved for it),
    // so a portal page renders static; the staff socket would refuse it.
    let html = layout(&app, &portal, uri.path(), false, &page.html).await?;
    let mut response = crate::document_titled(title, &html)?.into_response();
    *response.status_mut() = StatusCode::OK;
    Ok(response)
}

/// The portal layout around an owner's page: the tenant's title, the portal
/// menu the customer may see (decided like the staff menu, against the
/// customer's effective capabilities, which hold portal keys only) and the
/// account's links. The tenant's own portal title and logo come with Core
/// Customer Access; until then the title is the tenant's name.
async fn layout(
    app: &App,
    portal: &PortalScope,
    path: &str,
    live: bool,
    body: &str,
) -> Result<String> {
    let scope = portal.scope();
    let capabilities = {
        let client = app.db.get().await.map_err(Error::from)?;
        bilibi_authz::effective_capabilities(&**client, scope).await?
    };
    let tree = app
        .menu
        .visible_portal_tree(&|key| capabilities.contains(key));
    let mut links = Vec::new();
    flatten(&tree, path, &mut links);
    Html::render(&PortalLayout {
        title: &scope.tenant_name,
        logo: None,
        links: &links,
        user: &scope.user_name,
        email: &scope.email,
        csrf: &scope.csrf,
        live,
        body,
    })
    .map(|html| html.as_str().to_owned())
    .map_err(|_| Error::Internal)
}

/// Depth-first, the pages of the portal menu as one row of links.
fn flatten(nodes: &[menu::Node], path: &str, links: &mut Vec<PortalLink>) {
    for node in nodes {
        let item = node.item.item;
        if let Some(route) = item.route {
            links.push(PortalLink {
                id: item.id.replace('.', "-"),
                label: item.label.to_owned(),
                icon: item.icon.unwrap_or("document").to_owned(),
                route: route.to_owned(),
                active: route == path,
            });
        }
        flatten(&node.children, path, links);
    }
}

#[derive(serde::Deserialize)]
pub struct SignOut {
    csrf: String,
}
/// `POST /portal/sign-out`: the customer ends its own portal session and the
/// portal cookie is cleared. The staff `/logout` never ends a portal session.
pub async fn sign_out(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<SignOut>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let credential = token(&headers)?;
    app.user.portal_logout(&credential, &form.csrf).await?;
    let mut response = sign_in();
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("bilibi_portal=; HttpOnly; SameSite=Lax; Path=/portal; Max-Age=0"),
    );
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
