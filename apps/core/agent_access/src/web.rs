//! The person-facing pages (E4): device consent at `/oauth/device`, client
//! consent at `/oauth/authorize` and AI connections at
//! `/settings/ai-connections`.
//! Opening a link never approves anything: only an explicit submit decides,
//! approval records a non-expiring connection. The host proves the session before any adapter call and
//! `AgentAccess` keeps the self-service gate; this file owns only page state.
use crate::{
    setup_prompt, AgentAccess, AuthorizationRequest, AuthorizeParams, ConnectionSummary, Decision,
    DeviceReview, OAuthError,
};
use askama::Template;
use bilibi_agent_api::{
    mcp,
    plans::{PlanAuthority, PlanIdentity, Plans},
    Reviews,
};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_settings::Settings;
use bilibi_system::Instance;
use bilibi_tenancy::{ClientIdentity, ProofFuture, Scope, SelfReport};
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, ActionLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Choice, CodeInput,
    EmptyState, FlashMap, FlexRow, FlexTable, Html, IconButton, ListColumns, RadioGroup,
    StatusKind, TableEmpty, Toggle, NO_FLASH,
};
use serde::Deserialize;
use serde_json::{json, Value};
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
    pub plans: Arc<Plans>,
    pub reviews: Arc<Reviews>,
    pub step_up: Arc<bilibi_step_up::StepUp>,
    /// Names the records an agent request operated on, with the reader's own
    /// access, for that activity.
    pub records: Arc<bilibi_records::Records>,
}
impl Pages {
    pub(crate) async fn display(
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
        match operation {
            "connections" => Some("My Settings"),
            "review" => Some("Review entries"),
            _ => None,
        }
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
                    AuthorizePage::new(self.clone(), request.query, scope)
                        .await?
                        .view(scope)
                        .await?
                }
                "connections" => ConnectionsPage::new(self.clone())?.view(scope).await?,
                "review" => {
                    BrowserReviewPage::new(self.clone(), request.path)?
                        .view(scope)
                        .await?
                }
                "ai_status" => {
                    crate::ai_status::AiStatus::new(self.clone(), None)
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
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let state: State = match request.operation {
                "device" => Box::new(DevicePage::new(self.clone(), request.query)?),
                "authorize" => {
                    Box::new(AuthorizePage::new(self.clone(), request.query, scope).await?)
                }
                "connections" => Box::new(ConnectionsPage::new(self.clone())?),
                "review" => Box::new(BrowserReviewPage::new(self.clone(), request.path)?),
                "ai_status" => Box::new(crate::ai_status::AiStatus::new(
                    self.clone(),
                    Some(scope.clone()),
                )),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}

struct BrowserReviewPage {
    pages: Pages,
    review: String,
}

impl BrowserReviewPage {
    fn new(pages: Pages, path: &HashMap<String, String>) -> Result<Self> {
        let review = path
            .get("review")
            .filter(|id| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            })
            .cloned()
            .ok_or(Error::NotFound)?;
        Ok(Self { pages, review })
    }

    fn html(&self) -> Result<String> {
        let src = bilibi_ui::Assets::url("/assets/review-entries-v1.html");
        let frame = Html::render(&bilibi_ui::AppFrame {
            id: "review-card",
            src,
            title: "Review entries",
        })
        .map_err(|_| Error::Internal)?;
        let header = Html::render(&bilibi_ui::Header {
            title: Html::text("Review entries"),
            subtitle:
                "Edit the proposed values, check the complete review, then approve or cancel.",
            ..Default::default()
        })
        .map_err(|_| Error::Internal)?;
        let mut body = header;
        body.push(&frame);
        Html::render(&bilibi_ui::Page {
            id: "agent-review-page",
            variant: bilibi_ui::PageVariant::Detail,
            class: "gap-3",
            body,
        })
        .map(|html| html.to_string())
        .map_err(|_| Error::Internal)
    }

    async fn view(&self, scope: &Scope) -> Result<String> {
        let review = self
            .pages
            .reviews
            .browser(scope, &self.review)
            .await
            .map_err(review_error)?;
        self.pages
            .access
            .review_claims(scope, review.connection)
            .await
            .map_err(oauth_error)?;
        self.html()
    }

    fn event(&self, message: Value) -> Result<LiveBody> {
        Ok(LiveBody {
            html: self.html()?,
            events: vec![(
                "app-frame",
                json!({"frame":"review-card","message":message}),
            )],
            ..LiveBody::default()
        })
    }
}

fn oauth_error(error: OAuthError) -> Error {
    match error {
        OAuthError::Other(error) => error,
        _ => Error::Unauthorized,
    }
}

fn review_error(error: bilibi_agent_api::Error) -> Error {
    match error {
        bilibi_agent_api::Error::Backend(error) => error,
        bilibi_agent_api::Error::Forbidden => Error::Forbidden,
        bilibi_agent_api::Error::NotFound => Error::NotFound,
        bilibi_agent_api::Error::InvalidInput(_) => Error::Invalid,
        _ => Error::Internal,
    }
}

struct BrowserReviewAuthority {
    access: Arc<AgentAccess>,
    claims: crate::AgentClaims,
    trace: String,
    report: Option<SelfReport>,
    client: Option<ClientIdentity>,
}
impl PlanAuthority for BrowserReviewAuthority {
    fn verify<'a>(
        &'a self,
        tx: &'a tokio_postgres::Transaction<'_>,
    ) -> ProofFuture<'a, PlanIdentity> {
        Box::pin(async move {
            let scope = self
                .access
                .prove(tx, &self.claims, true, &self.trace, self.report.as_ref())
                .await
                .map_err(oauth_error)?
                .with_client_identity(self.client.clone());
            Ok(PlanIdentity {
                credential_binding: format!("agent-connection:{}", self.claims.connection),
                scope,
            })
        })
    }
}

impl LiveState<Scope, Error> for BrowserReviewPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let LiveInput::Event(event) = request.input else {
                self.pages
                    .reviews
                    .browser(request.scope, &self.review)
                    .await
                    .map_err(review_error)?;
                return Ok(LiveBody {
                    html: self.html()?,
                    ..LiveBody::default()
                });
            };
            if event.get("type").and_then(Value::as_str) != Some("app-frame") {
                return Err(Error::Invalid);
            }
            let wire = event
                .get("bridge")
                .and_then(Value::as_str)
                .ok_or(Error::Invalid)?;
            let message: Value = serde_json::from_str(wire).map_err(|_| Error::Invalid)?;
            let review = self
                .pages
                .reviews
                .browser(request.scope, &self.review)
                .await
                .map_err(review_error)?;
            if message.get("method").and_then(Value::as_str) == Some("open") {
                return self.event(json!({
                    "jsonrpc":"2.0",
                    "method":"ui/notifications/tool-result",
                    "params":mcp::tool_result(review.card, false)
                }));
            }
            let id = message.get("id").cloned().ok_or(Error::Invalid)?;
            if message.get("method").and_then(Value::as_str) != Some("tools/call") {
                return self.event(
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":"Method not found"}}),
                );
            }
            let name = message
                .pointer("/params/name")
                .and_then(Value::as_str)
                .ok_or(Error::Invalid)?;
            let arguments = message
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let claims = self
                .pages
                .access
                .review_claims(request.scope, review.connection)
                .await
                .map_err(oauth_error)?;
            let report = review
                .agent_self_report
                .as_deref()
                .and_then(SelfReport::parse);
            let client = review
                .client_identity
                .as_deref()
                .and_then(ClientIdentity::parse);
            let trace = format!("review-{}", self.review);
            let authority = BrowserReviewAuthority {
                access: self.pages.access.clone(),
                claims: claims.clone(),
                trace: trace.clone(),
                report: report.clone(),
                client: client.clone(),
            };
            let agent_scope = self
                .pages
                .access
                .seal(&claims, &trace, report.as_ref())
                .await
                .map_err(oauth_error)?
                .with_client_identity(client);
            let session = mcp::Session {
                db: &self.pages.db,
                scope: &agent_scope,
                authority: &authority,
                step_up: &self.pages.step_up,
                reviews: &self.pages.reviews,
            };
            let result = mcp::card_call(
                &self.pages.registry,
                &self.pages.plans,
                session,
                name,
                arguments,
            )
            .await?;
            self.event(json!({"jsonrpc":"2.0","id":id,"result":result}))
        })
    }
}

/// One rendered instant, the `<.datetime>` contract
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
pub(crate) fn instant(
    display: &DisplayContext,
    id: impl Into<String>,
    at: SystemTime,
) -> Result<Instant> {
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
// Only the choice is accepted; the reviewed connection IDs stay on the server.
fn replacement(fields: &BTreeMap<String, Value>, has_matches: bool) -> Option<bool> {
    if fields
        .keys()
        .any(|key| key != "type" && key != "replacement")
    {
        return None;
    }
    match fields.get("replacement").and_then(Value::as_str) {
        Some("replace") if has_matches => Some(true),
        Some("keep") if has_matches => Some(false),
        None if !has_matches => Some(false),
        _ => None,
    }
}
struct DuplicateView {
    name: String,
    approved: Instant,
    last_used: String,
    never_used: bool,
}
async fn duplicates(pages: &Pages, scope: &Scope, name: &str) -> Result<Vec<ConnectionSummary>> {
    match pages.access.list_connections(scope).await {
        Ok(rows) => Ok(rows
            .into_iter()
            .filter(|r| r.agent_id == name && r.revoked_at.is_none())
            .collect()),
        Err(error) => {
            refusal(error)?;
            Ok(Vec::new())
        }
    }
}
async fn duplicate_views(
    pages: &Pages,
    scope: &Scope,
    rows: &[ConnectionSummary],
    display: &DisplayContext,
) -> Result<Vec<DuplicateView>> {
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let client = pages.db.get().await?;
    let uses = pages
        .integration
        .own_agent_last_use(&**client, scope)
        .await?;
    let now = SystemTime::now();
    rows.iter()
        .map(|row| {
            Ok(DuplicateView {
                name: row.agent_id.clone(),
                approved: instant(
                    display,
                    format!("duplicate-approved-{}", row.id),
                    row.created_at,
                )?,
                last_used: uses
                    .get(&row.id.to_string())
                    .map(|at| format!("Last used {}", crate::ai_status::ago(*at, now)))
                    .unwrap_or_else(|| "Never used".to_owned()),
                never_used: !uses.contains_key(&row.id.to_string()),
            })
        })
        .collect()
}
fn replacement_choice(duplicates: &[DuplicateView]) -> RadioGroup {
    let unused = duplicates.iter().all(|d| d.never_used);
    RadioGroup::new(
        "replacement",
        "replacement",
        vec![
            Choice::new("Replace existing connections", "replace"),
            Choice::new("Keep both", "keep"),
        ],
    )
    .label("Existing connections")
    .value(if unused { "replace" } else { "keep" })
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
}
struct DevicePage {
    pages: Pages,
    code: String,
    device: Option<DeviceReview>,
    duplicates: Vec<ConnectionSummary>,
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
    duplicates: Vec<DuplicateView>,
    replacement: RadioGroup,
    completed: Option<CompletedView<'a>>,
}
fn device_message(error: &OAuthError) -> &'static str {
    match error {
        OAuthError::PersonRequired => {
            "Connect from your own account. You cannot approve connections while viewing as someone else."
        }
        _ => "This code expired or has already been used. Request a new code from your agent.",
    }
}
impl DevicePage {
    /// `?user_code=` only fills the field.
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        only(query, &["user_code"])?;
        let code = Self::code(query.get("user_code").map_or("", String::as_str))?;
        Ok(Self {
            pages,
            code,
            device: None,
            duplicates: Vec::new(),
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
        let completed = self.completed.as_ref().map(|done| CompletedView {
            approved: done.approved,
            agent_id: &done.agent_id,
        });
        let duplicates = duplicate_views(&self.pages, scope, &self.duplicates, &display).await?;
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
            replacement: replacement_choice(&duplicates),
            duplicates,
            completed,
        })
    }
    async fn lookup(&mut self, scope: &Scope, raw: &str) -> Result<()> {
        self.code = Self::code(raw)?;
        match self.pages.access.device(scope, &self.code).await {
            Ok(device) => {
                self.duplicates = duplicates(&self.pages, scope, &device.agent_id).await?;
                self.device = Some(device);
                self.error = None;
            }
            Err(error) => {
                self.device = None;
                self.error = Some(device_message(&refusal(error)?));
            }
        }
        Ok(())
    }
    async fn decide(&mut self, scope: &Scope, decision: Decision, replace: bool) -> Result<()> {
        let Some(device) = &self.device else {
            return Ok(());
        };
        match self
            .pages
            .access
            .decide_replacing(
                scope,
                &self.code,
                decision,
                &if replace {
                    self.duplicates.iter().map(|r| r.id).collect::<Vec<_>>()
                } else {
                    Vec::new()
                },
            )
            .await
        {
            Ok(()) => {
                let approved = decision == Decision::Approve;
                self.completed = Some(Completed {
                    approved,
                    agent_id: device.agent_id.clone(),
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
                        DeviceEvent::Approve { fields } => {
                            if let Some(replace) = replacement(&fields, !self.duplicates.is_empty())
                            {
                                self.decide(request.scope, Decision::Approve, replace)
                                    .await?
                            } else {
                                self.error = Some(device_message(&OAuthError::InvalidRequest));
                            }
                        }
                        DeviceEvent::Deny => {
                            self.decide(request.scope, Decision::Deny, false).await?
                        }
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
    duplicates: Vec<ConnectionSummary>,
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
    duplicates: Vec<DuplicateView>,
    replacement: RadioGroup,
}
fn authorize_message(error: &OAuthError) -> &'static str {
    match error {
        OAuthError::PersonRequired => {
            "Connect from your own account. Agents and people viewing as someone else cannot approve connections."
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
    async fn new(pages: Pages, query: &HashMap<String, String>, scope: &Scope) -> Result<Self> {
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
            duplicates: Vec::new(),
            error: None,
            pages,
        };
        page.validate().await?;
        if let Some(request) = &page.request {
            page.duplicates = duplicates(&page.pages, scope, &request.client_name).await?;
        }
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
        let duplicates = duplicate_views(&self.pages, scope, &self.duplicates, &display).await?;
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
            replacement: replacement_choice(&duplicates),
            duplicates,
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
    async fn approve(&mut self, scope: &Scope, replace: bool) -> Result<Option<String>> {
        let Some(request) = self.request.clone() else {
            return Ok(None);
        };
        match self
            .pages
            .access
            .authorize_reviewed(
                scope,
                &self.params,
                &self.pages.resource(),
                Some(&request),
                &if replace {
                    self.duplicates.iter().map(|r| r.id).collect::<Vec<_>>()
                } else {
                    Vec::new()
                },
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
                        AuthorizeEvent::Approve { fields } => {
                            if let Some(replace) = replacement(&fields, !self.duplicates.is_empty())
                            {
                                redirect = self.approve(request.scope, replace).await?
                            } else {
                                self.error = Some(authorize_message(&OAuthError::InvalidRequest));
                            }
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
    SwitchOn {
        id: String,
    },
    SwitchOff {
        id: String,
    },
    #[serde(rename = "clear-flash")]
    ClearFlash {
        key: String,
    },
}
pub struct ConnectionRow {
    pub id: String,
    pub dom_id: String,
    pub toggle_id: String,
    pub revoke_id: String,
    pub agent_id: String,
    pub approved_client: String,
    pub enabled: bool,
    pub status: &'static str,
    /// Other stated IDs, sorted, comma-separated; empty when none.
    pub seen: String,
    /// Self-reported `harness/model-version-effort` values, newest first,
    /// comma-separated; empty when none.
    pub self_report: String,
    pub client_identity: String,
    pub last_used: Html,
    pub revoked: bool,
}
#[derive(Template)]
#[template(
    source = r#"{% import "datetime.html" as time %}{% call time::datetime(instant=instant) %}{% endcall %}"#,
    ext = "html"
)]
struct ConnectionInstant {
    instant: Instant,
}

#[derive(Template)]
#[template(path = "connections.html")]
struct ConnectionsView {
    flash: FlashMap<'static>,
    header: Html,
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
                    BuiltinColumn::new("agent", "App"),
                    BuiltinColumn::new("approved_client", "Approved from"),
                    BuiltinColumn::new("last_used", "Last used"),
                    BuiltinColumn::new("status", "Status"),
                ],
            )
            .map_err(|_| Error::Internal)?
            .with_record_fields([
                bilibi_ui::RecordField::new("seen", "Also known as"),
                bilibi_ui::RecordField::new("self_report", SelfReport::LABEL),
                bilibi_ui::RecordField::new("client_identity", ClientIdentity::LABEL),
            ]),
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
        let display = self.pages.display(&**client, scope).await?;
        let last_use = if error.is_none() {
            self.pages
                .integration
                .own_agent_last_use(&**client, scope)
                .await?
        } else {
            Default::default()
        };
        let rows = rows
            .into_iter()
            .map(|row| {
                Ok(ConnectionRow {
                    last_used: last_use
                        .get(&row.id.to_string())
                        .map(|at| {
                            let instant =
                                instant(&display, format!("connection-{}-used", row.id), *at)?;
                            Html::render(&ConnectionInstant { instant })
                                .map_err(|_| Error::Internal)
                        })
                        .transpose()?
                        .unwrap_or_else(|| Html::text("Not used yet")),
                    id: row.id.to_string(),
                    dom_id: format!("connections-{}", row.id),
                    toggle_id: format!("connection-toggle-{}", row.id),
                    revoke_id: format!("revoke-{}", row.id),
                    approved_client: row.approved_client,
                    enabled: row.enabled,
                    status: if row.revoked_at.is_some() {
                        "Ended"
                    } else if row.enabled {
                        "On"
                    } else {
                        "Off"
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
                    .slot("approved_client", Html::text(&row.approved_client))
                    .slot("last_used", row.last_used)
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
                    let mut actions = Html::render(&Toggle {
                        on: row.enabled,
                        label: &format!("{} connection", row.agent_id),
                        id: &row.toggle_id,
                        event: if row.enabled {
                            "switch_off"
                        } else {
                            "switch_on"
                        },
                        name: "id",
                        value: &row.id,
                        ..Toggle::default()
                    })
                    .map_err(|_| Error::Internal)?;
                    actions.push(
                        &Html::render(&IconButton {
                            id: &row.revoke_id,
                            event: "revoke",
                            name: "id",
                            value: &row.id,
                            ..IconButton::new("unlink", "Revoke connection")
                        })
                        .map_err(|_| Error::Internal)?,
                    );
                    rendered.actions = actions;
                }
                Ok(rendered)
            })
            .collect::<Result<Vec<_>>>()?;
        let columns = self.columns.flex_columns();
        let mut table = FlexTable::new("agent-connections", columns, rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.actions = true;
        table.empty = Some(TableEmpty::State(if let Some(error) = error {
            EmptyState::missing("Connections unavailable", error).id("connections-error")
        } else {
            EmptyState::missing("No AI apps connected yet", "").id("connections-empty")
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
impl ConnectionsPage {
    async fn switch(&self, scope: &Scope, id: &str, enable: bool) -> Result<()> {
        let id = id
            .parse::<i64>()
            .ok()
            .filter(|id| *id > 0)
            .ok_or(Error::Invalid)?;
        self.pages
            .access
            .set_enabled(scope, id, enable)
            .await
            .map_err(|error| match error {
                OAuthError::Other(error) => error,
                _ => Error::Forbidden,
            })
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
                        ConnectionsEvent::SwitchOn { id } => {
                            self.switch(request.scope, &id, true).await?;
                        }
                        ConnectionsEvent::SwitchOff { id } => {
                            self.switch(request.scope, &id, false).await?;
                        }
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
