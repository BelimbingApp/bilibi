//! Portal pages and downloads (ADR 16): a tenant's customers on `/portal/...`.
//! The host reads only the portal cookie here, refuses a staff session, seals
//! a `PortalScope` and asks Authz for the route's portal capability before
//! the owner renders. Bilimbi has no customer portal; this mirrors the staff
//! dispatch in `routes.rs`.
use crate::{
    live, origin, routes,
    routes::{BoundPortalDownload, BoundPortalRoute, PORTAL_SIGN_IN},
    App, HttpError,
};
use axum::{
    body::Bytes,
    extract::{ws::WebSocketUpgrade, Form, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Redirect, Response},
};
use bilibi_base::menu;
use bilibi_database::{Error, Result};
use bilibi_module_registry::contributions::{Access, Method};
use bilibi_tenancy::PortalScope;
use bilibi_ui::{
    download::DownloadRequest, page::PageRequest, Html, PortalDocument, PortalLayout, PortalLink,
};
use std::collections::HashMap;

/// The portal home, where a customer lands after signing in.
pub const HOME: &str = "/portal";

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
/// one and a customer of a closed account are all `Unauthorized`; a
/// non-portal access (a staff embed named from a portal page) is refused.
/// The portal has no step-up prompt of its own yet, so a step-up still owed
/// is refused. `target` names the HTTP navigation being admitted, whose
/// check extends the window; the live socket's recheck only reads it.
async fn admit(
    app: &App,
    access: Access,
    credential: Option<&str>,
    target: Option<&str>,
) -> Result<PortalScope> {
    let Access::Portal(key) = access.base() else {
        return Err(Error::Forbidden);
    };
    let portal = app
        .security
        .guard_portal(credential.ok_or(Error::Unauthorized)?)
        .await?;
    let client = app.db.get().await.map_err(Error::from)?;
    bilibi_authz::authorize_key(&**client, portal.scope(), key).await?;
    drop(client);
    if let Some(operation) = access.step_up() {
        let holds = match target {
            Some(target) => match app.step_up.require(portal.scope(), operation, target).await {
                Ok(()) => true,
                Err(Error::StepUpRequired { .. }) => false,
                Err(error) => return Err(error),
            },
            None => app.step_up.holds(portal.scope(), operation).await?,
        };
        if !holds {
            return Err(Error::Forbidden);
        }
    }
    Ok(portal)
}

/// The portal's live side (ADR 16): `/portal/live` with the customer's
/// session, portal routes and panels only, and the portal layout around each
/// reply. No shell panels and no step-up prompt.
pub struct Portal;
impl live::Side for Portal {
    type Context = PortalScope;
    const SHELL: bool = false;
    fn resolve(app: &App, path: &str) -> Result<live::Resolved<PortalScope>> {
        let (bound, params) = routes::resolve_portal(app, path)?;
        Ok(live::Resolved {
            route: bound.route,
            page: bound.page,
            params,
        })
    }
    async fn admit(app: &App, access: Access, token: &str) -> Result<live::Gate<PortalScope>> {
        Ok(live::Gate::Open(Some(
            admit(app, access, Some(token), None).await?,
        )))
    }
    async fn activity(app: &App, token: &str) -> Result<PortalScope> {
        let client = app.db.get().await.map_err(Error::from)?;
        PortalScope::seal(
            app.security
                .sessions
                .activity_portal(&**client, token)
                .await?,
        )
    }
    fn scope(context: &PortalScope) -> &bilibi_security::Scope {
        context.scope()
    }
    async fn chrome(
        app: &App,
        context: &PortalScope,
        path: &str,
        _framed: bool,
        html: &str,
    ) -> Result<String> {
        let site = site_title(app, context).await?;
        layout(app, context, &site, path, true, html).await
    }
    async fn prompt(_app: &App, _context: &PortalScope, _back: &str) -> Option<String> {
        None
    }
}

/// `GET /portal/live`: the portal's live socket. It proves the customer's
/// portal session only; each page and panel it mounts has its own gate, and
/// it acts for nobody else. A portal page is never framed.
pub async fn upgrade(
    State(app): State<App>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let token = token(&headers)?;
    let portal = app.security.guard_portal(&token).await?;
    let policy = live::Policy::read(&app).await?;
    let Some(slot) = app.live.admit(&token, &policy) else {
        return Ok((StatusCode::TOO_MANY_REQUESTS, live::TOO_MANY_SOCKETS).into_response());
    };
    let identity = live::Identity::of(portal.scope());
    Ok(ws
        .max_message_size(live::FRAME_LIMIT)
        .max_frame_size(live::FRAME_LIMIT)
        .on_upgrade(move |socket| {
            live::run::<Portal>(app, token, identity, socket, false, slot, policy)
        }))
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
    let portal = match admit(
        &app,
        bound.route.access,
        credential.as_deref(),
        Some(uri.path()),
    )
    .await
    {
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
    let site = site_title(&app, &portal).await?;
    let html = layout(&app, &portal, &site, uri.path(), page.live, &page.html).await?;
    let html = Html::render(&PortalDocument {
        title,
        site: &site,
        logo: None,
        body: &html,
    })
    .map_err(|_| Error::Internal)?;
    let mut response = axum::response::Html(html.as_str().to_owned()).into_response();
    *response.status_mut() = StatusCode::OK;
    Ok(response)
}

/// The title of the customer's portal: its site's (Core Customer Access),
/// or the tenant's name while the tenant has none.
async fn site_title(app: &App, portal: &PortalScope) -> Result<String> {
    let access: std::sync::Arc<bilibi_customer_access::CustomerAccess> =
        app.services.get(bilibi_customer_access::SERVICE)?;
    Ok(access
        .site_of(portal)
        .await?
        .map_or_else(|| portal.scope().tenant_name.clone(), |site| site.title))
}

/// The portal layout around an owner's page: the portal's title, the portal
/// menu the customer may see (decided like the staff menu, against the
/// customer's effective capabilities, which hold portal keys only) and the
/// account's links. No portal has a logo yet.
async fn layout(
    app: &App,
    portal: &PortalScope,
    title: &str,
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
    flatten(&tree, &mut links);
    // The longest route that is the page or an ancestor of it is current, so
    // a record under a menu page keeps that page marked.
    let current = links
        .iter()
        .enumerate()
        .filter(|(_, link)| {
            path == link.route
                || path
                    .strip_prefix(link.route.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(|(_, link)| link.route.len())
        .map(|(index, _)| index);
    if let Some(index) = current {
        links[index].active = true;
    }
    Html::render(&PortalLayout {
        title,
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

/// Depth-first, the pages of the portal menu as one row of links. A portal
/// menu is a short list its owners arrange, so siblings follow their
/// `order` (then label), not the staff menu's alphabetical rule.
fn flatten(nodes: &[menu::Node], links: &mut Vec<PortalLink>) {
    let mut nodes: Vec<&menu::Node> = nodes.iter().collect();
    nodes.sort_by_key(|node| (node.item.item.order, node.item.item.label.to_lowercase()));
    for node in nodes {
        let item = node.item.item;
        if let Some(route) = item.route {
            links.push(PortalLink {
                id: item.id.replace('.', "-"),
                label: item.label.to_owned(),
                icon: item.icon.unwrap_or("document").to_owned(),
                route: route.to_owned(),
                active: false,
            });
        }
        flatten(&node.children, links);
    }
}

#[derive(serde::Deserialize)]
pub struct SignOut {
    csrf: String,
}
/// `POST /portal/sign-out`: the customer ends its own portal session and the
/// portal cookie is cleared. The staff `/logout` never ends a portal session.
/// A session that already ended (another tab, expiry) counts as signed out:
/// the cookie is cleared and the customer goes to the portal sign-in.
pub async fn sign_out(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<SignOut>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    if let Ok(credential) = token(&headers) {
        match app.user.portal_logout(&credential, &form.csrf).await {
            Ok(()) | Err(Error::Unauthorized) => {}
            Err(error) => return Err(error.into()),
        }
    }
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
        Some(uri.path()),
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
