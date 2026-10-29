//! Discovered HTTP pages and downloads: validate/bind once, authorize before
//! owner dispatch, on every request.
use crate::{origin, token, App, HttpError};
use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, on, MethodFilter},
    Router,
};
use bilibi_database::{Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Download, Method, Moved, Route, DOWNLOADS, MOVED, ROUTES},
    RegistryError, Services,
};
use bilibi_tenancy::PortalScope;
use bilibi_ui::{download::DownloadRequest, page::PageRequest, StatusKind};
use std::{collections::HashMap, sync::Arc};

type PageService = bilibi_ui::page::PageService<bilibi_security::Scope, Error>;
type DownloadService = bilibi_ui::download::DownloadService<bilibi_security::Scope, Error>;
type PortalPageService = bilibi_ui::page::PageService<PortalScope, Error>;
type PortalDownloadService = bilibi_ui::download::DownloadService<PortalScope, Error>;

#[derive(Clone)]
pub struct BoundRoute {
    pub route: &'static Route,
    pub page: Arc<PageService>,
}
/// A `Download` contribution with its owner service. Downloads share the page
/// routes' path rules and may not intersect a page or a host path.
#[derive(Clone)]
pub struct BoundDownload {
    pub download: &'static Download,
    pub service: Arc<DownloadService>,
}

/// A portal page (ADR 16): served from the portal cookie to a customer, whose
/// owner takes a `PortalScope`. Kept apart from the staff routes, so staff
/// page search, the shell and the live socket never see one.
#[derive(Clone)]
pub struct BoundPortalRoute {
    pub route: &'static Route,
    pub page: Arc<PortalPageService>,
}
#[derive(Clone)]
pub struct BoundPortalDownload {
    pub download: &'static Download,
    pub service: Arc<PortalDownloadService>,
}
/// Every discovered page, staff and portal, which share one pattern space.
pub struct Pages {
    pub staff: Vec<BoundRoute>,
    pub portal: Vec<BoundPortalRoute>,
}
pub struct Downloads {
    pub staff: Vec<BoundDownload>,
    pub portal: Vec<BoundPortalDownload>,
}
impl Pages {
    fn patterns(&self) -> impl Iterator<Item = (Method, &'static str)> + '_ {
        self.staff
            .iter()
            .map(|bound| (bound.route.method, bound.route.path))
            .chain(
                self.portal
                    .iter()
                    .map(|bound| (bound.route.method, bound.route.path)),
            )
    }
}
impl Downloads {
    fn patterns(&self) -> impl Iterator<Item = (Method, &'static str)> + '_ {
        self.staff
            .iter()
            .map(|bound| bound.download.path)
            .chain(self.portal.iter().map(|bound| bound.download.path))
            .map(|path| (Method::Get, path))
    }
}

/// Where a portal visitor with no customer session is sent to sign in.
pub const PORTAL_SIGN_IN: &str = "/portal/sign-in";

/// The host paths no discovered route, download or moved path may overlap.
const HOST_PATHS: [&str; 22] = [
    "/",
    "/login",
    "/login/challenge",
    "/logout",
    "/login/sso/{provider}/start",
    "/login/sso/callback",
    "/login/sso/backchannel/{provider}",
    "/preferences",
    crate::shell::COMPANY_PATH,
    "/forgot-password",
    "/reset-password/{token}",
    "/live",
    "/fonts/{name}",
    "/favicon.svg",
    "/favicon.ico",
    "/assets/{name}",
    crate::media::STREAM,
    crate::media::ATTACHMENT,
    crate::pdf::RENDER,
    crate::webhooks::PATH,
    crate::impersonation::ENTER,
    crate::impersonation::LEAVE,
];

/// Router-owned paths cannot be shadowed, even by another method. Literal
/// siblings beat parameters; crossed patterns and parameter renames fail boot.
pub fn bind(services: &Services) -> Result<Pages> {
    let mut bound = Pages {
        staff: Vec::new(),
        portal: Vec::new(),
    };
    for entry in services.catalog()?.entries::<Route>(ROUTES)? {
        let route = entry.get::<Route>()?;
        validate_host_free(entry, route.path)?;
        validate_access(services, entry, route.access)?;
        for previous in bound.patterns() {
            validate_pair(previous, (route.method, route.path))?;
        }
        if route.access.portal() {
            bound.portal.push(BoundPortalRoute {
                route,
                page: services.get(route.service)?,
            });
        } else {
            bound.staff.push(BoundRoute {
                route,
                page: services.get(route.service)?,
            });
        }
    }
    Ok(bound)
}

/// Downloads are bound after the pages and checked against them: a download
/// is a GET like a page, so the two share one pattern space.
pub fn bind_downloads(services: &Services, pages: &Pages) -> Result<Downloads> {
    let mut bound = Downloads {
        staff: Vec::new(),
        portal: Vec::new(),
    };
    for entry in services.catalog()?.entries::<Download>(DOWNLOADS)? {
        let download = entry.get::<Download>()?;
        validate_host_free(entry, download.path)?;
        validate_access(services, entry, download.access)?;
        for previous in pages.patterns().chain(bound.patterns()) {
            validate_pair(previous, (Method::Get, download.path))?;
        }
        if download.access.portal() {
            bound.portal.push(BoundPortalDownload {
                download,
                service: services.get(download.service)?,
            });
        } else {
            bound.staff.push(BoundDownload {
                download,
                service: services.get(download.service)?,
            });
        }
    }
    Ok(bound)
}

fn validate_host_free(entry: &bilibi_module_registry::Entry, path: &str) -> Result<()> {
    validate_pattern(path)?;
    if crate::api::reserved(path) {
        return Err(invalid(format!(
            "{} at {}:{} claims the host agent path {path}",
            entry.id, entry.provenance.source, entry.provenance.line
        )));
    }
    for reserved in HOST_PATHS {
        if intersects(path, reserved) {
            return Err(invalid(format!(
                "{} at {}:{} overlaps host path {reserved}",
                entry.id, entry.provenance.source, entry.provenance.line
            )));
        }
    }
    Ok(())
}
/// Step-up wraps a signed-in access once, for an operation some module
/// declared: a public page has no session to step up, a nested wrapper would
/// mean nothing more, and an undeclared key could never be configured.
fn validate_access(
    services: &Services,
    entry: &bilibi_module_registry::Entry,
    access: Access,
) -> Result<()> {
    let Access::StepUp { operation, inner } = access else {
        return Ok(());
    };
    let at = format!(
        "{} at {}:{}",
        entry.id, entry.provenance.source, entry.provenance.line
    );
    if matches!(inner, Access::Public | Access::StepUp { .. }) {
        return Err(invalid(format!(
            "{at} steps up a public or already stepped-up access"
        )));
    }
    let step_up: Arc<bilibi_base::step_up::StepUp> = services.get(bilibi_base::step_up::SERVICE)?;
    if step_up.declared(operation).is_none() {
        return Err(invalid(format!(
            "{at} steps up {operation}, which no module declares"
        )));
    }
    Ok(())
}
fn invalid(message: String) -> Error {
    RegistryError::Contribution(message).into()
}
fn parameter(segment: &str) -> bool {
    segment.starts_with('{')
}
fn validate_pattern(path: &str) -> Result<()> {
    let mut names = std::collections::HashSet::new();
    if !path.starts_with('/')
        || path.contains(['?', '#', '%', '\\'])
        || path.contains("//")
        || (path.len() > 1 && path.ends_with('/'))
    {
        return Err(invalid(format!("invalid page path {path}")));
    }
    for segment in path.split('/').skip(1) {
        if parameter(segment) {
            let name = segment
                .strip_prefix('{')
                .and_then(|s| s.strip_suffix('}'))
                .unwrap_or("");
            if name.is_empty()
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || !names.insert(name)
            {
                return Err(invalid(format!("invalid page parameter in {path}")));
            }
        } else if segment.contains(['{', '}', '*', ':']) || [".", ".."].contains(&segment) {
            return Err(invalid(format!("invalid page segment in {path}")));
        }
    }
    Ok(())
}
fn intersects(a: &str, b: &str) -> bool {
    let a: Vec<_> = a.split('/').collect();
    let b: Vec<_> = b.split('/').collect();
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(a, b)| a == &b || parameter(a) || parameter(b))
}
fn validate_pair(other: (Method, &str), route: (Method, &str)) -> Result<()> {
    let (other_method, other_path) = other;
    let (method, path) = route;
    // Axum shares parameter nodes even for disjoint methods and suffixes.
    for (a, b) in other_path.split('/').zip(path.split('/')) {
        if parameter(a) && parameter(b) && a != b {
            return Err(invalid(format!(
                "conflicting parameter names: {other_path} and {path}"
            )));
        }
        if a != b && !(parameter(a) && parameter(b)) {
            break;
        }
    }
    if other_method != method || !intersects(other_path, path) {
        return Ok(());
    }
    let mut left = false;
    let mut right = false;
    for (a, b) in other_path.split('/').zip(path.split('/')) {
        left |= !parameter(a) && parameter(b);
        right |= parameter(a) && !parameter(b);
    }
    if left == right {
        return Err(invalid(format!(
            "ambiguous routes: {other_path} and {path}"
        )));
    }
    Ok(())
}

pub fn install(mut router: Router<App>, routes: &Pages, downloads: &Downloads) -> Router<App> {
    for bound in &downloads.portal {
        let bound = bound.clone();
        router = router.route(
            bound.download.path,
            get(
                move |State(app): State<App>,
                      uri: Uri,
                      headers: HeaderMap,
                      Path(path): Path<HashMap<String, String>>,
                      Query(query): Query<HashMap<String, String>>| {
                    crate::portal::download(app, bound, uri, headers, path, query)
                },
            ),
        );
    }
    for bound in &routes.portal {
        let bound = bound.clone();
        router = router.route(
            bound.route.path,
            on(
                method_filter(bound.route.method),
                move |State(app): State<App>,
                      uri: Uri,
                      headers: HeaderMap,
                      Path(path): Path<HashMap<String, String>>,
                      Query(query): Query<HashMap<String, String>>,
                      body: Bytes| {
                    crate::portal::dispatch(app, bound, uri, headers, path, query, body)
                },
            ),
        );
    }
    for bound in &downloads.staff {
        let bound = bound.clone();
        router = router.route(
            bound.download.path,
            get(
                move |State(app): State<App>,
                      uri: Uri,
                      headers: HeaderMap,
                      Path(path): Path<HashMap<String, String>>,
                      Query(query): Query<HashMap<String, String>>| {
                    download(app, bound, uri, headers, path, query)
                },
            ),
        );
    }
    for bound in &routes.staff {
        let bound = bound.clone();
        router = router.route(
            bound.route.path,
            on(
                method_filter(bound.route.method),
                move |State(app): State<App>,
                      uri: Uri,
                      headers: HeaderMap,
                      Path(path): Path<HashMap<String, String>>,
                      Query(query): Query<HashMap<String, String>>,
                      body: Bytes| {
                    dispatch(app, bound, uri, headers, path, query, body)
                },
            ),
        );
    }
    router
}
fn method_filter(method: Method) -> MethodFilter {
    match method {
        Method::Get => MethodFilter::GET,
        Method::Post => MethodFilter::POST,
        Method::Put => MethodFilter::PUT,
        Method::Patch => MethodFilter::PATCH,
        Method::Delete => MethodFilter::DELETE,
    }
}

/// Retired page paths, bound after the pages and downloads: each claims no
/// host path, collides with no page or download, and lands on a GET page whose
/// pattern is exactly its `to`, so a rename that drops the new page fails boot
/// instead of redirecting into a 404.
pub fn bind_moved(
    services: &Services,
    pages: &Pages,
    downloads: &Downloads,
) -> Result<Vec<&'static Moved>> {
    let mut bound: Vec<&'static Moved> = Vec::new();
    for entry in services.catalog()?.entries::<Moved>(MOVED)? {
        let moved = entry.get::<Moved>()?;
        validate_host_free(entry, moved.from)?;
        validate_pattern(moved.to)?;
        let taken = pages
            .patterns()
            .chain(downloads.patterns())
            .chain(bound.iter().map(|other| (Method::Get, other.from)));
        for other in taken {
            validate_pair(other, (Method::Get, moved.from))?;
        }
        if !pages
            .patterns()
            .any(|(method, path)| method == Method::Get && path == moved.to)
        {
            return Err(invalid(format!(
                "{} at {}:{} moves to {}, which no page serves",
                entry.id, entry.provenance.source, entry.provenance.line, moved.to
            )));
        }
        bound.push(moved);
    }
    Ok(bound)
}

/// Each moved path answers a GET with a permanent redirect to its page,
/// carrying the parameters' raw segments and the query string unchanged.
/// No access check: the destination page applies its own.
pub fn install_moved(mut router: Router<App>, moved: &[&'static Moved]) -> Router<App> {
    for moved in moved {
        let moved = *moved;
        router = router.route(
            moved.from,
            get(move |uri: Uri| async move {
                axum::response::Redirect::permanent(&moved_destination(moved, &uri))
            }),
        );
    }
    router
}

fn moved_destination(moved: &Moved, uri: &Uri) -> String {
    let values: HashMap<&str, &str> = moved
        .from
        .split('/')
        .zip(uri.path().split('/'))
        .filter(|(pattern, _)| parameter(pattern))
        .collect();
    let mut destination = moved
        .to
        .split('/')
        .map(|segment| {
            if parameter(segment) {
                values.get(segment).copied().unwrap_or_default()
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/");
    if let Some(query) = uri.query() {
        destination.push('?');
        destination.push_str(query);
    }
    destination
}

/// What a request's access check concluded.
pub enum Admission {
    /// Through, with the proven scope (none for a public route).
    Admitted(Option<bilibi_security::Scope>),
    /// A step-up route outside development, and this session has not
    /// confirmed access within the window: a GET goes to the prompt first.
    StepUp(bilibi_security::Scope),
}

/// `target` names the HTTP navigation being admitted: its step-up check is
/// the audited one that also extends the window. Without it (a live socket's
/// recheck) the window is only read.
pub async fn admit(
    app: &App,
    access: Access,
    credential: Option<&str>,
    target: Option<&str>,
) -> Result<Admission> {
    let scope = check_base(app, access.base(), credential).await?;
    let Some(operation) = access.step_up() else {
        return Ok(Admission::Admitted(scope));
    };
    let scope = scope.ok_or(Error::Forbidden)?;
    let holds = match target {
        Some(target) => match app.step_up.require(&scope, operation, target).await {
            Ok(()) => true,
            Err(Error::StepUpRequired { .. }) => false,
            Err(error) => return Err(error),
        },
        None => app.step_up.holds(&scope, operation).await?,
    };
    Ok(if holds {
        Admission::Admitted(Some(scope))
    } else {
        Admission::StepUp(scope)
    })
}

/// [`admit`] for a caller with no prompt to send anyone to (an embed, a
/// panel): a step-up still owed is a refusal.
pub async fn check_access(
    app: &App,
    access: Access,
    credential: Option<&str>,
) -> Result<Option<bilibi_security::Scope>> {
    match admit(app, access, credential, None).await? {
        Admission::Admitted(scope) => Ok(scope),
        Admission::StepUp(_) => Err(Error::Forbidden),
    }
}

async fn check_base(
    app: &App,
    access: Access,
    credential: Option<&str>,
) -> Result<Option<bilibi_security::Scope>> {
    if matches!(access, Access::Public) {
        return Ok(None);
    }
    let scope = app
        .security
        .guard(credential.ok_or(Error::Unauthorized)?)
        .await?;
    let client = app.db.get().await.map_err(Error::from)?;
    match access {
        Access::Capability(key) => bilibi_authz::authorize_key(&**client, &scope, key).await?,
        Access::Operator => {
            bilibi_tenancy::prove_operator(&**client, &scope).await?;
        }
        Access::OperatorCapability(key) => {
            bilibi_authz::authorize_operator(&**client, &scope, key).await?;
        }
        Access::AnyOf(keys) => bilibi_authz::authorize_any(&**client, &scope, keys).await?,
        Access::Owner => {
            bilibi_tenancy::prove_owner(&**client, &scope).await?;
        }
        Access::SignedIn => {}
        // `base()` unwrapped the one wrapper boot allows.
        Access::StepUp { .. } => return Err(Error::Internal),
        // Portal pages are bound apart; a staff session never opens one.
        Access::Portal(_) => return Err(Error::Forbidden),
        Access::Public => unreachable!(),
    }
    Ok(Some(scope))
}

/// Sends a browser that owes a step-up to the prompt (or to enrollment),
/// carrying the address it asked for so it comes back there.
async fn step_up_redirect(
    app: &App,
    scope: &bilibi_security::Scope,
    uri: &Uri,
) -> Result<Response> {
    let back = uri
        .path_and_query()
        .map_or(uri.path(), |address| address.as_str());
    let destination = app.step_up.prompt(scope, back).await?;
    Ok(axum::response::Redirect::to(&destination).into_response())
}

async fn dispatch(
    app: App,
    bound: BoundRoute,
    uri: Uri,
    headers: HeaderMap,
    path: HashMap<String, String>,
    query: HashMap<String, String>,
    body: Bytes,
) -> std::result::Result<Response, HttpError> {
    let credential = token(&headers).ok();
    let scope = match admit(
        &app,
        bound.route.access,
        credential.as_deref(),
        Some(uri.path()),
    )
    .await
    {
        Err(Error::Unauthorized) if bound.route.method == Method::Get => {
            return Ok(crate::auth::sign_in_redirect(&app, credential.is_some()))
        }
        Ok(Admission::StepUp(scope)) if bound.route.method == Method::Get => {
            return Ok(step_up_redirect(&app, &scope, &uri).await?)
        }
        Ok(Admission::StepUp(_)) => return Err(Error::Forbidden.into()),
        Ok(Admission::Admitted(scope)) => scope,
        Err(error) => return Err(error.into()),
    };
    if bound.route.method != Method::Get {
        origin(&app, &headers)?;
        if let Some(scope) = &scope {
            if headers.get("x-csrf-token").and_then(|h| h.to_str().ok())
                != Some(scope.csrf.as_str())
            {
                return Err(Error::Forbidden.into());
            }
        }
    }
    let framed = matches!(
        headers.get("sec-fetch-dest").and_then(|h| h.to_str().ok()),
        Some("iframe" | "frame")
    );
    if framed && !bound.route.framed {
        return Err(Error::Forbidden.into());
    }
    let rendered = bound
        .page
        .0
        .render(PageRequest {
            operation: bound.route.operation,
            scope: scope.as_ref(),
            path: &path,
            query: &query,
            body: &body,
            framed,
        })
        .await;
    let page = match rendered {
        // The page's own facade asked for step-up while rendering.
        Err(Error::StepUpRequired { reason, .. })
            if bound.route.method == Method::Get && reason != "impersonating" =>
        {
            let scope = scope.as_ref().ok_or(Error::Forbidden)?;
            return Ok(step_up_redirect(&app, scope, &uri).await?);
        }
        Err(Error::NotFound) if bound.route.method == Method::Get => {
            return match crate::auth::missing_record_redirect(&app, bound.route.path) {
                Some(response) => Ok(response),
                None => Err(Error::NotFound.into()),
            }
        }
        page => page?,
    };
    if let Some(destination) = bound.page.0.redirect(bound.route.operation) {
        return Ok(axum::response::Redirect::to(destination).into_response());
    }
    let shell = crate::shell::render(
        &app,
        scope.as_ref(),
        uri.path(),
        framed,
        page.live,
        &page.html,
        topbar_actions(&bound, framed),
    )
    .await?;
    if let (Some(credential), Some(_)) = (&credential, scope) {
        let client = app.db.get().await.map_err(Error::from)?;
        app.security
            .sessions
            .activity(&**client, credential)
            .await?;
    }
    let title = page
        .title
        .as_deref()
        .or_else(|| bound.page.0.title(bound.route.operation))
        .unwrap_or("Bilibi");
    // A missing record or a refused action elsewhere sent the visitor here with
    // its message (a one-shot cookie), or a live action redirected here with a
    // success of its own (held by `landing`).
    let waiting = crate::auth::waiting_flash(&headers).filter(|_| !framed);
    let landed = match (&waiting, credential.as_deref()) {
        (None, Some(credential)) if !framed && crate::auth::navigating(&headers) => {
            app.landing.take(credential, uri.path())
        }
        _ => None,
    };
    let html = match (waiting, &landed) {
        (Some(message), _) => flash_group(&shell.html, StatusKind::Error, message)?,
        (None, Some((kind, message))) => flash_group(&shell.html, *kind, message)?,
        (None, None) => shell.html,
    };
    let mut response = crate::document_themed(title, &html, shell.theme)?.into_response();
    if waiting.is_some() {
        crate::auth::clear_flash(&mut response);
    }
    if framed {
        response.headers_mut().insert(
            header::HeaderName::from_static("x-bilibi-framed"),
            header::HeaderValue::from_static("true"),
        );
    }
    *response.status_mut() = StatusCode::OK;
    Ok(response)
}

/// A download: access is proven for this request, then the owner reads the
/// bytes through its facade, then the file's own headers go out. Nothing is
/// rendered into the shell and no missing-record redirect applies: a refused
/// or missing file is a bare 403 or 404, as Bilimbi's controllers answer.
async fn download(
    app: App,
    bound: BoundDownload,
    uri: Uri,
    headers: HeaderMap,
    path: HashMap<String, String>,
    query: HashMap<String, String>,
) -> std::result::Result<Response, HttpError> {
    let credential = token(&headers).ok();
    let scope = match admit(
        &app,
        bound.download.access,
        credential.as_deref(),
        Some(uri.path()),
    )
    .await
    {
        Err(Error::Unauthorized) => {
            return Ok(crate::auth::sign_in_redirect(&app, credential.is_some()))
        }
        Ok(Admission::StepUp(scope)) => return Ok(step_up_redirect(&app, &scope, &uri).await?),
        Ok(Admission::Admitted(scope)) => scope,
        Err(error) => return Err(error.into()),
    };
    // Registration refuses a public download; a scope is always proven here.
    let (Some(scope), Some(credential)) = (scope, credential) else {
        return Err(Error::Forbidden.into());
    };
    let file = bound
        .service
        .0
        .serve(DownloadRequest {
            operation: bound.download.operation,
            scope: &scope,
            path: &path,
            query: &query,
        })
        .await?;
    // A download is user activity; the guard above never renewed the session.
    let client = app.db.get().await.map_err(Error::from)?;
    app.security
        .sessions
        .activity(&**client, &credential)
        .await?;
    let delivery = file.headers();
    let mut response = (StatusCode::OK, file.bytes).into_response();
    for (name, value) in delivery {
        // `FileBody::headers` cleans every value; an unrepresentable one is a
        // host fault, not a reason to send the file under default headers.
        let value = HeaderValue::from_str(&value).map_err(|_| Error::Internal)?;
        response
            .headers_mut()
            .insert(HeaderName::from_static(name), value);
    }
    Ok(response)
}

/// The page with one flash group after it, the outlet for a message that
/// arrived with the visitor.
fn flash_group(page: &str, kind: StatusKind, message: &str) -> Result<String> {
    let mut flash = bilibi_ui::FlashMap::default();
    match kind {
        StatusKind::Error => flash.error = message,
        StatusKind::Warning => flash.warning = message,
        StatusKind::Success => flash.success = message,
        StatusKind::Info => flash.info = message,
    }
    let group = bilibi_ui::Html::render(&bilibi_ui::FloatingFlashGroup {
        id: "flash-redirect",
        flash,
    })
    .map_err(|_| Error::Internal)?;
    Ok(format!("{page}{group}"))
}

/// Resolve the same discovered GET routes as HTTP, with literal siblings first.
pub fn resolve(app: &App, path: &str) -> Result<(BoundRoute, HashMap<String, String>)> {
    if !path.starts_with('/')
        || path.len() > 2048
        || path.contains(['?', '#', '\\'])
        || path.contains("//")
    {
        return Err(Error::Invalid);
    }
    let segments: Vec<_> = path.split('/').map(decode_segment).collect::<Result<_>>()?;
    let mut matches = Vec::new();
    for bound in app
        .routes
        .staff
        .iter()
        .filter(|b| b.route.method == Method::Get)
    {
        let pattern: Vec<_> = bound.route.path.split('/').collect();
        if pattern.len() != segments.len() {
            continue;
        }
        let mut params = HashMap::new();
        let mut specificity = 0;
        let mut valid = true;
        for (pattern, value) in pattern.into_iter().zip(&segments) {
            if parameter(pattern) {
                if value.is_empty() || [".", ".."].contains(&value.as_str()) {
                    valid = false;
                    break;
                }
                params.insert(pattern[1..pattern.len() - 1].into(), value.clone());
            } else if pattern == value {
                specificity += 1;
            } else {
                valid = false;
                break;
            }
        }
        if valid {
            matches.push((specificity, bound.clone(), params));
        }
    }
    matches
        .into_iter()
        .max_by_key(|(score, _, _)| *score)
        .map(|(_, bound, params)| (bound, params))
        .ok_or(Error::NotFound)
}

fn decode_segment(segment: &str) -> Result<String> {
    let mut bytes = Vec::with_capacity(segment.len());
    let mut input = segment.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = input
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or(Error::Invalid)?;
            let low = input
                .next()
                .and_then(|v| (v as char).to_digit(16))
                .ok_or(Error::Invalid)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    let value = String::from_utf8(bytes).map_err(|_| Error::Invalid)?;
    if value.contains(['/', '\\']) || value.chars().any(char::is_control) {
        return Err(Error::Invalid);
    }
    Ok(value)
}

/// The page's top-bar controls for the shell; a framed render has no top bar.
pub fn topbar_actions(bound: &BoundRoute, framed: bool) -> bilibi_ui::Html {
    if framed {
        return bilibi_ui::Html::default();
    }
    bound
        .page
        .0
        .topbar_actions(bound.route.operation)
        .unwrap_or_default()
}
