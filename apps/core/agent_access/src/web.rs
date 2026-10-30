//! The person-facing pages (E4): device consent at `/oauth/device`, client
//! consent at `/oauth/authorize` and AI connections at
//! `/settings/ai-connections`, ported from Bilimbi
//! `apps/core/agent_access/lib/agent_access/web/{device,authorize,connections}_live.ex`.
//! Opening a link never approves anything: only an explicit submit decides,
//! the server computes and displays the connection's end time, and a submitted
//! expiry is refused. The host proves the session before any adapter call and
//! `AgentAccess` keeps the self-service gate; this file owns only page state.
use crate::{
    setup_prompt, AgentAccess, AuthorizationRequest, AuthorizeParams, Decision, DeviceReview,
    OAuthError,
};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_settings::Settings;
use bilibi_system::Instance;
use bilibi_tenancy::{ClientIdentity, Scope, SelfReport};
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, ActionLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, CodeInput,
    EmptyState, FlashMap, FlexRow, FlexTable, Html, IconButton, ListColumns, StatusKind,
    TableEmpty, NO_FLASH,
};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::SystemTime,
};
use tokio_postgres::GenericClient;

/// `core/agent_access.pages`: one adapter for the three routes; the operation
/// names the page. Every field is shared by the mounted states.
#[derive(Clone)]
pub struct Pages {
    pub access: Arc<AgentAccess>,
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    /// Reads the instance label and colour that both consent pages open with.
    pub settings: Arc<Settings>,
    /// `APP_ORIGIN`, the OAuth issuer; the MCP resource is `{origin}/mcp`.
    /// Never the request's Host header.
    pub origin: String,
    /// The person's own agent requests, for the AI tag's activity and last use.
    pub integration: Arc<bilibi_integration::Integration>,
    /// Names a called operation in words ("Read a company") for that activity.
    pub registry: Arc<bilibi_agent_api::Registry>,
}
impl Pages {
    async fn display(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<DisplayContext> {
        let locale = self.locales.resolve(client, scope, None).await?.locale;
        self.display.display(client, scope, None, locale).await
    }
    fn resource(&self) -> String {
        format!("{}/mcp", self.origin)
    }
    /// The banner naming which installation the person connects the agent to.
    async fn instance_banner(
        &self,
        client: &(impl GenericClient + Sync),
        id: &str,
    ) -> Result<Html> {
        Instance::resolve(&self.settings, client)
            .await?
            .banner(id, "mb-4")
            .map_err(|_| Error::Internal)
    }
}
type State = Box<dyn LiveState<Scope, Error>>;
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "connections").then_some("My Settings")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = match request.operation {
                "device" => {
                    DevicePage::new(self.clone(), request.query)?
                        .view(scope)
                        .await?
                }
                "authorize" => {
                    AuthorizePage::new(self.clone(), request.query)
                        .await?
                        .view(scope)
                        .await?
                }
                "connections" => ConnectionsPage::new(self.clone())?.view(scope).await?,
                "ai_status" => {
                    crate::ai_status::AiStatus::new(self.clone())
                        .view(scope)
                        .await?
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: State = match request.operation {
                "device" => Box::new(DevicePage::new(self.clone(), request.query)?),
                "authorize" => Box::new(AuthorizePage::new(self.clone(), request.query).await?),
                "connections" => Box::new(ConnectionsPage::new(self.clone())?),
                "ai_status" => Box::new(crate::ai_status::AiStatus::new(self.clone())),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}

/// One rendered instant, the Bilimbi `<.datetime>` contract
/// (`templates/datetime.html`): both server-decidable texts travel with the
/// element so the shell's display-mode control can swap them later.
pub struct Instant {
    pub id: String,
    pub iso: String,
    pub text: String,
    pub company: String,
    pub utc: String,
    pub mode: &'static str,
}
fn instant(display: &DisplayContext, id: impl Into<String>, at: SystemTime) -> Result<Instant> {
    let timestamp = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    let rendered = display.render(timestamp, Format::DateTime, Precision::Minute);
    Ok(Instant {
        id: id.into(),
        iso: rendered.iso,
        text: rendered.text,
        company: rendered.company,
        utc: rendered.utc,
        mode: rendered.mode.as_str(),
    })
}

/// The page's one flash, shown until dismissed or the page is left.
#[derive(Clone, Copy)]
struct Flash {
    kind: StatusKind,
    message: &'static str,
}
fn flash_map(flash: Option<Flash>) -> FlashMap<'static> {
    let mut messages = NO_FLASH;
    if let Some(flash) = flash {
        match flash.kind {
            StatusKind::Success => messages.success = flash.message,
            StatusKind::Error => messages.error = flash.message,
            StatusKind::Warning => messages.warning = flash.message,
            StatusKind::Info => messages.info = flash.message,
        }
    }
    messages
}
fn clear_flash(flash: &mut Option<Flash>, key: &str) {
    if flash.is_some_and(|f| f.kind.name() == key) {
        *flash = None;
    }
}
/// A storage failure stays an error for the host; every OAuth refusal becomes
/// page copy.
pub(crate) fn refusal(error: OAuthError) -> Result<OAuthError> {
    match error {
        OAuthError::Other(error) => Err(error),
        other => Ok(other),
    }
}
/// Expiry belongs to server state. An approve carrying any field is an old or
/// forged date-bearing form (Bilimbi `params == %{}`).
fn submitted(fields: &BTreeMap<String, Value>) -> bool {
    fields.keys().any(|key| key != "type")
}
pub(crate) fn html<T: Template>(template: T) -> Result<String> {
    template.render().map_err(|_| Error::Internal)
}
fn only(query: &HashMap<String, String>, allowed: &[&str]) -> Result<()> {
    if query.keys().all(|key| allowed.contains(&key.as_str())) {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}
// ---- /oauth/device -----------------------------------------------------------

struct Completed {
    approved: bool,
    agent_id: String,
    expires_at: SystemTime,
}
struct DevicePage {
    pages: Pages,
    code: String,
    device: Option<DeviceReview>,
    expires_at: Option<SystemTime>,
    completed: Option<Completed>,
    error: Option<&'static str>,
    flash: Option<Flash>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum DeviceEvent {
    Lookup {
        #[serde(default)]
        user_code: String,
    },
    Approve {
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
    Deny,
    Another,
    Dismiss,
    /// The flash outlet's own dismissal (`data-event="clear-flash"`).
    #[serde(rename = "clear-flash")]
    ClearFlash {
        key: String,
    },
}
pub struct CompletedView<'a> {
    pub approved: bool,
    pub agent_id: &'a str,
    pub expiry: Option<Instant>,
}
impl CompletedView<'_> {
    pub fn title(&self) -> &'static str {
        if self.approved {
            "Agent connected"
        } else {
            "Connection refused"
        }
    }
}
#[derive(Template)]
#[template(path = "device.html")]
struct DeviceView<'a> {
    instance: Html,
    flash: FlashMap<'static>,
    error: Option<&'static str>,
    code: &'a str,
    code_input: CodeInput,
    device: Option<&'a DeviceReview>,
    expiry: Option<Instant>,
    completed: Option<CompletedView<'a>>,
}
fn device_message(error: &OAuthError) -> &'static str {
    match error {
        OAuthError::PersonRequired => {
            "Connect from your own account. You cannot approve connections while viewing as someone else."
        }
        OAuthError::InvalidExpiry => {
            "The connection end time cannot be changed here. Check the code again."
        }
        _ => "This code expired or has already been used. Request a new code from your agent.",
    }
}
impl DevicePage {
    /// `?user_code=` only fills the field (Bilimbi `handle_params`).
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        only(query, &["user_code"])?;
        let code = Self::code(query.get("user_code").map_or("", String::as_str))?;
        Ok(Self {
            pages,
            code,
            device: None,
            expires_at: None,
            completed: None,
            error: None,
            flash: None,
        })
    }
    fn code(raw: &str) -> Result<String> {
        if raw.len() > 64 {
            return Err(Error::Invalid);
        }
        Ok(crate::normalize_user_code(raw))
    }
    async fn view(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let display = self.pages.display(&**client, scope).await?;
        let expiry = self
            .expires_at
            .map(|at| instant(&display, "device-expiry", at))
            .transpose()?;
        let completed = match &self.completed {
            Some(done) => Some(CompletedView {
                approved: done.approved,
                agent_id: &done.agent_id,
                expiry: done
                    .approved
                    .then(|| instant(&display, "device-completed-expiry", done.expires_at))
                    .transpose()?,
            }),
            None => None,
        };
        html(DeviceView {
            instance: self
                .pages
                .instance_banner(&**client, "device-instance")
                .await?,
            flash: flash_map(self.flash),
            error: self.error,
            code: &self.code,
            code_input: CodeInput::new(
                "device_user_code",
                "user_code",
                "Code shown by your agent",
                8,
            )
            .value(&self.code)
            .group_size(4),
            device: self.device.as_ref(),
            expiry,
            completed,
        })
    }
    async fn lookup(&mut self, scope: &Scope, raw: &str) -> Result<()> {
        self.code = Self::code(raw)?;
        match self.pages.access.device(scope, &self.code).await {
            Ok(device) => {
                self.device = Some(device);
                self.error = None;
                self.expires_at = Some(self.pages.access.connection_expiry().await?);
            }
            Err(error) => {
                self.device = None;
                self.error = Some(device_message(&refusal(error)?));
            }
        }
        Ok(())
    }
    async fn decide(&mut self, scope: &Scope, decision: Decision) -> Result<()> {
        let (Some(device), Some(expires_at)) = (&self.device, self.expires_at) else {
            return Ok(());
        };
        match self
            .pages
            .access
            .decide(scope, &self.code, decision, expires_at)
            .await
        {
            Ok(()) => {
                let approved = decision == Decision::Approve;
                self.completed = Some(Completed {
                    approved,
                    agent_id: device.agent_id.clone(),
                    expires_at,
                });
                self.device = None;
                self.code = String::new();
                self.error = None;
                self.flash = Some(Flash {
                    kind: StatusKind::Success,
                    message: if approved {
                        "Agent connected."
                    } else {
                        "Connection refused."
                    },
                });
            }
            Err(error) => self.error = Some(device_message(&refusal(error)?)),
        }
        Ok(())
    }
    fn reset(&mut self) {
        self.device = None;
        self.expires_at = None;
        self.completed = None;
        self.code = String::new();
        self.error = None;
    }
}
impl LiveState<Scope, Error> for DevicePage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    only(values, &["user_code"])?;
                    self.code = Self::code(values.get("user_code").map_or("", String::as_str))?;
                }
                LiveInput::Event(value) => {
                    match serde_json::from_value::<DeviceEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?
                    {
                        DeviceEvent::Lookup { user_code } => {
                            self.lookup(request.scope, &user_code).await?
                        }
                        DeviceEvent::Approve { fields } if submitted(&fields) => {
                            self.error = Some(device_message(&OAuthError::InvalidExpiry));
                        }
                        DeviceEvent::Approve { .. } => {
                            self.decide(request.scope, Decision::Approve).await?
                        }
                        DeviceEvent::Deny => self.decide(request.scope, Decision::Deny).await?,
                        DeviceEvent::Another => self.reset(),
                        DeviceEvent::Dismiss => self.error = None,
                        DeviceEvent::ClearFlash { key } => clear_flash(&mut self.flash, &key),
                    }
                }
            }
            let mut query = HashMap::new();
            if !self.code.is_empty() {
                query.insert("user_code".into(), self.code.clone());
            }
            Ok(LiveBody {
                html: self.view(request.scope).await?,
                query,
                ..LiveBody::default()
            })
        })
    }
}

// ---- /oauth/authorize --------------------------------------------------------

struct AuthorizePage {
    pages: Pages,
    query: HashMap<String, String>,
    params: AuthorizeParams,
    request: Option<AuthorizationRequest>,
    expires_at: Option<SystemTime>,
    error: Option<&'static str>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AuthorizeEvent {
    Approve {
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
    Deny,
    Dismiss,
}
#[derive(Template)]
#[template(path = "authorize.html")]
struct AuthorizeView<'a> {
    instance: Html,
    origin: &'a str,
    connections: Html,
    actions: Html,
    error: Option<&'static str>,
    request: Option<&'a AuthorizationRequest>,
    expiry: Option<Instant>,
}
fn authorize_message(error: &OAuthError) -> &'static str {
    match error {
        OAuthError::PersonRequired => {
            "Connect from your own account. Agents and people viewing as someone else cannot approve connections."
        }
        OAuthError::InvalidExpiry => {
            "The connection end time cannot be changed here. Start a new connection."
        }
        OAuthError::InvalidClient => {
            "This app has not registered a connection. Return to the app and start sign-in again."
        }
        _ => {
            "This connection request is invalid. Return to your client and start a new connection."
        }
    }
}
/// RFC 3986 unreserved characters pass; everything else is percent-encoded.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|byte| {
            if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
                (byte as char).to_string()
            } else {
                format!("%{byte:02X}")
            }
        })
        .collect()
}
impl AuthorizePage {
    /// The request is validated against its client registration before
    /// consent renders; an invalid one shows an error and never redirects.
    async fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        if query.values().any(|value| value.len() > 2048) {
            return Err(Error::Invalid);
        }
        let params: AuthorizeParams =
            serde_json::from_value(serde_json::to_value(query).map_err(|_| Error::Invalid)?)
                .map_err(|_| Error::Invalid)?;
        let mut page = Self {
            query: query.clone(),
            params,
            request: None,
            expires_at: None,
            error: None,
            pages,
        };
        page.validate().await?;
        Ok(page)
    }
    async fn validate(&mut self) -> Result<()> {
        match self
            .pages
            .access
            .authorization_request(&self.params, &self.pages.resource())
            .await
        {
            Ok(request) => {
                self.request = Some(request);
                self.expires_at = Some(self.pages.access.connection_expiry().await?);
                self.error = None;
            }
            Err(error) => {
                self.request = None;
                self.error = Some(authorize_message(&refusal(error)?));
            }
        }
        Ok(())
    }
    async fn view(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let display = self.pages.display(&**client, scope).await?;
        let expiry = self
            .expires_at
            .map(|at| instant(&display, "oauth-expiry", at))
            .transpose()?;
        html(AuthorizeView {
            instance: self
                .pages
                .instance_banner(&**client, "oauth-instance")
                .await?,
            origin: &self.pages.origin,
            connections: Html::render(&ActionLink {
                id: "oauth-connections",
                title: "Your connections",
                href: "/settings/ai-connections",
                body: Html::text("Your connections"),
                ..ActionLink::default()
            })
            .map_err(|_| Error::Internal)?,
            actions: {
                let mut actions = Html::render(&Button {
                    variant: ButtonVariant::Primary,
                    id: "oauth-consent-approve",
                    button_type: "submit",
                    working: "Connecting…",
                    body: Html::text("Approve connection"),
                    ..Button::default()
                })
                .map_err(|_| Error::Internal)?;
                actions.push(
                    &Html::render(&Button {
                        id: "oauth-consent-deny",
                        button_type: "button",
                        event: "deny",
                        body: Html::text("Refuse"),
                        ..Button::default()
                    })
                    .map_err(|_| Error::Internal)?,
                );
                actions
            },
            error: self.error,
            request: self.request.as_ref(),
            expiry,
        })
    }
    /// The registered callback with the result, the client's `state` and the
    /// issuer (RFC 9207); any earlier value of those keys is replaced.
    fn return_to_client(&self, request: &AuthorizationRequest, result: (&str, &str)) -> String {
        let (base, existing) = request
            .redirect_uri
            .split_once('?')
            .map_or((request.redirect_uri.as_str(), ""), |(base, query)| {
                (base, query)
            });
        let mut pairs: Vec<String> = existing
            .split('&')
            .filter(|pair| !pair.is_empty())
            .filter(|pair| {
                let key = pair.split('=').next().unwrap_or("");
                !["code", "error", "state", "iss"].contains(&key)
            })
            .map(str::to_owned)
            .collect();
        for (key, value) in [
            result,
            ("state", request.state.as_str()),
            ("iss", self.pages.origin.as_str()),
        ] {
            pairs.push(format!("{}={}", encode(key), encode(value)));
        }
        format!("{base}?{}", pairs.join("&"))
    }
    async fn approve(&mut self, scope: &Scope) -> Result<Option<String>> {
        let Some(request) = self.request.clone() else {
            return Ok(None);
        };
        let expires_at = self.expires_at.ok_or(Error::Internal)?;
        match self
            .pages
            .access
            .authorize_reviewed(
                scope,
                &self.params,
                &self.pages.resource(),
                expires_at,
                Some(&request),
            )
            .await
        {
            Ok(code) => Ok(Some(self.return_to_client(&request, ("code", &code)))),
            Err(error) => {
                self.error = Some(authorize_message(&refusal(error)?));
                Ok(None)
            }
        }
    }
    /// Refusal revalidates the registered callback too.
    async fn deny(&mut self) -> Result<Option<String>> {
        if self.request.is_none() {
            return Ok(None);
        }
        self.validate().await?;
        Ok(self
            .request
            .as_ref()
            .map(|request| self.return_to_client(request, ("error", "access_denied"))))
    }
}
impl LiveState<Scope, Error> for AuthorizePage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut redirect = None;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    match serde_json::from_value::<AuthorizeEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?
                    {
                        AuthorizeEvent::Approve { fields } if submitted(&fields) => {
                            self.error = Some(authorize_message(&OAuthError::InvalidExpiry));
                        }
                        AuthorizeEvent::Approve { .. } => {
                            redirect = self.approve(request.scope).await?
                        }
                        AuthorizeEvent::Deny => redirect = self.deny().await?,
                        AuthorizeEvent::Dismiss => self.error = None,
                    }
                }
            }
            Ok(LiveBody {
                html: self.view(request.scope).await?,
                query: self.query.clone(),
                redirect,
                ..LiveBody::default()
            })
        })
    }
}

// ---- /settings/ai-connections -------------------------------------------------

struct ConnectionsPage {
    pages: Pages,
    columns: ListColumns,
    pending: Option<i64>,
    flash: Option<Flash>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ConnectionsEvent {
    Grid {
        #[serde(flatten)]
        fields: BTreeMap<String, Value>,
    },
    Revoke {
        id: String,
    },
    CancelRevoke,
    ConfirmRevoke,
    #[serde(rename = "clear-flash")]
    ClearFlash {
        key: String,
    },
}
pub struct ConnectionRow {
    pub id: String,
    pub dom_id: String,
    pub revoke_id: String,
    pub agent_id: String,
    pub expiry: Instant,
    pub status: &'static str,
    /// Other stated IDs, sorted, comma-separated; empty when none.
    pub seen: String,
    /// Self-reported `harness/model-version-effort` values, newest first,
    /// comma-separated; empty when none.
    pub self_report: String,
    pub client_identity: String,
    pub revoked: bool,
}
#[derive(Template)]
#[template(
    source = r#"<p>MCP lets an AI agent act on your behalf in Bilibi, with your permissions and nothing more. Hand it routine tasks, and it shows you each change to check before it is saved. Risky changes also need your 2FA code.</p><ul class="mt-1 list-disc space-y-0.5 pl-4"><li>To connect, paste the prompt below into your AI app's chat. Then sign in to Bilibi in your browser and approve the connection.</li><li>To disconnect an AI app, choose Revoke connection in the table above. Access ends at once.</li></ul>"#,
    ext = "html"
)]
struct Intro;

#[derive(Template)]
#[template(path = "connections.html")]
struct ConnectionsView {
    flash: FlashMap<'static>,
    header: Html,
    intro: Html,
    table: Html,
    pending: Option<i64>,
    /// Impersonation and an agent facet see the table's refusal, not the prompt.
    show_setup: bool,
    /// The paste-ready text, shown whole so the person sees what the agent is told.
    prompt_text: String,
    /// A copy reachable only from this computer or network gets the extra
    /// help; the same test as the AI status panel's `local_only`.
    local_help: bool,
    /// Offer the ChatGPT plugin ZIP; see `offers_plugin`.
    plugin: bool,
    plugin_href: &'static str,
    /// The MCP address and the name a tool saves it under, for the manual steps.
    address: String,
    server_name: String,
}
/// What a person on their own account sees below the table.
struct Setup {
    prompt: String,
    address: String,
    server_name: String,
    local_only: bool,
    plugin: bool,
}
impl ConnectionsPage {
    /// The address takes no query. Older links carried `?client_id=` and still open the tab.
    fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            columns: ListColumns::mount(
                "agent-connections",
                vec![
                    BuiltinColumn::new("agent", "Harness"),
                    BuiltinColumn::new("expiry", "Expires"),
                    BuiltinColumn::new("status", "Status"),
                    BuiltinColumn::new("seen", "Also known as"),
                    BuiltinColumn::new("self_report", SelfReport::LABEL),
                    BuiltinColumn::new("client_identity", ClientIdentity::LABEL),
                ],
            )
            .map_err(|_| Error::Internal)?,
            pending: None,
            flash: None,
        })
    }
    /// Only the person may set up their own connection.
    async fn setup(&self, scope: &Scope) -> Result<Option<Setup>> {
        if super::self_service(scope).is_err() {
            return Ok(None);
        }
        let client = self.pages.db.get().await?;
        let instance = Instance::resolve(&self.pages.settings, &**client).await?;
        Ok(Some(Setup {
            prompt: setup_prompt::prompt(&instance),
            address: setup_prompt::address(&instance),
            server_name: setup_prompt::server_name(&instance),
            local_only: !instance.public_address(),
            plugin: super::offers_plugin(scope, &instance.origin),
        }))
    }
    async fn view(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let display = self.pages.display(&**client, scope).await?;
        let (rows, error) = match self.pages.access.list_connections(scope).await {
            Ok(rows) => (rows, None),
            Err(error) => {
                refusal(error)?;
                (
                    Vec::new(),
                    Some("Use your own account to manage connections."),
                )
            }
        };
        let now = SystemTime::now();
        let rows = rows
            .into_iter()
            .map(|row| {
                Ok(ConnectionRow {
                    id: row.id.to_string(),
                    dom_id: format!("connections-{}", row.id),
                    revoke_id: format!("revoke-{}", row.id),
                    expiry: instant(
                        &display,
                        format!("connection-expiry-{}", row.id),
                        row.expires_at,
                    )?,
                    status: if row.revoked_at.is_some() {
                        "Revoked"
                    } else if row.expires_at > now {
                        "Connected"
                    } else {
                        "Expired"
                    },
                    seen: row
                        .seen_agent_ids
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    client_identity: row
                        .observed_clients
                        .keys()
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(", "),
                    self_report: {
                        let mut reports: Vec<_> = row.self_reported_agents.into_iter().collect();
                        reports.sort_by(|a, b| (&b.1, &b.0).cmp(&(&a.1, &a.0)));
                        reports
                            .into_iter()
                            .map(|(report, _)| report)
                            .collect::<Vec<_>>()
                            .join(", ")
                    },
                    revoked: row.revoked_at.is_some(),
                    agent_id: row.agent_id,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let rows = rows
            .into_iter()
            .map(|row| {
                let mut rendered = FlexRow::new(&row.id)
                    .slot("agent", Html::text(&row.agent_id))
                    .cell(
                        "expiry",
                        bilibi_ui::flex_table::Cell::instant(bilibi_ui::flex_table::Instant::new(
                            &row.expiry.iso,
                            &row.expiry.text,
                            &row.expiry.company,
                            &row.expiry.utc,
                            row.expiry.mode,
                        )),
                    )
                    .slot("status", Html::text(row.status))
                    .slot(
                        "seen",
                        if row.seen.is_empty() {
                            Html::EMPTY
                        } else {
                            Html::render(&Badge::status(BadgeKind::Warning, &row.seen))
                                .map_err(|_| Error::Internal)?
                        },
                    )
                    .slot("self_report", Html::text(&row.self_report))
                    .slot("client_identity", Html::text(&row.client_identity));
                rendered.dom_id = Some(row.dom_id);
                if !row.revoked {
                    rendered.actions = Html::render(&IconButton {
                        id: &row.revoke_id,
                        event: "revoke",
                        name: "id",
                        value: &row.id,
                        ..IconButton::new("unlink", "Revoke connection")
                    })
                    .map_err(|_| Error::Internal)?;
                }
                Ok(rendered)
            })
            .collect::<Result<Vec<_>>>()?;
        let mut columns = self.columns.flex_columns();
        if let Some(expiry) = columns.iter_mut().find(|column| column.id == "expiry") {
            expiry.kind = bilibi_ui::flex_table::ColumnKind::Field;
        }
        let mut table = FlexTable::new("agent-connections", columns, rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.actions = true;
        table.empty = Some(TableEmpty::State(if let Some(error) = error {
            EmptyState::missing("Connections unavailable", error).id("connections-error")
        } else {
            EmptyState::missing(
                "No AI apps connected yet",
                "Paste the prompt below into your AI app.",
            )
            .id("connections-empty")
        }));
        let render = |rendered: askama::Result<Html>| rendered.map_err(|_| Error::Internal);
        let setup = self.setup(scope).await?;
        let prompt = setup.as_ref().map(|setup| setup.prompt.clone());
        let show_setup = prompt.is_some();
        html(ConnectionsView {
            flash: flash_map(self.flash),
            header: render(Html::render(&bilibi_ui::Header {
                title: Html::text("My Settings"),
                ..bilibi_ui::Header::default()
            }))?,
            intro: render(Html::render(&Intro))?,
            table: render(Html::render(&table))?,
            pending: self.pending,
            show_setup,
            prompt_text: prompt.unwrap_or_default(),
            local_help: setup.as_ref().is_some_and(|setup| setup.local_only),
            plugin: setup.as_ref().is_some_and(|setup| setup.plugin),
            plugin_href: super::PLUGIN_DOWNLOAD,
            address: setup
                .as_ref()
                .map(|setup| setup.address.clone())
                .unwrap_or_default(),
            server_name: setup.map(|setup| setup.server_name).unwrap_or_default(),
        })
    }
    async fn confirm(&mut self, scope: &Scope) -> Result<()> {
        let Some(id) = self.pending.take() else {
            return Ok(());
        };
        self.flash = Some(match self.pages.access.revoke(scope, id).await {
            Ok(()) => Flash {
                kind: StatusKind::Success,
                message: "Connection revoked.",
            },
            Err(error) => {
                refusal(error)?;
                Flash {
                    kind: StatusKind::Error,
                    message: "Connection was not revoked. It may already be revoked.",
                }
            }
        });
        Ok(())
    }
}
impl LiveState<Scope, Error> for ConnectionsPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => {}
                LiveInput::Event(value) => {
                    match serde_json::from_value::<ConnectionsEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?
                    {
                        ConnectionsEvent::Grid { fields } => {
                            let value = serde_json::to_value(fields).map_err(|_| Error::Invalid)?;
                            self.columns.handle(
                                &params::string_values_checked(&value, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            );
                        }
                        ConnectionsEvent::Revoke { id } => {
                            self.pending = Some(
                                id.parse::<i64>()
                                    .ok()
                                    .filter(|id| *id > 0)
                                    .ok_or(Error::Invalid)?,
                            );
                        }
                        ConnectionsEvent::CancelRevoke => self.pending = None,
                        ConnectionsEvent::ConfirmRevoke => self.confirm(request.scope).await?,
                        ConnectionsEvent::ClearFlash { key } => clear_flash(&mut self.flash, &key),
                    }
                }
            }
            Ok(LiveBody {
                html: self.view(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}
