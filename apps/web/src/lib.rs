//! Axum host: routes and downloads, cookie/Origin boundary, and one server-held
//! page per socket.
mod api;
mod auth;
mod composition;
mod create_owner;
mod errors;
mod impersonation;
mod landing;
mod live;
mod mcp;
mod media;
mod pdf;
mod perf;
mod portal;
mod portal_auth;
mod routes;
mod scim;
mod shell;
mod two_factor_reset;
mod webhooks;
use askama::Template;
use axum::{
    extract::{DefaultBodyLimit, Form, Query, State, WebSocketUpgrade},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{Html, IntoResponse, Redirect, Response},
    routing::{get, post},
    Router,
};
use bilibi_database::{Db, Error, Result};
use bilibi_ui::Document;
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

pub use create_owner::create_owner;
pub use two_factor_reset::two_factor_reset;

#[derive(Clone)]
pub struct App {
    routes: Arc<routes::Pages>,
    downloads: Arc<routes::Downloads>,
    moved: Arc<Vec<&'static bilibi_module_registry::contributions::Moved>>,
    pub db: Db,
    pub origin: Arc<str>,
    settings: Arc<bilibi_base::settings::Settings>,
    pub step_up: Arc<bilibi_base::step_up::StepUp>,
    /// The host the server listens on, shown in the status bar;
    /// the serving command sets it from `BIND_ADDR`.
    pub listen_address: Option<Arc<str>>,
    pub services: Arc<bilibi_module_registry::Services>,
    pub company: Arc<bilibi_company::CompanyService>,
    pub user: Arc<bilibi_user::UserService>,
    pub security: Arc<bilibi_security::Security>,
    pub renderer: Arc<bilibi_ui::Renderer>,
    pub menu: Arc<bilibi_base::menu::Menu>,
    pub display: Arc<bilibi_base::datetime::DisplayPolicy>,
    pub mutations: Arc<bilibi_database::MutationBoundary>,
    pub perf: Arc<bilibi_base::perf::Recorder>,
    /// Messages a live redirect carries to the page it lands on.
    pub landing: Arc<landing::Landing>,
    /// The live boundary's socket and message counts (`live::Limits`).
    pub(crate) live: Arc<live::Limits>,
    api: Arc<api::Edge>,
    webhooks: Arc<webhooks::Boundary>,
}
impl App {
    /// Which instance this is, with the operator's label and colour
    /// (`bilibi_base::system::Instance::resolve`): what every page, consent
    /// page and agent answer names.
    pub async fn instance(&self) -> Result<bilibi_base::system::Instance> {
        let client = self.db.get().await?;
        bilibi_base::system::Instance::resolve(&self.settings, &**client).await
    }
    /// Public OAuth origin policy. Rechecked at issuance so a live role change
    /// cannot keep issuing tokens on a development HTTP origin.
    /// `Ok(false)` is a policy refusal; instance settings that cannot be read
    /// are an `Err`, so callers can report that separately.
    pub async fn secure_origin(&self) -> Result<bool> {
        let instance = self.instance().await?;
        let uri: axum::http::Uri = self.origin.parse().map_err(|_| Error::Invalid)?;
        let host = uri.host().ok_or(Error::Invalid)?.trim_matches(['[', ']']);
        let loopback = host == "localhost"
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback());
        let bare = uri
            .authority()
            .is_some_and(|authority| !authority.as_str().contains('@'))
            && uri.path_and_query().is_none_or(|path| path.as_str() == "/");
        Ok(bare
            && (uri.scheme_str() == Some("https")
                || (instance.role == bilibi_base::system::Role::Dev
                    && uri.scheme_str() == Some("http")
                    && loopback)))
    }
    pub fn new(db: Db, origin: &str) -> Result<Self> {
        let modules = modules()?;
        let mut services = bilibi_module_registry::Services::default();
        services.provide("bootstrap.database", Arc::new(db))?;
        // What this program is made of, for Base Software's status and reviews.
        services.provide("bootstrap.composition", Arc::new(composition()))?;
        services.install(&modules)?;
        let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
        let company = services.get("core/company.companies")?;
        let user = services.get("core/user.accounts")?;
        let security = services.get("base/security.guard")?;
        let step_up: Arc<bilibi_base::step_up::StepUp> =
            services.get(bilibi_base::step_up::SERVICE)?;
        let settings = services.get("base/settings.store")?;
        let renderer = services.get("base/ui.renderer")?;
        for entry in services
            .catalog()?
            .entries::<bilibi_base::menu::MenuItem>(bilibi_base::menu::ITEMS)?
        {
            let item = entry.get::<bilibi_base::menu::MenuItem>()?;
            if let Some(icon) = item.icon {
                bilibi_ui::Icons::check_menu(icon, entry.provenance.module);
            }
        }
        let menu = bilibi_base::menu::Menu::from_catalog(services.catalog()?)?;
        let display = services.get("base/datetime.display")?;
        let mutations = services.get("base/database.mutations")?;
        let perf = services.get("base/perf.recorder")?;
        for dropped in menu.dropped() {
            eprintln!("warning: {dropped}");
        }
        let routes = Arc::new(routes::bind(&services)?);
        let downloads = Arc::new(routes::bind_downloads(&services, &routes)?);
        let moved = Arc::new(routes::bind_moved(&services, &routes, &downloads)?);
        let api = Arc::new(api::Edge::new(&services)?);
        let webhooks = Arc::new(webhooks::Boundary::bind(&services)?);
        Ok(Self {
            routes,
            downloads,
            moved,
            db,
            origin: origin.into(),
            settings,
            step_up,
            listen_address: None,
            company,
            user,
            security,
            renderer,
            menu: Arc::new(menu),
            display,
            mutations,
            perf,
            landing: Arc::default(),
            live: Arc::default(),
            api,
            webhooks,
            services: Arc::new(services),
        })
    }
}

pub fn modules() -> Result<Vec<&'static bilibi_module_registry::Module>> {
    bilibi_base::link();
    bilibi_core::link();
    bilibi_domains::link();
    bilibi_extensions::link();
    Ok(bilibi_module_registry::discover()?)
}

/// The product binary's boot check: the linked Domain and Extension descriptors
/// must be exactly the mounted crates discovery found. Examples that link their
/// own reference Extensions call `modules` instead.
pub fn verify_composition() -> Result<()> {
    Ok(bilibi_module_registry::verify_composition(
        &modules()?,
        bilibi_domains::MODULES,
        bilibi_extensions::MODULES,
    )?)
}

/// What this program is made of (docs/versions.md): the Platform version, its
/// commit and release tag, and each mounted Domain or Extension with its
/// version, as `scripts/discover-modules.py --pin` recorded them at build time.
pub fn composition() -> bilibi_module_registry::Composition {
    composition::composition()
}

/// `bilibi-web status`: the composition as plain text, one line per part.
pub fn status() -> String {
    use bilibi_module_registry::{Layer, PinState};
    let made = composition();
    let mut text = format!("Bilibi {}", made.version);
    match (&made.pin, made.tag, made.commit) {
        (PinState::Pinned, Some(tag), Some(commit)) => {
            text.push_str(&format!(", release {tag}, commit {commit}"))
        }
        (PinState::Pinned, None, Some(commit)) => {
            text.push_str(&format!(", unreleased, commit {commit}"))
        }
        (_, _, Some(commit)) => text.push_str(&format!(", built from commit {commit}")),
        (_, _, None) => {}
    }
    text.push('\n');
    text.push_str(match &made.pin {
        PinState::Pinned => "Pin: .local/composition, compiled in at build time\n",
        PinState::Unpinned => {
            "Pin: none; built without scripts/discover-modules.py --pin, so Domain versions are unknown\n"
        }
        PinState::Stale(_) => "",
    });
    if let PinState::Stale(reason) = &made.pin {
        text.push_str(&format!("Pin: stale, {reason}\n"));
    }
    for (layer, heading) in [
        (Layer::Domains, "Domains"),
        (Layer::Extensions, "Extensions"),
    ] {
        let mounted: Vec<_> = made
            .containers
            .iter()
            .filter(|container| container.layer == layer)
            .collect();
        if mounted.is_empty() {
            text.push_str(&format!("{heading}: none mounted\n"));
            continue;
        }
        text.push_str(&format!("{heading}:\n"));
        for container in mounted {
            let at = match (container.version, container.commit) {
                (Some(version), Some(commit)) => format!("{version}, commit {commit}"),
                (None, Some(commit)) => format!("untagged, commit {commit}"),
                (_, None) => "unpinned".to_owned(),
            };
            text.push_str(&format!(
                "  {} {at}, modules: {}\n",
                container.id,
                container.modules.join(", ")
            ));
        }
    }
    text
}

pub async fn migrate(db: &Db) -> Result<bilibi_database::Migrated> {
    bilibi_database::migrate(db, &modules()?).await
}

/// `tenant-provision` and `platform-provision` commands (Bilimbi
/// `mix bilimbi.tenant.provision` and `mix bilimbi.platform.provision`):
/// `--tenant-name`, `--company-name` and `--company-code`, the tenant name
/// defaulting to the company name for the platform operator. Ids are
/// generated by PostgreSQL; re-running the platform command resolves the
/// existing operator and company instead of creating a second one.
pub async fn provision(db: &Db, command: &str, args: &[String]) -> Result<()> {
    use bilibi_core::company::{self as primary, NewCompany, ProvisionOutcome};
    let mut options: HashMap<&str, String> = HashMap::new();
    let mut pairs = args.iter();
    while let Some(flag) = pairs.next() {
        let value = pairs.next().map(|value| value.trim().to_owned());
        match (flag.as_str(), value) {
            (key @ ("--tenant-name" | "--company-name" | "--company-code"), Some(value))
                if !value.is_empty() =>
            {
                options.insert(key, value);
            }
            _ => {
                eprintln!("usage: bilibi-web {command} [--tenant-name NAME] --company-name NAME --company-code CODE");
                return Err(Error::Invalid);
            }
        }
    }
    let company = NewCompany {
        name: options.remove("--company-name").unwrap_or_default(),
        code: options.remove("--company-code"),
        ..NewCompany::default()
    };
    let tenant_name = options.remove("--tenant-name").unwrap_or_default();
    if company.name.is_empty()
        || company.code.is_none()
        || (command == "tenant-provision" && tenant_name.is_empty())
    {
        eprintln!("usage: bilibi-web {command} [--tenant-name NAME] --company-name NAME --company-code CODE");
        return Err(Error::Invalid);
    }
    let outcome = if command == "tenant-provision" {
        primary::provision_tenant(db, &tenant_name, &company).await?
    } else {
        primary::provision_platform_operator(db, &tenant_name, &company).await?
    };
    match outcome {
        ProvisionOutcome::Provisioned(result) => {
            let state = |created: bool| if created { "created" } else { "existing" };
            println!(
                "{}: tenant {} ({}), primary company {} “{}” ({}).",
                if command == "tenant-provision" {
                    "Tenant ready"
                } else {
                    "Platform operator ready"
                },
                result.tenant.id,
                state(result.tenant_created),
                result.company.id,
                result.company.name,
                state(result.company_created)
            );
            Ok(())
        }
        ProvisionOutcome::Refused(errors) => {
            for error in errors {
                eprintln!("{}: {}", error.field, error.message);
            }
            Err(Error::Invalid)
        }
    }
}

/// `media-cleanup` command: one tenant's abandoned private attachment drafts.
pub async fn media_cleanup(db: &Db, tenant: i64, hours: u64) -> Result<()> {
    media::cleanup(db, tenant, hours).await
}

pub fn router(app: App) -> Router {
    let perf = app.perf.clone();
    let pages = routes::install(Router::new(), &app.routes, &app.downloads);
    routes::install_moved(pages, &app.moved)
        .merge(api::router(app.clone()))
        .merge(webhooks::router())
        .merge(scim::router())
        .route("/", get(auth::home).post(auth::login))
        .route("/login", get(auth::home).post(auth::login))
        .route(
            "/login/challenge",
            get(auth::challenge_page).post(auth::challenge),
        )
        .route(
            "/forgot-password",
            get(auth::forgot_page).post(auth::forgot),
        )
        .route(
            "/reset-password/{token}",
            get(auth::reset_page).post(auth::reset),
        )
        .route("/login/sso/{provider}/start", get(auth::sso_start))
        .route("/login/sso/callback", get(auth::sso_callback))
        .route(
            "/login/sso/backchannel/{provider}",
            post(auth::sso_backchannel),
        )
        .route("/logout", post(logout))
        .route("/portal/sign-in", get(portal_auth::sign_in_root))
        .route(
            "/portal/sign-in/{site}",
            get(portal_auth::sign_in_page).post(portal_auth::sign_in),
        )
        .route(
            "/portal/forgot/{site}",
            get(portal_auth::forgot_page).post(portal_auth::forgot),
        )
        .route(
            "/portal/reset/{site}/{token}",
            get(portal_auth::reset_page).post(portal_auth::reset),
        )
        .route(
            "/portal/accept/{site}/{token}",
            get(portal_auth::accept_page).post(portal_auth::accept),
        )
        .route(
            "/portal/register/{site}",
            get(portal_auth::register_page).post(portal_auth::register),
        )
        .route(
            "/portal/verify/{site}/{token}",
            get(portal_auth::verify_page).post(portal_auth::verify),
        )
        .route("/portal/sign-out", post(portal::sign_out))
        .route("/portal/live", get(portal::upgrade))
        .route(impersonation::LEAVE, post(impersonation::leave))
        .route(impersonation::ENTER, post(impersonation::enter))
        .route("/preferences", post(shell::save))
        .route(shell::COMPANY_PATH, post(shell::choose_company))
        .route("/api/pins", get(shell::pins))
        .route("/api/pins/toggle", post(shell::toggle_pin))
        .route("/api/pins/reorder", post(shell::reorder_pins))
        .route("/live", get(upgrade))
        .route(media::STREAM, get(media::stream))
        .route(media::ATTACHMENT, get(media::attachment))
        .route(pdf::RENDER, get(pdf::render))
        .route("/fonts/{name}", get(embedded_asset))
        .route("/assets/{name}", get(embedded_asset))
        .route("/assets/ui-components.html", get(components_specimen))
        .route("/assets/ui-feedback.html", get(feedback_specimen))
        .route("/favicon.svg", get(embedded_asset))
        .route("/favicon.ico", get(embedded_asset))
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), errors::pages))
        .layer(middleware::from_fn(response_headers))
        .layer(
            tower_http::compression::CompressionLayer::new()
                .quality(tower_http::CompressionLevel::Precise(4))
                .compress_when(tower_http::compression::predicate::Predicate::and(
                    tower_http::compression::DefaultPredicate::new(),
                    is_textual,
                )),
        )
        .layer(middleware::from_fn_with_state(perf, perf::requests))
        .with_state(app)
}

/// Only text pages and assets compress; any response with a
/// `Content-Disposition` (attachments, inline media, release bundles) and any
/// non-text body keeps its bytes and `Content-Length`.
fn is_textual(
    _status: StatusCode,
    _version: axum::http::Version,
    headers: &HeaderMap,
    _extensions: &axum::http::Extensions,
) -> bool {
    if headers.contains_key(header::CONTENT_DISPOSITION) {
        return false;
    }
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return false;
    };
    let Ok(value) = value.to_str() else {
        return false;
    };
    let essence = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/")
        || matches!(
            essence.as_str(),
            "application/json" | "application/javascript" | "application/xml"
        )
        || essence.ends_with("+json")
        || essence.ends_with("+xml")
}

/// The shared component specimen: the screenshot baseline for the primitives
/// in `bilibi_ui`, served like the asset specimen and not a product page.
async fn components_specimen() -> std::result::Result<Html<String>, HttpError> {
    let specimen = bilibi_ui::ComponentsSpecimen::new().map_err(|_| Error::Internal)?;
    Ok(Html(render(specimen)?))
}

/// The feedback, dialog and tab specimen: the screenshot baseline for those
/// components, served beside the primitives specimen.
async fn feedback_specimen() -> std::result::Result<Html<String>, HttpError> {
    Ok(Html(render(bilibi_ui::FeedbackSpecimen::new())?))
}

/// Only an allowlisted embedded asset with its current content hash earns a
/// public immutable policy. Pages (including specimens) keep the host default.
async fn embedded_asset(
    uri: axum::http::Uri,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    let Some(asset) = bilibi_ui::Assets::get(uri.path()) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let version = query.get("v");
    if version.is_some_and(|value| value != asset.version()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let mut response = (
        [(header::CONTENT_TYPE, asset.content_type)],
        asset.bytes.as_ref(),
    )
        .into_response();
    if version.is_some() && !asset.content_type.starts_with("text/html") {
        response.headers_mut().insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static("public, max-age=31536000, immutable"),
        );
    }
    response
}

/// Defaults only: a handler that sets its own policy, as signed media streams
/// do with a sandboxing CSP, keeps it.
/// A server-side failure is also logged here with its request line: the
/// page shows only the error's public text.
async fn response_headers(request: axum::extract::Request, next: Next) -> Response {
    let path = request.uri().path();
    let line = format!("{} {}", request.method(), redacted(path));
    // A credential link's page never sends its path on: `strict-origin`
    // names only this origin. Not `no-referrer`, which would make the
    // browser post the page's own form with `Origin: null`, which the
    // origin check refuses.
    let referrer = if credential_path(path) {
        "strict-origin"
    } else {
        "same-origin"
    };
    let mut response = next.run(request).await;
    if let Some(Failure(cause)) = response.extensions_mut().remove::<Failure>() {
        eprintln!("error: {line} failed: {cause}");
    }
    for (key,value) in [
        ("cache-control","no-store"),
        ("x-content-type-options","nosniff"),
        ("referrer-policy",referrer),
        ("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'self'; form-action 'self'; base-uri 'none'"),
    ] { response.headers_mut().entry(axum::http::HeaderName::from_static(key)).or_insert(HeaderValue::from_static(value)); }
    response
}

/// The emailed credential links (a password reset, a portal invitation, a
/// request's confirmation): the
/// `{token}` segment is a bearer secret, so it is never logged, shown in a
/// debug report or sent on as a referrer.
const CREDENTIAL_PATHS: [&str; 4] = [
    "/reset-password/{token}",
    "/portal/reset/{site}/{token}",
    "/portal/accept/{site}/{token}",
    "/portal/verify/{site}/{token}",
];
/// `path` with a credential link's token replaced; any other path as it is.
pub(crate) fn redacted(path: &str) -> std::borrow::Cow<'_, str> {
    let given: Vec<&str> = path.split('/').collect();
    for pattern in CREDENTIAL_PATHS {
        let shape: Vec<&str> = pattern.split('/').collect();
        let matches = shape.len() == given.len()
            && shape
                .iter()
                .zip(&given)
                .all(|(shape, given)| shape.starts_with('{') || shape == given);
        if matches {
            return shape
                .iter()
                .zip(&given)
                .map(|(shape, given)| {
                    if *shape == "{token}" {
                        "[redacted]"
                    } else {
                        given
                    }
                })
                .collect::<Vec<_>>()
                .join("/")
                .into();
        }
    }
    path.into()
}
/// Whether `path` is an emailed credential link.
pub(crate) fn credential_path(path: &str) -> bool {
    matches!(redacted(path), std::borrow::Cow::Owned(_))
}

/// A failed request. API clients read the status and text; `errors::pages`
/// turns the same response into an error page for browsers.
struct HttpError(Error, Option<std::sync::Arc<errors::Failure>>);
/// The cause of a server-side `HttpError`, for `response_headers` to log.
#[derive(Clone)]
struct Failure(String);
/// Whether an error is the server's fault (logged), not the request's.
pub(crate) fn server_fault(error: &Error) -> bool {
    !matches!(
        error,
        Error::Unauthorized
            | Error::Forbidden
            | Error::NotFound
            | Error::Invalid
            | Error::Refused(_)
            | Error::Conflict
            | Error::StepUpRequired { .. }
    )
}
impl From<Error> for HttpError {
    fn from(error: Error) -> Self {
        let failure = errors::Failure::capture(&error);
        Self(error, failure)
    }
}
impl IntoResponse for HttpError {
    fn into_response(self) -> Response {
        let mut response = (errors::status_of(&self.0), self.0.to_string()).into_response();
        if server_fault(&self.0) {
            response
                .extensions_mut()
                .insert(Failure(format!("{:?}", self.0)));
        }
        if let Some(failure) = self.1 {
            response.extensions_mut().insert(failure);
        }
        response
    }
}
fn render<T: Template>(template: T) -> Result<String> {
    template.render().map_err(|_| Error::Internal)
}
fn document_titled(title: &str, body: &str) -> Result<Html<String>> {
    document_themed(title, body, "system")
}
fn document_themed(title: &str, body: &str, theme: &str) -> Result<Html<String>> {
    Ok(Html(render(Document { title, body, theme })?))
}
fn token(headers: &HeaderMap) -> Result<String> {
    headers
        .get(header::COOKIE)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| {
            h.split(';').find_map(|cookie| {
                cookie
                    .trim()
                    .strip_prefix("bilibi_session=")
                    .map(str::to_owned)
            })
        })
        .filter(|s| s.len() == 32 && s.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or(Error::Unauthorized)
}
fn origin(app: &App, headers: &HeaderMap) -> Result<()> {
    if headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) == Some(&app.origin) {
        Ok(())
    } else {
        Err(Error::Forbidden)
    }
}
#[derive(Deserialize)]
struct Logout {
    csrf: String,
}
async fn logout(
    State(app): State<App>,
    headers: HeaderMap,
    Form(form): Form<Logout>,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let bearer = token(&headers)?;
    let scope = {
        let client = app.db.get().await.map_err(Error::from)?;
        app.user.sessions.check(&**client, &bearer).await?
    };
    if scope.csrf != form.csrf {
        return Err(Error::Forbidden.into());
    }
    let destination = app
        .user
        .external_logout_destination(&scope, &bearer)
        .await
        .unwrap_or(None);
    app.user.logout(&bearer, &form.csrf).await?;
    let mut response = if let Some(destination) = destination {
        auth::provider_signed_out(&app, &destination).await?
    } else {
        Redirect::to("/").into_response()
    };
    response.headers_mut().insert(
        header::SET_COOKIE,
        HeaderValue::from_static("bilibi_session=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0"),
    );
    Ok(response)
}

async fn upgrade(
    State(app): State<App>,
    headers: HeaderMap,
    Query(options): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> std::result::Result<Response, HttpError> {
    origin(&app, &headers)?;
    let token = token(&headers)?;
    // Upgrade proves the session only. Each discovered page/panel has its own
    // gate. The socket keeps the identity it was opened under and acts for
    // nobody else (an impersonation switch moves the session underneath it).
    // The company is the one the page was rendered under (`company`, the
    // shell's `data-company-id`, empty for none), not the session's company
    // now: a page whose session switched company while its socket was down
    // then reloads at its first event instead of re-rendering under a top bar
    // that names the old company. A socket opened without the parameter binds
    // the session's company, as before.
    let scope = app.security.guard(&token).await?;
    let policy = live::Policy::read(&app).await?;
    let Some(slot) = app.live.admit(&token, &scope, &policy) else {
        return Ok((StatusCode::TOO_MANY_REQUESTS, live::TOO_MANY_SOCKETS).into_response());
    };
    let identity = match options.get("company") {
        Some(company) => {
            live::Identity::rendered(&scope, company.parse::<i64>().ok().filter(|id| *id > 0))
        }
        None => live::Identity::of(&scope),
    };
    Ok(ws
        // Bound allocations above the accepted event budget so an oversized
        // form can receive a nonfatal error (see live::run).
        .max_message_size(live::FRAME_LIMIT)
        .max_frame_size(live::FRAME_LIMIT)
        .on_upgrade(move |socket| {
            live::run::<live::Staff>(
                app,
                token,
                identity,
                socket,
                options.get("framed").is_some_and(|v| v == "true"),
                slot,
                policy,
            )
        }))
}

pub mod seed;
