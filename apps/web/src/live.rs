//! Module-owned live state. Web only owns routing, guards, targeting and transport.
//! Session rechecks mirror Bilimbi `BilimbiWeb.RouteAccess`: an expired scope
//! returns through the page's HTTP guard so it can redirect to sign-in with a flash.
//! One transport serves both sides (ADR 16): the staff application on `/live`
//! and a tenant's customer portal on `/portal/live`; a [`Side`] supplies what
//! differs (the session, routes, access and chrome).
use crate::{routes, App};
use axum::extract::ws::{Message, WebSocket};
use bilibi_base::agent_api::page::PageContracts;
use bilibi_base::media::{Media, StagedUpload, Staging, StagingRequest};
use bilibi_base::perf::{Finish, Kind, Outcome};
use bilibi_database::{page_write, Error, Result};
use bilibi_module_registry::contributions::{Access, Embed, Route, EMBEDS};
use bilibi_security::Scope;
use bilibi_ui::page::{
    Change, LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PanelSpec,
};
use bilibi_ui::params;
use bilibi_ui::upload::{ClientEntry, Registration, UploadError, CHUNK_SIZE, MAX_ENTRIES};
use bilibi_ui::{LIVE_EVENT_LIMIT, LIVE_EVENT_TOO_LARGE};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};

type State<C> = Box<dyn LiveState<C, Error>>;
/// A hard allocation cap above the accepted JSON-event budget. The headroom
/// lets stale clients receive an acknowledged refusal without disconnecting.
/// Upload chunks retain their separate CHUNK_SIZE check in `Session::chunk`.
pub const FRAME_LIMIT: usize = LIVE_EVENT_LIMIT * 2;
/// The header of a binary upload frame: the entry id and the offset the bytes
/// start at, both big-endian `u32`.
const CHUNK_HEADER: usize = 8;
type Service<C> = bilibi_ui::page::PageService<C, Error>;

/// The live boundary's limits, shared by both sides and read from Session's
/// operator settings (`session.live.*`): open sockets per session and on this
/// server, and messages per session per minute. A socket is counted from its
/// upgrade until it closes; a message is counted before it is decoded, so an
/// excess one reaches no route admission, Authz decision, owner or session
/// activity. Counts are per server, as the HTTP throttles' are.
#[derive(Default)]
pub struct Limits {
    sockets: std::sync::Mutex<Sockets>,
    messages: std::sync::Mutex<HashMap<String, (u64, u32)>>,
}
#[derive(Default)]
struct Sockets {
    total: u32,
    per_session: HashMap<String, u32>,
}
/// One admitted socket; dropping it frees its place.
pub struct Slot {
    limits: Arc<Limits>,
    token: String,
}
impl Drop for Slot {
    fn drop(&mut self) {
        let mut sockets = self
            .limits
            .sockets
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        sockets.total = sockets.total.saturating_sub(1);
        if let Some(count) = sockets.per_session.get_mut(&self.token) {
            *count -= 1;
            if *count == 0 {
                sockets.per_session.remove(&self.token);
            }
        }
    }
}
/// The operator's live limits: sockets per session, sockets in total and
/// messages per session per minute.
pub struct Policy {
    pub per_session: u32,
    pub total: u32,
    pub messages: u32,
}
impl Policy {
    pub async fn read(app: &App) -> Result<Self> {
        use bilibi_base::session::{
            LIVE_MESSAGES_PER_MINUTE_KEY, LIVE_SOCKETS_PER_SESSION_KEY, LIVE_SOCKETS_TOTAL_KEY,
        };
        let client = app.db.get().await?;
        let mut values = [0u32; 3];
        for (value, key) in values.iter_mut().zip([
            LIVE_SOCKETS_PER_SESSION_KEY,
            LIVE_SOCKETS_TOTAL_KEY,
            LIVE_MESSAGES_PER_MINUTE_KEY,
        ]) {
            *value = app
                .settings
                .get_global(&**client, key)
                .await?
                .as_u64()
                .and_then(|limit| u32::try_from(limit).ok())
                .ok_or(Error::Internal)?;
        }
        let [per_session, total, messages] = values;
        Ok(Self {
            per_session,
            total,
            messages,
        })
    }
}
/// The refusal of a socket over either limit.
pub const TOO_MANY_SOCKETS: &str =
    "Too many pages are open. Close some tabs, then reload this page.";
/// The refusal of a message over the session's budget; nothing acted.
const TOO_MANY_MESSAGES: &str = "Too many actions at once. Wait a moment and try again.";
/// Refused messages in a row after which a socket is closed: a page that
/// keeps sending past its budget is not an ordinary one.
const REFUSALS_BEFORE_CLOSE: u32 = 100;
impl Limits {
    /// A place for one more socket of the session `token`, or `None` when the
    /// session or this server already holds its limit.
    pub fn admit(self: &Arc<Self>, token: &str, policy: &Policy) -> Option<Slot> {
        let mut sockets = self.sockets.lock().unwrap_or_else(|p| p.into_inner());
        let held = sockets.per_session.get(token).copied().unwrap_or(0);
        if held >= policy.per_session || sockets.total >= policy.total {
            return None;
        }
        sockets.total += 1;
        sockets.per_session.insert(token.to_owned(), held + 1);
        Some(Slot {
            limits: self.clone(),
            token: token.to_owned(),
        })
    }
    /// Whether the session `token` may send one more message this minute.
    /// Memory is bounded: a full map keeps only the current minute's keys.
    fn message(&self, token: &str, per_minute: u32) -> bool {
        const MAX_KEYS: usize = 16_384;
        let minute = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() / 60)
            .unwrap_or(0);
        let mut windows = self.messages.lock().unwrap_or_else(|p| p.into_inner());
        if windows.len() >= MAX_KEYS {
            windows.retain(|_, (window, _)| *window == minute);
            if windows.len() >= MAX_KEYS && !windows.contains_key(token) {
                return false;
            }
        }
        let entry = windows.entry(token.to_owned()).or_insert((minute, 0));
        if entry.0 != minute {
            *entry = (minute, 0);
        }
        if entry.1 >= per_minute {
            return false;
        }
        entry.1 += 1;
        true
    }
}

/// A path resolved to one side's page route.
pub struct Resolved<C: 'static> {
    pub route: &'static Route,
    pub page: Arc<Service<C>>,
    pub params: HashMap<String, String>,
}
/// What a route or embed access concluded for this socket's session: open
/// (with the proven context, none for a public access), or a step-up owed.
pub enum Gate<C> {
    Open(Option<C>),
    StepUp,
}
/// The side a live socket serves (ADR 16). The staff side reads `/live`
/// with the staff session and draws the shell; the portal side reads
/// `/portal/live` with a customer session, serves only portal routes and
/// draws the portal layout. A session of the other kind never resolves.
pub trait Side: Send + Sync + 'static {
    type Context: Send + Sync + 'static;
    /// Whether the shell's own panels (bell, search, AI tag) mount.
    const SHELL: bool;
    fn resolve(app: &App, path: &str) -> Result<Resolved<Self::Context>>;
    fn admit(
        app: &App,
        access: Access,
        token: &str,
    ) -> impl std::future::Future<Output = Result<Gate<Self::Context>>> + Send;
    /// Records the session's activity after an accepted event.
    fn activity(
        app: &App,
        token: &str,
    ) -> impl std::future::Future<Output = Result<Self::Context>> + Send;
    fn scope(context: &Self::Context) -> &Scope;
    /// The page's chrome around a reply's html.
    fn chrome(
        app: &App,
        context: &Self::Context,
        path: &str,
        framed: bool,
        html: &str,
    ) -> impl std::future::Future<Output = Result<String>> + Send;
    /// Where a step-up the page's facade asked for is confirmed, if anywhere.
    fn prompt(
        app: &App,
        context: &Self::Context,
        back: &str,
    ) -> impl std::future::Future<Output = Option<String>> + Send;
}
/// The staff application's side: the shell around every page.
pub struct Staff;
impl Side for Staff {
    type Context = Scope;
    const SHELL: bool = true;
    fn resolve(app: &App, path: &str) -> Result<Resolved<Scope>> {
        let (bound, params) = routes::resolve(app, path)?;
        Ok(Resolved {
            route: bound.route,
            page: bound.page,
            params,
        })
    }
    async fn admit(app: &App, access: Access, token: &str) -> Result<Gate<Scope>> {
        Ok(match routes::admit(app, access, Some(token), None).await? {
            routes::Admission::Admitted(scope) => Gate::Open(scope),
            routes::Admission::StepUp(_) => Gate::StepUp,
        })
    }
    async fn activity(app: &App, token: &str) -> Result<Scope> {
        let client = app.db.get().await?;
        app.security.sessions.activity(&**client, token).await
    }
    fn scope(context: &Scope) -> &Scope {
        context
    }
    async fn chrome(
        app: &App,
        context: &Scope,
        path: &str,
        framed: bool,
        html: &str,
    ) -> Result<String> {
        let topbar_actions = routes::topbar_actions(&routes::resolve(app, path)?.0, framed);
        Ok(
            crate::shell::render(app, Some(context), path, framed, true, html, topbar_actions)
                .await?
                .html,
        )
    }
    async fn prompt(app: &App, context: &Scope, back: &str) -> Option<String> {
        app.step_up.prompt(context, back).await.ok()
    }
}
fn page_target() -> String {
    "page".into()
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Event {
    Mount {
        path: String,
        #[serde(default)]
        params: HashMap<String, String>,
    },
    Navigate {
        path: String,
        #[serde(default)]
        params: HashMap<String, String>,
    },
    /// `path` is the browser's pathname after Back or Forward inside a page
    /// marked `data-history="params"`; the host keeps the mounted state when
    /// that path belongs to the same page and remounts otherwise.
    Params {
        params: HashMap<String, String>,
        #[serde(default)]
        path: Option<String>,
    },
    #[serde(rename = "event")]
    Action {
        #[serde(default = "page_target")]
        target: String,
        payload: Value,
    },
    /// The files chosen in one of the target's file inputs (Bilimbi's upload
    /// preflight): the target's `Uploads` registers each or refuses it, and
    /// the reply names the id to stream each accepted one under.
    Upload {
        #[serde(default = "page_target")]
        target: String,
        upload: String,
        entries: Vec<ClientEntry>,
        /// A resumable input's files' SHA-256, by entry `ref`.
        #[serde(default)]
        sha256: HashMap<String, String>,
    },
    /// The person removed an entry before acting on it.
    UploadCancel {
        #[serde(default = "page_target")]
        target: String,
        id: u64,
    },
}
fn decode(text: &str) -> (Option<u64>, Result<Event>) {
    // Separate transport metadata before decoding the strict module intent. Keep
    // valid request IDs even on rejection so the browser can acknowledge errors.
    let Ok(mut value) = serde_json::from_str::<Value>(text) else {
        return (None, Err(Error::Invalid));
    };
    let Some(object) = value.as_object_mut() else {
        return (None, Err(Error::Invalid));
    };
    let request_id = match object.remove("request_id") {
        None | Some(Value::Null) => None,
        Some(value) => match serde_json::from_value::<u64>(value) {
            Ok(id) => Some(id),
            Err(_) => return (None, Err(Error::Invalid)),
        },
    };
    (
        request_id,
        serde_json::from_value(value).map_err(|_| Error::Invalid),
    )
}

/// Who a live socket acts for: the session's account, its operator while
/// impersonating, the company it works in and the CSRF secret the page was
/// rendered with. The secret rotates on every impersonation switch, so a
/// socket left over from before one (another tab of the same session) is
/// stale even when it switched back. The company is bound the same way: a
/// page shows and acts on the company it was opened in, so when the session
/// switches company in another tab (or loses its reach), the next event here
/// is refused before it acts and the page reloads under the new company. No
/// write started on one company can land on another.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Identity {
    user: i64,
    impersonator: Option<i64>,
    company: Option<i64>,
    csrf: String,
}
impl Identity {
    pub fn of(scope: &Scope) -> Self {
        Self {
            user: scope.user(),
            impersonator: scope.impersonator().map(|operator| operator.user()),
            company: scope.current_company().map(|company| company.get()),
            csrf: scope.csrf.clone(),
        }
    }
    /// The identity a page was rendered under: the proven session's account,
    /// operator and secret, with the company the page stamped on its shell
    /// (`data-company-id`, sent by `live.js` when it opens the socket) in
    /// place of the session's company now. The stamp narrows, never widens:
    /// every event still acts on the session's own scope, and a stamp that no
    /// longer matches it only makes the first event refuse and reload the
    /// page, which is what a reconnect after a switch needs.
    pub fn rendered(scope: &Scope, company: Option<i64>) -> Self {
        Self {
            company,
            ..Self::of(scope)
        }
    }
}

struct Wake {
    generation: u64,
    target: String,
    change: Option<Change>,
}
/// The page-write permit of one live event (ADR 01, `base/agent_api`
/// `page`): while the owner handles it, a business write its page writes
/// declaration does not cover is refused. Refresh and params write nothing.
fn page_event(
    app: &App,
    service: &'static str,
    input: &LiveInput<'_>,
) -> Result<Option<page_write::Event>> {
    let LiveInput::Event(value) = input else {
        return Ok(None);
    };
    let name = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let pages: Arc<PageContracts> = app.services.get(bilibi_base::agent_api::page::SERVICE)?;
    Ok(Some(page_write::Event {
        service,
        permit: pages.permit(service, &name),
        name,
    }))
}
async fn permitted<T>(
    event: Option<page_write::Event>,
    update: impl std::future::Future<Output = T>,
) -> T {
    match event {
        Some(event) => page_write::during(event, update).await,
        None => update.await,
    }
}

struct Mounted<C: 'static> {
    state: State<C>,
    access: Access,
    /// The page or panel service, whose page writes declaration permits
    /// this state's event writes.
    service: &'static str,
    jobs: JoinSet<()>,
    generation: u64,
}
impl<C: 'static> Mounted<C> {
    fn new(
        mut state: State<C>,
        access: Access,
        service: &'static str,
        generation: u64,
        target: &str,
        sender: &mpsc::Sender<Wake>,
    ) -> Self {
        let mut jobs = JoinSet::new();
        for mut subscription in state.subscriptions() {
            let sender = sender.clone();
            let target = target.to_owned();
            jobs.spawn(async move {
                loop {
                    let change = match subscription.receiver.recv().await {
                        Ok(change) => Some(change),
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => None,
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    };
                    if sender
                        .send(Wake {
                            generation,
                            target: target.clone(),
                            change,
                        })
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
        }
        Self {
            state,
            access,
            service,
            jobs,
            generation,
        }
    }
}
impl<C: 'static> Drop for Mounted<C> {
    fn drop(&mut self) {
        self.jobs.abort_all();
    }
}
struct Panel<C: 'static> {
    spec: PanelSpec,
    mounted: Mounted<C>,
}
struct Session<S: Side> {
    identity: Identity,
    /// Set when the session no longer holds `identity`: the socket then
    /// reloads its page under the session's current identity instead of
    /// acting, and closes.
    stale: bool,
    framed: bool,
    path: String,
    /// Route template of the mounted page, the stable identity Perf records.
    route: String,
    /// The discovered route the page mounted under, which decides whether a
    /// path the page or the browser names is still this page.
    page_route: Option<&'static bilibi_module_registry::contributions::Route>,
    page: Option<Mounted<S::Context>>,
    panels: HashMap<String, Panel<S::Context>>,
    /// `EmbedContext::Shell` panels (the bell), keyed by their element id.
    /// They outlive navigation and are never mounted in a framed page.
    shell: HashMap<String, Panel<S::Context>>,
    generation: u64,
    sender: mpsc::Sender<Wake>,
    /// The address the page last answered with: path and canonical query.
    canonical: String,
    /// Page-owned keys and the disjoint namespaces owned by live page panels.
    query: HashMap<String, String>,
    /// The browser's own query on a mount or a Back step, which the next
    /// page reply keeps in place of the canonical one when that only adds
    /// values the address left out (Bilimbi patches the URL only on change).
    address: Option<HashMap<String, String>>,
    /// The last upload entry id handed out; ids are unique per socket, so a
    /// frame can never reach another state's entry.
    uploads: u64,
    /// The open staging files of resumable entries, by entry id. A file goes
    /// when its entry does (consumed, removed, replaced or its state
    /// dropped), and stays when the socket does, so a reconnect resumes it.
    staged: HashMap<u64, StagedUpload>,
}
impl<S: Side> Session<S> {
    async fn scope(&mut self, app: &App, token: &str) -> Result<S::Context> {
        let page = self.page.as_ref().ok_or(Error::Invalid)?;
        let path = self.path.clone();
        let scope = self.admit(app, page.access, token, path).await?;
        self.bound(scope)
    }
    /// A page route's access for this socket. A step-up the session owes
    /// (never confirmed, or the window ran out while the page was open) sends
    /// the browser through `path` over HTTP, whose guard shows the prompt and
    /// comes back; nothing acts on the request.
    async fn admit(
        &mut self,
        app: &App,
        access: bilibi_module_registry::contributions::Access,
        token: &str,
        path: String,
    ) -> Result<S::Context> {
        let admission = S::admit(app, access, token).await;
        match admission {
            Err(Error::Unauthorized) => {
                // Unlike a page-owned refusal, an ended session must leave the
                // live error view. Reload through HTTP, whose route guard sends
                // the browser to sign-in and preserves the expiry feedback.
                self.path = path;
                self.stale = true;
                Err(Error::Unauthorized)
            }
            Err(error) => Err(error),
            Ok(Gate::Open(scope)) => scope.ok_or(Error::Forbidden),
            Ok(Gate::StepUp) => {
                self.path = path;
                self.stale = true;
                Err(Error::Unauthorized)
            }
        }
    }
    /// Where this page's browser confirms a step-up and comes back.
    async fn prompt(&mut self, app: &App, token: &str) -> Option<String> {
        let scope = self.scope(app, token).await.ok()?;
        let back = if self.canonical.is_empty() {
            self.path.clone()
        } else {
            self.canonical.clone()
        };
        S::prompt(app, &scope, &back).await
    }
    /// An embed's access for this socket: a step-up still owed is a refusal,
    /// since a panel has no prompt to send anyone to.
    async fn check(app: &App, access: Access, token: &str) -> Result<()> {
        match S::admit(app, access, token).await? {
            Gate::Open(_) => Ok(()),
            Gate::StepUp => Err(Error::Forbidden),
        }
    }
    /// Refuses a scope the socket was not opened for; nothing acts on it.
    fn bound(&mut self, scope: S::Context) -> Result<S::Context> {
        if Identity::of(S::scope(&scope)) != self.identity {
            self.stale = true;
            return Err(Error::Unauthorized);
        }
        Ok(scope)
    }
    async fn mount(
        &mut self,
        app: &App,
        token: &str,
        path: String,
        query: HashMap<String, String>,
    ) -> Result<Vec<Value>> {
        let bound = S::resolve(app, &path)?;
        let params = &bound.params;
        if self.framed && !bound.route.framed {
            return Err(Error::Forbidden);
        }
        let scope = self
            .admit(app, bound.route.access, token, path.clone())
            .await?;
        let scope = self.bound(scope)?;
        let state = bound
            .page
            .0
            .mount(PageRequest {
                operation: bound.route.operation,
                scope: Some(&scope),
                path: params,
                query: &query,
                body: &[],
                framed: self.framed,
            })
            .await?
            .ok_or(Error::Invalid)?;
        // Old state/subscribers cannot survive a route change or receive queued wakes.
        self.panels.clear();
        self.generation += 1;
        self.page = Some(Mounted::new(
            state,
            bound.route.access,
            bound.route.service,
            self.generation,
            "page",
            &self.sender,
        ));
        self.path = path;
        self.route = bound.route.path.to_owned();
        self.page_route = Some(bound.route);
        self.query = query.clone();
        self.address = Some(query);
        self.sync_shell(app, token, &scope).await?;
        self.update(app, token, "page", LiveInput::Refresh).await
    }
    /// Moves the mounted page to `path` without remounting, when that path is
    /// served by the same service and operation (a saved layout's address under
    /// the workspace), guarding the new route's access first. Returns the
    /// route's path parameters, or `None` when the path is another page's.
    async fn relocate(
        &mut self,
        app: &App,
        token: &str,
        path: &str,
    ) -> Result<Option<HashMap<String, String>>> {
        let bound = S::resolve(app, path)?;
        let current = self.page_route.ok_or(Error::Invalid)?;
        if bound.route.service != current.service
            || bound.route.operation != current.operation
            || (self.framed && !bound.route.framed)
        {
            return Ok(None);
        }
        let scope = self
            .admit(app, bound.route.access, token, path.to_owned())
            .await?;
        self.bound(scope)?;
        self.path = path.to_owned();
        self.route = bound.route.path.to_owned();
        self.page_route = Some(bound.route);
        if let Some(page) = &mut self.page {
            page.access = bound.route.access;
        }
        Ok(Some(bound.params))
    }
    /// Mounts each shell embed the actor may see, once per socket, and drops
    /// one whose access is gone. A refused embed renders nothing in the shell.
    async fn sync_shell(&mut self, app: &App, token: &str, scope: &S::Context) -> Result<()> {
        if self.framed || !S::SHELL {
            return Ok(());
        }
        for slot in crate::shell::embeds(app)? {
            match Self::check(app, slot.embed.access, token).await {
                Ok(_) => {}
                Err(Error::Forbidden) => {
                    self.shell.remove(&slot.id);
                    continue;
                }
                Err(error) => return Err(error),
            }
            if self.shell.contains_key(&slot.id) {
                continue;
            }
            let service: Arc<Service<S::Context>> = app.services.get(slot.embed.service)?;
            let empty = HashMap::new();
            let state = service
                .0
                .mount(PageRequest {
                    operation: slot.embed.operation,
                    scope: Some(scope),
                    path: &empty,
                    query: &empty,
                    body: &[],
                    framed: false,
                })
                .await?
                .ok_or(Error::Invalid)?;
            self.generation += 1;
            let mounted = Mounted::new(
                state,
                slot.embed.access,
                slot.embed.service,
                self.generation,
                &slot.id,
                &self.sender,
            );
            let spec = PanelSpec {
                id: slot.id.clone(),
                embed: slot.embed_id.to_owned(),
                path: HashMap::new(),
                query: HashMap::new(),
            };
            self.shell.insert(slot.id, Panel { spec, mounted });
        }
        Ok(())
    }
    async fn update(
        &mut self,
        app: &App,
        token: &str,
        target: &str,
        input: LiveInput<'_>,
    ) -> Result<Vec<Value>> {
        let scope = self.scope(app, token).await?;
        if target == "page" {
            let page = self.page.as_mut().ok_or(Error::Invalid)?;
            let event = page_event(app, page.service, &input)?;
            let mut body = permitted(
                event,
                page.state.update(LiveRequest {
                    scope: &scope,
                    credential: token,
                    input,
                }),
            )
            .await?;
            hold_flash(app, token, &mut body);
            // A page may move only among its own routes; any other path is a
            // module bug, not a navigation.
            if let Some(path) = body.path.as_deref().filter(|path| *path != self.path) {
                if self.relocate(app, token, path).await?.is_none() {
                    return Err(Error::Invalid);
                }
            }
            // Canonicalize child namespaces before the combined URL, but send
            // the parent first so its anchors exist when fragments arrive.
            let fragments = self.sync_panels(app, token, &scope).await?;
            let mut messages = vec![self.page_message(app, &scope, body).await?];
            messages.extend(fragments);
            return Ok(messages);
        }
        let panel = match self.shell.get_mut(target) {
            Some(panel) => panel,
            None => self.panels.get_mut(target).ok_or(Error::Invalid)?,
        };
        let access = panel.mounted.access;
        Self::check(app, access, token).await?;
        let panel = match self.shell.get_mut(target) {
            Some(panel) => panel,
            None => self.panels.get_mut(target).ok_or(Error::Invalid)?,
        };
        let event = page_event(app, panel.mounted.service, &input)?;
        let mut body = permitted(
            event,
            panel.mounted.state.update(LiveRequest {
                scope: &scope,
                credential: token,
                input,
            }),
        )
        .await?;
        hold_flash(app, token, &mut body);
        let owns_query = self.merge_panel_query(target, &body.query)?;
        let mut message = fragment(target, body);
        if owns_query {
            message["url"] = json!(self.url());
        }
        Ok(vec![message])
    }
    async fn page_message(
        &mut self,
        app: &App,
        scope: &S::Context,
        body: LiveBody,
    ) -> Result<Value> {
        let html = S::chrome(app, scope, &self.path, self.framed, &body.html).await?;
        let prefixes: Vec<_> = self
            .panels
            .values()
            .map(|p| p.mounted.state.query_prefix())
            .filter(|prefix| !prefix.is_empty())
            .collect();
        if body
            .query
            .keys()
            .any(|key| prefixes.iter().any(|prefix| key.starts_with(prefix)))
        {
            return Err(Error::Invalid);
        }
        self.query
            .retain(|key, _| prefixes.iter().any(|prefix| key.starts_with(prefix)));
        self.query.extend(body.query);
        let panel_params = !prefixes.is_empty();
        let url = self.url();
        Ok(
            json!({"type":"page", "target":"app", "html":html, "url":url, "panel_params":panel_params, "notice":body.notice, "rows":body.rows, "redirect":body.redirect, "events":events(&body.events)}),
        )
    }
    /// Replace exactly one panel's namespace; never let a child overwrite its
    /// parent or another panel. Shell state deliberately has no URL ownership.
    fn merge_panel_query(&mut self, id: &str, query: &HashMap<String, String>) -> Result<bool> {
        let Some(panel) = self.panels.get(id) else {
            return Ok(false);
        };
        let prefix = panel.mounted.state.query_prefix();
        if prefix.is_empty() {
            return Ok(false);
        }
        if !prefix.ends_with('_')
            || !prefix
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || c == b'_')
            || query.keys().any(|key| !key.starts_with(prefix))
            || self.panels.iter().any(|(other_id, other)| {
                let other_prefix = other.mounted.state.query_prefix();
                other_id != id
                    && !other_prefix.is_empty()
                    && (prefix.starts_with(other_prefix) || other_prefix.starts_with(prefix))
            })
        {
            return Err(Error::Invalid);
        }
        self.query.retain(|key, _| !key.starts_with(prefix));
        self.query.extend(query.clone());
        Ok(true)
    }
    fn url(&mut self) -> Option<String> {
        let canonical = params::address(&self.path, &self.query);
        // A list's first render keeps a clean address: the browser's own query
        // stands while every value it carries is the page's too, and only a
        // reply whose state moved writes the canonical query (Bilimbi's
        // push_patch). A value the page dropped or corrected is rewritten, as
        // the workspace's consumed `open` is.
        let url = match self.address.take() {
            Some(address)
                if address
                    .iter()
                    .all(|(key, value)| self.query.get(key) == Some(value)) =>
            {
                Some(params::address(&self.path, &address))
            }
            Some(_) => Some(canonical.clone()),
            None => (canonical != self.canonical).then(|| canonical.clone()),
        };
        self.canonical = canonical;
        url
    }
    async fn sync_panels(
        &mut self,
        app: &App,
        token: &str,
        scope: &S::Context,
    ) -> Result<Vec<Value>> {
        let specs = self.page.as_ref().ok_or(Error::Invalid)?.state.panels();
        if specs.len() > 32 {
            return Err(Error::Invalid);
        }
        let mut ids = std::collections::HashSet::new();
        for spec in &specs {
            if spec.id.is_empty()
                || spec.id.len() > 128
                || ["page", "shell", "app"].contains(&spec.id.as_str())
                || self.shell.contains_key(&spec.id)
                || !spec
                    .id
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(&c))
                || !ids.insert(&spec.id)
            {
                return Err(Error::Invalid);
            }
        }
        self.panels
            .retain(|id, panel| specs.iter().any(|s| &s.id == id && s == &panel.spec));
        let mut messages = Vec::new();
        for spec in specs {
            let id = spec.id.clone();
            let result = async {
                let newly_mounted = !self.panels.contains_key(&id);
                if newly_mounted {
                    let embed = app
                        .services
                        .catalog()?
                        .entries::<Embed>(EMBEDS)?
                        .iter()
                        .find(|e| e.id == spec.embed)
                        .ok_or(Error::NotFound)?
                        .get::<Embed>()?;
                    Self::check(app, embed.access, token).await?;
                    let service: Arc<Service<S::Context>> = app.services.get(embed.service)?;
                    let state = service
                        .0
                        .mount(PageRequest {
                            operation: embed.operation,
                            scope: Some(scope),
                            path: &spec.path,
                            query: &spec.query,
                            body: &[],
                            framed: self.framed,
                        })
                        .await?
                        .ok_or(Error::Invalid)?;
                    self.generation += 1;
                    let mounted = Mounted::new(
                        state,
                        embed.access,
                        embed.service,
                        self.generation,
                        &id,
                        &self.sender,
                    );
                    self.panels.insert(id.clone(), Panel { spec, mounted });
                }
                let access = self.panels.get(&id).ok_or(Error::Internal)?.mounted.access;
                Self::check(app, access, token).await?;
                let panel = self.panels.get_mut(&id).ok_or(Error::Internal)?;
                let input = if !panel.mounted.state.query_prefix().is_empty()
                    && (newly_mounted || self.address.is_some())
                {
                    LiveInput::Params(&self.query)
                } else {
                    LiveInput::Refresh
                };
                panel
                    .mounted
                    .state
                    .update(LiveRequest {
                        scope,
                        credential: token,
                        input,
                    })
                    .await
            }
            .await;
            let result = result.and_then(|body| {
                self.merge_panel_query(&id, &body.query)?;
                Ok(body)
            });
            match result {
                Ok(body) => messages.push(fragment(&id, body)),
                Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                Err(error) => {
                    if crate::server_fault(&error) {
                        eprintln!("error: live panel {id} on {} failed: {error:?}", self.path);
                    }
                    self.panels.remove(&id);
                    messages.push(json!({"type":"error", "target":id, "message":error.to_string(), "clear":true}));
                }
            }
        }
        Ok(messages)
    }
    /// The mounted state a target names: the page, a shell panel or a page
    /// panel. A panel's access is rechecked by the caller through `update`.
    fn mounted_mut(&mut self, target: &str) -> Result<&mut Mounted<S::Context>> {
        if target == "page" {
            return self.page.as_mut().ok_or(Error::Invalid);
        }
        match self.shell.get_mut(target) {
            Some(panel) => Ok(&mut panel.mounted),
            None => self
                .panels
                .get_mut(target)
                .map(|panel| &mut panel.mounted)
                .ok_or(Error::Invalid),
        }
    }
    /// The target whose `Uploads` holds the entry `id`, if any still does.
    fn upload_owner(&mut self, id: u64) -> Option<String> {
        let holds = |mounted: &mut Mounted<S::Context>| {
            mounted
                .state
                .uploads()
                .is_some_and(|uploads| uploads.contains(id))
        };
        if self.page.as_mut().is_some_and(holds) {
            return Some("page".into());
        }
        for (target, panel) in self.shell.iter_mut().chain(self.panels.iter_mut()) {
            if holds(&mut panel.mounted) {
                return Some(target.clone());
            }
        }
        None
    }
    /// Registers the files chosen in one input of `target` (Bilimbi's upload
    /// preflight), then re-renders the target so the rows appear. The page's
    /// guard runs first; a panel's own access is rechecked by the render.
    async fn upload(
        &mut self,
        app: &App,
        token: &str,
        target: &str,
        upload: &str,
        entries: Vec<ClientEntry>,
        sha256: HashMap<String, String>,
    ) -> Result<Vec<Value>> {
        if entries.is_empty()
            || entries.len() > MAX_ENTRIES
            || entries.iter().any(|entry| {
                entry.name.is_empty() || entry.name.len() > 255 || entry.content_type.len() > 255
            })
            || sha256.len() > entries.len()
            || sha256.values().any(|digest| digest.len() != 64)
        {
            return Err(Error::Invalid);
        }
        let scope = self.scope(app, token).await?;
        let resumable = self
            .mounted_mut(target)?
            .state
            .uploads()
            .and_then(|uploads| uploads.spec(upload))
            .is_some_and(|spec| spec.resumable);
        // Resolved once, and the connection is back in the pool before any
        // staging file is opened or hashed.
        let staging = if resumable {
            let media = app.services.get::<Media>(bilibi_base::media::SERVICE)?;
            let client = app.db.get().await?;
            media.staging(&**client).await.ok().flatten()
        } else {
            None
        };
        let mut next = self.uploads;
        let uploads = self
            .mounted_mut(target)?
            .state
            .uploads()
            .ok_or(Error::Invalid)?;
        let mut registrations = uploads
            .register(upload, entries, &mut next)
            .ok_or(Error::Invalid)?;
        // A resumable input's files are staged when the installation can and
        // the browser named their SHA-256; a refusal replaces the acceptance.
        if resumable {
            for registration in &mut registrations {
                if let Registration::Accepted { client_ref, id, .. } = registration {
                    let digest = sha256.get(client_ref.as_str()).map(String::as_str);
                    if let Some(error) =
                        uploads.place(*id, digest, staging.as_ref().map(Staging::limit))
                    {
                        *registration = Registration::Refused {
                            client_ref: std::mem::take(client_ref),
                            error,
                        };
                    }
                }
            }
        }
        self.uploads = next;
        if let Some(staging) = &staging {
            for registration in &mut registrations {
                self.stage(staging, S::scope(&scope), target, registration)
                    .await?;
            }
        }
        let mut messages = vec![
            json!({"type":"upload", "target":target, "upload":upload, "entries":registrations}),
        ];
        messages.extend(self.update(app, token, target, LiveInput::Refresh).await?);
        Ok(messages)
    }
    /// Opens the staging file of a staged entry the browser just announced
    /// and answers where to continue: the bytes a dropped connection left
    /// there, or, when every byte is there and hashes as announced, nothing.
    async fn stage(
        &mut self,
        staging: &Staging,
        scope: &Scope,
        target: &str,
        registration: &mut Registration,
    ) -> Result<()> {
        let Registration::Accepted {
            client_ref,
            id,
            received,
            done,
            ..
        } = registration
        else {
            return Ok(());
        };
        let mounted = self.mounted_mut(target)?;
        let service = mounted.service;
        let Some((upload, size, sha256)) = mounted
            .state
            .uploads()
            .and_then(|uploads| uploads.staging(*id))
            .map(|(upload, size, sha256)| (upload.to_owned(), size, sha256))
        else {
            return Ok(());
        };
        let request = StagingRequest {
            owner: service,
            upload: &upload,
            size,
            sha256,
        };
        let outcome = match staging.stage(scope, request).await {
            Ok(mut file) => {
                let outcome = self.settle_staged(target, *id, &mut file).await?;
                if outcome.is_some() {
                    self.staged.insert(*id, file);
                } else if let Some(file) = self.unshared(file) {
                    file.remove().await;
                }
                outcome
            }
            Err(error) => {
                eprintln!("error: an upload could not be staged: {error:?}");
                None
            }
        };
        match outcome {
            Some(outcome) => {
                *received = outcome.received;
                *done = outcome.done;
            }
            None => {
                if let Some(uploads) = self.mounted_mut(target)?.state.uploads() {
                    uploads.interrupt(*id);
                }
                *registration = Registration::Refused {
                    client_ref: std::mem::take(client_ref),
                    error: UploadError::Interrupted,
                };
            }
        }
        Ok(())
    }
    /// Records what a staging file holds on its entry: the bytes staged so
    /// far, or, once all of them are there, the checked bytes themselves (a
    /// first hash mismatch empties the file and starts again from zero).
    /// `None` interrupts the entry.
    async fn settle_staged(
        &mut self,
        target: &str,
        id: u64,
        file: &mut StagedUpload,
    ) -> Result<Option<bilibi_ui::ChunkOutcome>> {
        let size = self
            .mounted_mut(target)?
            .state
            .uploads()
            .and_then(|uploads| uploads.staging(id))
            .map(|(_, size, _)| size);
        let Some(size) = size else {
            return Ok(None);
        };
        let finished = if file.received() == size {
            match file.finish().await {
                Ok(bytes) => bytes,
                Err(_) => return Ok(None),
            }
        } else {
            None
        };
        let uploads = self
            .mounted_mut(target)?
            .state
            .uploads()
            .ok_or(Error::Invalid)?;
        Ok(match finished {
            Some(bytes) => uploads.complete(id, bytes),
            None => uploads.staged(id, file.received()),
        })
    }
    /// Removes the staging files whose entries are gone from every state.
    async fn prune_staged(&mut self) {
        let ids: Vec<u64> = self.staged.keys().copied().collect();
        for id in ids {
            if self.upload_owner(id).is_none() {
                if let Some(file) = self.staged.remove(&id).and_then(|file| self.unshared(file)) {
                    file.remove().await;
                }
            }
        }
    }
    /// A staging file whose entry is done with it, to remove: none while
    /// another entry on this socket still stages into it (the same file
    /// chosen again), and dropping it then only closes it.
    fn unshared(&self, file: StagedUpload) -> Option<StagedUpload> {
        (!self.staged.values().any(|other| other.same_file(&file))).then_some(file)
    }
    async fn upload_cancel(
        &mut self,
        app: &App,
        token: &str,
        target: &str,
        id: u64,
    ) -> Result<Vec<Value>> {
        self.scope(app, token).await?;
        let cancelled = self
            .mounted_mut(target)?
            .state
            .uploads()
            .is_some_and(|uploads| uploads.cancel(id));
        if !cancelled {
            return Err(Error::Invalid);
        }
        self.update(app, token, target, LiveInput::Refresh).await
    }
    /// Appends one binary frame to the entry it names. The reply carries the
    /// bytes now held, so the browser sends the next chunk; a completed or
    /// refused entry also re-renders its owner, under the owner's guard.
    async fn chunk(&mut self, app: &App, token: &str, frame: &[u8]) -> Result<Vec<Value>> {
        if frame.len() < CHUNK_HEADER || frame.len() > CHUNK_HEADER + CHUNK_SIZE {
            return Err(Error::Invalid);
        }
        let id = u64::from(u32::from_be_bytes([frame[0], frame[1], frame[2], frame[3]]));
        let offset = u64::from(u32::from_be_bytes([frame[4], frame[5], frame[6], frame[7]]));
        let Some(target) = self.upload_owner(id) else {
            // Removed or consumed meanwhile; the hook stops streaming it.
            return Ok(vec![json!({"type":"chunk", "id":id, "error":"unknown"})]);
        };
        let uploads = self
            .mounted_mut(&target)?
            .state
            .uploads()
            .ok_or(Error::Invalid)?;
        let outcome = if uploads.staging(id).is_some() {
            self.stage_chunk(&target, id, offset, &frame[CHUNK_HEADER..])
                .await?
        } else {
            uploads.append(id, offset, &frame[CHUNK_HEADER..])
        };
        match outcome {
            Ok(outcome) => {
                let mut messages = vec![
                    json!({"type":"chunk", "id":id, "received":outcome.received, "size":outcome.size, "done":outcome.done}),
                ];
                if outcome.done {
                    messages.extend(self.update(app, token, &target, LiveInput::Refresh).await?);
                }
                Ok(messages)
            }
            Err(error) => {
                let code = error.map_or("unknown", |error| error.code());
                let mut messages = vec![json!({"type":"chunk", "id":id, "error":code})];
                if error.is_some() {
                    messages.extend(self.update(app, token, &target, LiveInput::Refresh).await?);
                }
                Ok(messages)
            }
        }
    }
    /// Writes one chunk of a staged entry to its staging file; a refused or
    /// failed write interrupts the entry and removes the file.
    async fn stage_chunk(
        &mut self,
        target: &str,
        id: u64,
        offset: u64,
        bytes: &[u8],
    ) -> Result<std::result::Result<bilibi_ui::ChunkOutcome, Option<UploadError>>> {
        let settled = match self.staged.remove(&id) {
            Some(mut file) => match file.write(offset, bytes.to_vec()).await {
                Ok(_) => match self.settle_staged(target, id, &mut file).await? {
                    Some(outcome) => {
                        self.staged.insert(id, file);
                        Some(outcome)
                    }
                    None => {
                        if let Some(file) = self.unshared(file) {
                            file.remove().await;
                        }
                        None
                    }
                },
                Err(_) => {
                    if let Some(file) = self.unshared(file) {
                        file.remove().await;
                    }
                    None
                }
            },
            None => None,
        };
        Ok(settled.ok_or_else(|| {
            if let Ok(mounted) = self.mounted_mut(target) {
                if let Some(uploads) = mounted.state.uploads() {
                    uploads.interrupt(id);
                }
            }
            Some(UploadError::Interrupted)
        }))
    }
    /// Perf identity for an accepted event: the page it mounts, or the page it
    /// acts on. No identity (an unroutable path, nothing mounted) is not recorded.
    fn identity(&self, app: &App, event: &Event) -> Option<String> {
        let route = match event {
            Event::Mount { path, .. } | Event::Navigate { path, .. } => {
                S::resolve(app, path).ok()?.route.path.to_owned()
            }
            _ => self.route.clone(),
        };
        (!route.is_empty()).then(|| format!("event:{route}"))
    }
    async fn event(&mut self, app: &App, token: &str, event: Event) -> Result<Vec<Value>> {
        let messages = match event {
            Event::Mount { path, params } | Event::Navigate { path, params } => {
                self.mount(app, token, path, params).await?
            }
            // Back or Forward inside a `data-history="params"` page: the
            // historical path stays this page's state (its route parameters
            // join the params, after the query so the path wins) or, when it
            // names another page, mounts that page.
            Event::Params { params, path } => {
                self.query = params.clone();
                self.address = Some(params.clone());
                match path {
                    Some(path)
                        if path != self.path
                            && self
                                .panels
                                .values()
                                .any(|panel| !panel.mounted.state.query_prefix().is_empty()) =>
                    {
                        // A child query belongs to this record, not another
                        // record served by the same page service/operation.
                        self.mount(app, token, path, params).await?
                    }
                    Some(path) => match self.relocate(app, token, &path).await? {
                        Some(route_params) => {
                            let mut params = params;
                            params.extend(route_params);
                            self.update(app, token, "page", LiveInput::Params(&params))
                                .await?
                        }
                        None => self.mount(app, token, path, params).await?,
                    },
                    None => {
                        self.update(app, token, "page", LiveInput::Params(&params))
                            .await?
                    }
                }
            }
            Event::Action { target, payload } => {
                self.update(app, token, &target, LiveInput::Event(&payload))
                    .await?
            }
            Event::Upload {
                target,
                upload,
                entries,
                sha256,
            } => {
                self.upload(app, token, &target, &upload, entries, sha256)
                    .await?
            }
            Event::UploadCancel { target, id } => {
                self.upload_cancel(app, token, &target, id).await?
            }
        };
        let scope = S::activity(app, token).await?;
        self.bound(scope)?;
        Ok(messages)
    }
}
/// Holds the message a redirecting reply carries for the page it lands on.
fn hold_flash(app: &App, token: &str, body: &mut LiveBody) {
    if let (Some(to), Some((kind, message))) = (&body.redirect, body.flash.take()) {
        app.landing.hold(token, to, kind, message);
    }
}
fn fragment(id: &str, body: LiveBody) -> Value {
    json!({"type":"fragment", "target":id, "html":body.html, "notice":body.notice, "rows":body.rows, "redirect":body.redirect, "events":events(&body.events)})
}
/// Owner pushes for browser hooks, dispatched by live.js after the patch.
fn events(pushes: &[(&'static str, Value)]) -> Value {
    pushes
        .iter()
        .map(|(name, detail)| json!({"name":name, "detail":detail}))
        .collect()
}
fn fatal(error: &Error) -> bool {
    matches!(
        error,
        Error::Unauthorized
            | Error::Forbidden
            | Error::NotFound
            | Error::Database(_)
            | Error::Pool(_)
            | Error::Internal
    )
}

/// Sends the browser back through the address it holds: the page's own guard
/// then decides, under the session's current identity (or none).
async fn reload(socket: &mut WebSocket, path: &str) {
    let path = if path.is_empty() { "/" } else { path };
    let message = json!({"type":"page", "target":"app", "html":"", "url":Value::Null, "notice":"", "rows":0, "redirect":path, "request_id":Value::Null, "history":"replace"});
    let _ = socket.send(Message::Text(message.to_string().into())).await;
}

pub async fn run<S: Side>(
    app: App,
    token: String,
    identity: Identity,
    mut socket: WebSocket,
    framed: bool,
    slot: Slot,
    policy: Policy,
) {
    // Held for the socket's life: its place among the session's sockets.
    let _slot = slot;
    let mut refused = 0u32;
    let (sender, mut wakes) = mpsc::channel(64);
    let mut session = Session::<S> {
        identity,
        stale: false,
        framed,
        path: String::new(),
        route: String::new(),
        page_route: None,
        page: None,
        panels: HashMap::new(),
        shell: HashMap::new(),
        generation: 0,
        sender,
        canonical: String::new(),
        query: HashMap::new(),
        address: None,
        uploads: 0,
        staged: HashMap::new(),
    };
    // A termination of this socket's session (logout elsewhere, an operator,
    // a credential change) signs this page out at once: the browser is sent
    // back through the page it holds, whose guard forwards it to sign-in with
    // the expired-session message. A lagged receiver is covered by the next
    // guarded action or refresh, which refuses an ended session anyway.
    let mut terminations = app.security.sessions.subscribe_terminations();
    let mut refresh = tokio::time::interval(Duration::from_secs(15));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    refresh.tick().await;
    loop {
        let mut request_id = None;
        let mut target = "page".to_owned();
        let mut history = "replace";
        let result = tokio::select! {
            message = socket.recv() => match message {
                Some(Ok(Message::Text(text))) => {
                    // Over the session's budget: refused before it is read,
                    // so nothing is admitted, decided, mounted or extended.
                    if !app.live.message(&token, policy.messages) {
                        refused += 1;
                        if refused > REFUSALS_BEFORE_CLOSE {break;}
                        let message = json!({"type":"error", "target":"page", "message":TOO_MANY_MESSAGES, "clear":false, "request_id":Value::Null});
                        if socket.send(Message::Text(message.to_string().into())).await.is_err() {break;}
                        continue;
                    }
                    refused = 0;
                    if text.len() > LIVE_EVENT_LIMIT {
                        // Deserialize metadata only: ignored payloads are skipped,
                        // not allocated as a Value or delivered to a page handler.
                        #[derive(Default, Deserialize)]
                        struct Metadata {
                            request_id: Option<u64>,
                            target: Option<String>,
                        }
                        let metadata = serde_json::from_str::<Metadata>(&text).unwrap_or_default();
                        let message = json!({"type":"error", "target":metadata.target.as_deref().unwrap_or("page"), "message":LIVE_EVENT_TOO_LARGE, "clear":false, "request_id":metadata.request_id});
                        if socket.send(Message::Text(message.to_string().into())).await.is_err() {break;}
                        continue;
                    }
                    let (id, event) = decode(&text);
                    request_id = id;
                    match event {
                        Ok(event) => {
                            match &event {
                                Event::Action {target: id, ..} | Event::Upload {target: id, ..} | Event::UploadCancel {target: id, ..} => target = id.clone(),
                                _ => {}
                            }
                            if matches!(event, Event::Navigate {..} | Event::Params {path: None, ..} | Event::Action {..}) {history = "push";}
                            let identity = session.identity(&app, &event);
                            app.perf.observe(Kind::Event, identity.as_deref(), session.event(&app, &token, event), |result| {
                                Finish::outcome(if result.is_ok() { Outcome::Ok } else { Outcome::Error })
                            }).await
                        },
                        Err(error) => Err(error),
                    }
                },
                // An upload chunk: bytes for an entry the socket registered.
                Some(Ok(Message::Binary(frame))) => session.chunk(&app, &token, &frame).await,
                Some(Ok(Message::Ping(data))) => { if socket.send(Message::Pong(data)).await.is_err() {break;} continue; },
                Some(Ok(Message::Pong(_))) => continue,
                Some(Err(_)) => {
                    // A malformed peer can exceed even the hard allocation cap.
                    // Try to report the refusal before closing; the browser also
                    // keeps drafts on an unconfirmed disconnect if sending fails.
                    let message = json!({"type":"error", "target":"page", "message":"This request could not be received. Your draft is kept. Shorten the text or reconnect and try again.", "clear":false, "request_id":Value::Null});
                    let _ = socket.send(Message::Text(message.to_string().into())).await;
                    break;
                },
                _ => break,
            },
            wake = wakes.recv() => {
                let Some(wake) = wake else {break;};
                let mounted = if wake.target == "page" { session.page.as_ref() } else { session.shell.get(&wake.target).or_else(|| session.panels.get(&wake.target)).map(|p| &p.mounted) };
                if !mounted.is_some_and(|m| m.generation == wake.generation) {continue;}
                target = wake.target;
                match session.scope(&app, &token).await {
                    Ok(scope) => {
                        let scope = S::scope(&scope);
                        if wake.change.as_ref().is_some_and(|c| c.tenant != scope.tenant() || c.user.is_some_and(|user| user != scope.user())) {continue;}
                        session.update(&app, &token, &target, LiveInput::Refresh).await
                    },
                    Err(error) => Err(error),
                }
            },
            _ = refresh.tick() => {
                if session.page.is_none() {continue;}
                session.update(&app, &token, "page", LiveInput::Refresh).await
            },
            terminated = terminations.recv() => {
                match terminated {
                    Ok(event) if event.id == token => {
                        reload(&mut socket, &session.path).await;
                        break;
                    },
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    _ => continue,
                }
            },
        };
        // A stale socket's request is refused before it acted; the page is
        // reloaded rather than told of an error it did not cause. Any lost
        // authorization also returns through the HTTP guard, which sends an
        // ended session to sign-in with its expiry feedback. A step-up the
        // session owes reloads the same way, through the page's address.
        if session.stale || matches!(&result, Err(Error::Unauthorized)) {
            reload(&mut socket, &session.path).await;
            break;
        }
        // An owner's facade asked for step-up during this event: the
        // browser confirms (or enrolls) and comes back to this address, where
        // the person repeats the action. Nothing acted.
        if let Err(Error::StepUpRequired { reason, .. }) = &result {
            if *reason != "impersonating" {
                if let Some(destination) = session.prompt(&app, &token).await {
                    let message = json!({"type":"page", "target":"app", "html":"", "url":Value::Null, "notice":"", "rows":0, "redirect":destination, "request_id":request_id, "history":"push"});
                    if socket
                        .send(Message::Text(message.to_string().into()))
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
            }
        }
        let (mut messages, close) = match result {
            Ok(messages) => (messages, false),
            Err(error) => {
                if crate::server_fault(&error) {
                    eprintln!(
                        "error: live {target} event on {} failed: {error:?}",
                        session.path
                    );
                }
                let panel = target != "page" && target != "shell";
                let clear = fatal(&error);
                let parent_denied =
                    session.page.is_some() && session.scope(&app, &token).await.is_err();
                let close =
                    clear && (!panel || parent_denied || matches!(error, Error::Unauthorized));
                if panel && clear {
                    session.panels.remove(&target);
                    session.shell.remove(&target);
                }
                (
                    vec![
                        json!({"type":"error", "target":if close {"app"} else {&target}, "message":error.to_string(), "clear":clear}),
                    ],
                    close,
                )
            }
        };
        // After a failed panel is gone, so its entries' staging goes too.
        session.prune_staged().await;
        for message in &mut messages {
            message["request_id"] = json!(request_id);
            if message["type"] == "page" || message["type"] == "fragment" {
                message["history"] = json!(history);
            }
            if socket
                .send(Message::Text(message.to_string().into()))
                .await
                .is_err()
            {
                return;
            }
        }
        if close {
            break;
        }
    }
    // Dropping Session aborts all module subscription tasks and drops state/receivers.
}
