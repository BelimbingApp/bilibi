//! Axum host: routes and downloads, cookie/Origin boundary, and one server-held
//! page per socket.
mod api;
mod auth;
mod composition;
mod create_owner;
mod environment;
mod errors;
mod impersonation;
mod landing;
mod live;
mod mcp;
mod media;
mod pdf;
mod perf;
mod routes;
mod shell;
mod transport;
mod webhooks;
use askama::Template;
use axum::{
    extract::{DefaultBodyLimit, Form, Path, Query, State, WebSocketUpgrade},
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
pub use environment::Environment;

#[derive(Clone)]
pub struct App {
    routes: Arc<Vec<routes::BoundRoute>>,
    downloads: Arc<Vec<routes::BoundDownload>>,
    moved: Arc<Vec<&'static bilibi_module_registry::contributions::Moved>>,
    pub db: Db,
    pub origin: Arc<str>,
    /// `BILIBI_ENV`: development turns Base Step-up enforcement off, nothing else.
    pub environment: Environment,
    settings: Arc<bilibi_base::settings::Settings>,
    pub step_up: Arc<bilibi_base::step_up::StepUp>,
    /// Text messages; the simulator is on outside production only.
    pub sms: Arc<bilibi_base::sms::Gateway>,
    /// The host the server listens on, for the development status bar only;
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
    api: Arc<api::Edge>,
    webhooks: Arc<webhooks::Boundary>,
    transport: Arc<transport::Boundary>,
}
impl App {
    /// Which instance this is, with the operator's label and colour
    /// (`bilibi_base::system::Instance::resolve`): what every page, consent
    /// page and agent answer names.
    pub async fn instance(&self) -> Result<bilibi_base::system::Instance> {
        let client = self.db.get().await?;
        bilibi_base::system::Instance::resolve(&self.settings, &**client).await
    }
    pub fn new(db: Db, origin: &str) -> Result<Self> {
        let environment = Environment::from_env()?;
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
        step_up.set_enforced(environment.requires_step_up());
        let sms: Arc<bilibi_base::sms::Gateway> = services.get(bilibi_base::sms::SERVICE)?;
        sms.enable_simulator(environment.sms_simulator());
        eprintln!("sms: {}", sms.describe());
        let settings = services.get("base/settings.store")?;
        let renderer = services.get("base/ui.renderer")?;
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
        let transport = Arc::new(transport::Boundary::bind(&services)?);
        Ok(Self {
            routes,
            downloads,
            moved,
            db,
            origin: origin.into(),
            environment,
            settings,
            step_up,
            sms,
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
            api,
            webhooks,
            transport,
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
    use bilibi_core::company::{primary, NewCompany, ProvisionOutcome};
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
    let mut pages = routes::install(Router::new(), &app.routes, &app.downloads);
    // The SMS simulator's inbox exists only where the simulator runs: in
    // production the route is not registered at all.
    if app.environment.sms_simulator() {
        pages = pages.route("/dev/sms-inbox", get(auth::sms_inbox));
    }
    routes::install_moved(pages, &app.moved)
        .merge(api::router(app.clone()))
        .merge(webhooks::router())
        .merge(transport::router())
        .route("/", get(auth::home).post(auth::login))
        .route("/login", get(auth::home).post(auth::login))
        .route("/login/verify", get(auth::verify_page).post(auth::verify))
        .route("/login/verify/resend", post(auth::resend))
        .route("/login/verify/cancel", get(auth::cancel_verify))
        .route(
            "/forgot-password",
            get(auth::forgot_page).post(auth::forgot),
        )
        .route(
            "/reset-password/{token}",
            get(auth::reset_page).post(auth::reset),
        )
        .route("/logout", post(logout))
        .route(impersonation::LEAVE, post(impersonation::leave))
        .route(impersonation::ENTER, post(impersonation::enter))
        .route("/preferences", post(shell::save))
        .route("/api/pins", get(shell::pins))
        .route("/api/pins/toggle", post(shell::toggle_pin))
        .route("/api/pins/reorder", post(shell::reorder_pins))
        .route("/live", get(upgrade))
        .route(media::STREAM, get(media::stream))
        .route(media::ATTACHMENT, get(media::attachment))
        .route(pdf::RENDER, get(pdf::render))
        .route("/fonts/{name}", get(font_asset))
        .route(
            "/assets/ui-specimen.html",
            get(|| async { asset_response("ui-specimen.html") }),
        )
        .route("/assets/ui-components.html", get(components_specimen))
        .route(
            "/assets/logo.svg",
            get(|| async { asset_response("logo.svg") }),
        )
        .route("/assets/ui-feedback.html", get(feedback_specimen))
        .route(
            "/assets/ui-feedback-driver.js",
            get(|| async { asset_response("ui-feedback-driver.js") }),
        )
        .route(
            "/assets/ui.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    bilibi_ui::UI_JS,
                )
            }),
        )
        .route(
            "/favicon.svg",
            get(|| async { asset_response("favicon.svg") }),
        )
        .route(
            "/favicon.ico",
            get(|| async { asset_response("favicon.ico") }),
        )
        .route(
            "/assets/app.css",
            get(|| async { ([(header::CONTENT_TYPE, "text/css")], bilibi_ui::CSS) }),
        )
        .route(
            "/assets/shell.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    bilibi_ui::SHELL_JS,
                )
            }),
        )
        .route(
            "/assets/hooks.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    bilibi_ui::HOOKS_JS,
                )
            }),
        )
        .route(
            "/assets/topbar.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript")],
                    bilibi_ui::TOPBAR_JS,
                )
            }),
        )
        .route(
            "/assets/live.js",
            get(|| async {
                let script = bilibi_ui::JS.replace(
                    "__BILIBI_DEV_RELOAD__",
                    if cfg!(debug_assertions) {
                        "true"
                    } else {
                        "false"
                    },
                );
                ([(header::CONTENT_TYPE, "text/javascript")], script)
            }),
        )
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), errors::pages))
        .layer(middleware::from_fn(response_headers))
        .layer(middleware::from_fn_with_state(perf, perf::requests))
        .with_state(app)
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

fn asset_response(name: &str) -> Response {
    match bilibi_ui::static_asset(name) {
        Some(asset) => ([(header::CONTENT_TYPE, asset.content_type)], asset.bytes).into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

async fn font_asset(Path(name): Path<String>) -> Response {
    asset_response(&name)
}

/// Defaults only: a handler that sets its own policy, as signed media streams
/// do with a sandboxing CSP, keeps it.
/// A server-side failure is also logged here with its request line: the
/// page shows only the error's public text.
async fn response_headers(request: axum::extract::Request, next: Next) -> Response {
    let line = format!("{} {}", request.method(), request.uri().path());
    let mut response = next.run(request).await;
    if let Some(Failure(cause)) = response.extensions_mut().remove::<Failure>() {
        eprintln!("error: {line} failed: {cause}");
    }
    for (key,value) in [
        ("cache-control","no-store"),
        ("x-content-type-options","nosniff"),
        ("referrer-policy","same-origin"),
        ("content-security-policy","default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; connect-src 'self'; frame-ancestors 'self'; form-action 'self'; base-uri 'none'"),
    ] { response.headers_mut().entry(axum::http::HeaderName::from_static(key)).or_insert(HeaderValue::from_static(value)); }
    response
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
    app.user.logout(&token(&headers)?, &form.csrf).await?;
    let mut response = Redirect::to("/").into_response();
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
    let identity = live::Identity::of(&app.security.guard(&token).await?);
    Ok(ws
        // A JSON intent is small; the limit leaves room for one upload chunk
        // frame (bilibi_ui::upload::CHUNK_SIZE plus its eight-byte header).
        .max_message_size(live::FRAME_LIMIT)
        .max_frame_size(live::FRAME_LIMIT)
        .on_upgrade(move |socket| {
            live::run(
                app,
                token,
                identity,
                socket,
                options.get("framed").is_some_and(|v| v == "true"),
            )
        }))
}

pub mod seed;
