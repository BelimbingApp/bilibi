//! Module-owned live state. Web only owns routing, guards, targeting and transport.
//! Session rechecks mirror Bilimbi `BilimbiWeb.RouteAccess`: an expired scope
//! returns through the page's HTTP guard so it can redirect to sign-in with a flash.
use crate::{routes, App};
use axum::extract::ws::{Message, WebSocket};
use bilibi_base::agent_api::page::PageContracts;
use bilibi_base::perf::{Finish, Kind, Outcome};
use bilibi_database::{page_write, Error, Result};
use bilibi_module_registry::contributions::{Access, Embed, EMBEDS};
use bilibi_security::Scope;
use bilibi_ui::page::{
    Change, LiveBody, LiveInput, LiveRequest, LiveState, PageRequest, PanelSpec,
};
use bilibi_ui::params;
use bilibi_ui::upload::{ClientEntry, CHUNK_SIZE, MAX_ENTRIES};
use bilibi_ui::{LIVE_EVENT_LIMIT, LIVE_EVENT_TOO_LARGE};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};

type State = Box<dyn LiveState<Scope, Error>>;
/// A hard allocation cap above the accepted JSON-event budget. The headroom
/// lets stale clients receive an acknowledged refusal without disconnecting.
/// Upload chunks retain their separate CHUNK_SIZE check in `Session::chunk`.
pub const FRAME_LIMIT: usize = LIVE_EVENT_LIMIT * 2;
/// The header of a binary upload frame: the entry id and the offset the bytes
/// start at, both big-endian `u32`.
const CHUNK_HEADER: usize = 8;
type Service = bilibi_ui::page::PageService<Scope, Error>;
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

struct Mounted {
    state: State,
    access: Access,
    /// The page or panel service, whose page writes declaration permits
    /// this state's event writes.
    service: &'static str,
    jobs: JoinSet<()>,
    generation: u64,
}
impl Mounted {
    fn new(
        mut state: State,
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
impl Drop for Mounted {
    fn drop(&mut self) {
        self.jobs.abort_all();
    }
}
struct Panel {
    spec: PanelSpec,
    mounted: Mounted,
}
struct Session {
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
    page: Option<Mounted>,
    panels: HashMap<String, Panel>,
    /// `EmbedContext::Shell` panels (the bell), keyed by their element id.
    /// They outlive navigation and are never mounted in a framed page.
    shell: HashMap<String, Panel>,
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
}
impl Session {
    async fn scope(&mut self, app: &App, token: &str) -> Result<Scope> {
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
    ) -> Result<Scope> {
        let admission = routes::admit(app, access, Some(token), None).await;
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
            Ok(routes::Admission::Admitted(scope)) => scope.ok_or(Error::Forbidden),
            Ok(routes::Admission::StepUp(_)) => {
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
        app.step_up.prompt(&scope, &back).await.ok()
    }
    /// Refuses a scope the socket was not opened for; nothing acts on it.
    fn bound(&mut self, scope: Scope) -> Result<Scope> {
        if Identity::of(&scope) != self.identity {
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
        let (bound, params) = routes::resolve(app, &path)?;
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
                path: &params,
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
        let (bound, params) = routes::resolve(app, path)?;
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
        Ok(Some(params))
    }
    /// Mounts each shell embed the actor may see, once per socket, and drops
    /// one whose access is gone. A refused embed renders nothing in the shell.
    async fn sync_shell(&mut self, app: &App, token: &str, scope: &Scope) -> Result<()> {
        if self.framed {
            return Ok(());
        }
        for slot in crate::shell::embeds(app)? {
            match routes::check_access(app, slot.embed.access, Some(token)).await {
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
            let service: Arc<Service> = app.services.get(slot.embed.service)?;
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
        routes::check_access(app, panel.mounted.access, Some(token)).await?;
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
    async fn page_message(&mut self, app: &App, scope: &Scope, body: LiveBody) -> Result<Value> {
        let topbar_actions =
            routes::topbar_actions(&routes::resolve(app, &self.path)?.0, self.framed);
        let html = crate::shell::render(
            app,
            Some(scope),
            &self.path,
            self.framed,
            true,
            &body.html,
            topbar_actions,
        )
        .await?
        .html;
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
    async fn sync_panels(&mut self, app: &App, token: &str, scope: &Scope) -> Result<Vec<Value>> {
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
                    routes::check_access(app, embed.access, Some(token)).await?;
                    let service: Arc<Service> = app.services.get(embed.service)?;
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
                let panel = self.panels.get_mut(&id).ok_or(Error::Internal)?;
                routes::check_access(app, panel.mounted.access, Some(token)).await?;
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
    fn mounted_mut(&mut self, target: &str) -> Result<&mut Mounted> {
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
        let holds = |mounted: &mut Mounted| {
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
    ) -> Result<Vec<Value>> {
        if entries.is_empty()
            || entries.len() > MAX_ENTRIES
            || entries.iter().any(|entry| {
                entry.name.is_empty() || entry.name.len() > 255 || entry.content_type.len() > 255
            })
        {
            return Err(Error::Invalid);
        }
        self.scope(app, token).await?;
        let mut next = self.uploads;
        let mounted = self.mounted_mut(target)?;
        let registrations = mounted
            .state
            .uploads()
            .and_then(|uploads| uploads.register(upload, entries, &mut next))
            .ok_or(Error::Invalid)?;
        self.uploads = next;
        let mut messages = vec![
            json!({"type":"upload", "target":target, "upload":upload, "entries":registrations}),
        ];
        messages.extend(self.update(app, token, target, LiveInput::Refresh).await?);
        Ok(messages)
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
        let outcome = self
            .mounted_mut(&target)?
            .state
            .uploads()
            .ok_or(Error::Invalid)?
            .append(id, offset, &frame[CHUNK_HEADER..]);
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
    /// Perf identity for an accepted event: the page it mounts, or the page it
    /// acts on. No identity (an unroutable path, nothing mounted) is not recorded.
    fn identity(&self, app: &App, event: &Event) -> Option<String> {
        let route = match event {
            Event::Mount { path, .. } | Event::Navigate { path, .. } => {
                routes::resolve(app, path).ok()?.0.route.path.to_owned()
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
            } => self.upload(app, token, &target, &upload, entries).await?,
            Event::UploadCancel { target, id } => {
                self.upload_cancel(app, token, &target, id).await?
            }
        };
        let client = app.db.get().await?;
        let scope = app.security.sessions.activity(&**client, token).await?;
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

pub async fn run(app: App, token: String, identity: Identity, mut socket: WebSocket, framed: bool) {
    let (sender, mut wakes) = mpsc::channel(64);
    let mut session = Session {
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
