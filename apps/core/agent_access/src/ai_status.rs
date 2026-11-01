//! The status bar's AI tag and the panel it opens: the `shell.ai` embed
//! (`contributions.rs`), rendered by Web in the status bar's centre and
//! mounted as the shell panel `#app-ai`. It shows the signed-in person their
//! own AI: whether an AI app is connected, which ones and when they were last
//! used, what they read and did (each line says what it operated on: the kind
//! of record, how many and, for one record, its name as the reader's own
//! access shows it), and, when none is connected, the same prompt and address
//! `/settings/ai-connections` offers. Everything here is the
//! person's own and tenant-bound (`AgentAccess::list_connections`,
//! `Integration::own_agent_activity`), so no capability is needed; an
//! impersonating or agent session sees only that it must use its own account.
//! No Bilimbi counterpart.
use crate::{
    setup_prompt,
    web::{html, instant, refusal, Instant, Pages},
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_integration::{Summary, Target};
use bilibi_records::Reference;
use bilibi_system::Instance;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Subscription},
    ActionLink, Html, StatusbarTag,
};
use std::{
    collections::HashMap,
    time::{Duration, SystemTime},
};

/// The panel lists this many of the newest requests.
const RECENT: usize = 10;
/// Fetched before the protocol handshake rows are left out.
const FETCH: i64 = 60;
const MANAGE: &str = "/settings/ai-connections";
/// The one page that lists every recorded request, for a reader who holds its
/// capability (`bilibi_integration::LIST`).
const ALL_ACTIVITY: &str = "/system/api-exchanges?direction=incoming";

pub(crate) struct AiStatus {
    pages: Pages,
}

/// One live connection as the panel lists it.
struct ConnectionView {
    id: i64,
    name: String,
    /// When it last made a request; `None` until it has.
    last_used: Option<Instant>,
    ends: Instant,
}
/// One request the person's AI made, in words.
struct ActivityView {
    id: String,
    when: Instant,
    what: String,
    /// The app that made the request, with "(disconnected)" once its
    /// connection is revoked; empty when the connection is not the person's.
    app: String,
    /// A refusal or failure, in words; empty when the request succeeded.
    outcome: &'static str,
}

#[derive(Template)]
#[template(path = "ai_status.html")]
struct AiStatusView {
    /// The rendered `StatusbarTag`.
    tag: Html,
    /// "Connected" or "Not connected", under the panel's title.
    state_text: &'static str,
    /// False for an impersonating or agent session, which manages nothing here.
    own_account: bool,
    connected: bool,
    connections: Vec<ConnectionView>,
    activity: Vec<ActivityView>,
    /// Only the person's own account gets the connect steps.
    show_connect: bool,
    prompt_text: String,
    address: String,
    /// Internet AI apps cannot reach this Bilibi: say to use a local program.
    local_only: bool,
    /// Offer the ChatGPT plugin ZIP; see `offers_plugin`.
    plugin: bool,
    plugin_href: &'static str,
    /// `/settings/ai-connections`, for the manual steps under Connect.
    manage: &'static str,
    /// The link to `/settings/ai-connections` beside the connections heading.
    manage_link: Html,
    /// The link to the page listing every request, beside the activity
    /// heading; empty for a reader who cannot open it.
    all_activity_link: Html,
}

impl AiStatus {
    pub(crate) fn new(pages: Pages) -> Self {
        Self { pages }
    }
    pub(crate) async fn view(&self, scope: &Scope) -> Result<String> {
        let now = SystemTime::now();
        // The person's own grants; a borrowed or agent session is refused.
        let (connections, own_account) = match self.pages.access.list_connections(scope).await {
            Ok(rows) => (rows, true),
            Err(error) => {
                refusal(error)?;
                (Vec::new(), false)
            }
        };
        let apps: HashMap<String, (String, bool)> = connections
            .iter()
            .map(|row| {
                (
                    row.id.to_string(),
                    (row.agent_id.clone(), row.revoked_at.is_some()),
                )
            })
            .collect();
        let live: Vec<_> = connections
            .into_iter()
            .filter(|row| row.revoked_at.is_none() && row.expires_at > now)
            .collect();
        // One connection at a time: naming records (below) takes connections
        // of its own, so none is held across it.
        let (instance, display, last_use, rows, all_activity) = {
            let client = self.pages.db.get().await?;
            let client = &**client;
            let instance = Instance::resolve(&self.pages.settings, client).await?;
            let display = self.pages.display(client, scope).await?;
            let last_use = if own_account {
                self.pages
                    .integration
                    .own_agent_last_use(client, scope)
                    .await?
            } else {
                Default::default()
            };
            let rows = if own_account && !live.is_empty() {
                self.pages
                    .integration
                    .own_agent_activity(client, scope, FETCH)
                    .await?
            } else {
                Vec::new()
            };
            let all_activity = own_account
                && bilibi_authz::holds_proof(client, scope, bilibi_integration::LIST)
                    .await
                    .unwrap_or(false);
            (instance, display, last_use, rows, all_activity)
        };
        let used = |id: i64| last_use.get(&id.to_string()).copied();
        let mut live = live;
        // The most recently used first, then the newest grant.
        live.sort_by(|a, b| used(b.id).cmp(&used(a.id)).then(b.id.cmp(&a.id)));
        let connections = live
            .iter()
            .map(|row| {
                Ok(ConnectionView {
                    id: row.id,
                    name: row.agent_id.clone(),
                    last_used: used(row.id)
                        .map(|at| {
                            instant(&display, format!("app-ai-connection-{}-used", row.id), at)
                        })
                        .transpose()?,
                    ends: instant(
                        &display,
                        format!("app-ai-connection-{}-ends", row.id),
                        row.expires_at,
                    )?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let rows: Vec<Summary> = rows.into_iter().filter(listed).take(RECENT).collect();
        let names = self.names(scope, &rows).await?;
        let activity = rows
            .iter()
            .map(|row| {
                Ok(ActivityView {
                    id: row.id.clone(),
                    when: instant(
                        &display,
                        format!("app-ai-activity-{}-at", row.id),
                        row.occurred_at,
                    )?,
                    what: self.what(row, &names),
                    app: match row.connection_id.as_ref().and_then(|id| apps.get(id)) {
                        Some((name, true)) => format!("{name} (disconnected)"),
                        Some((name, false)) => name.clone(),
                        None => String::new(),
                    },
                    outcome: outcome(row),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let connected = !connections.is_empty();
        let tag_title = if !own_account {
            "AI apps are connected from your own account".to_owned()
        } else if !connected {
            "No AI app connected. Open to connect one.".to_owned()
        } else {
            let mut names: Vec<&str> = Vec::new();
            for connection in &connections {
                if !names.contains(&connection.name.as_str()) {
                    names.push(&connection.name);
                }
            }
            let last = live.iter().filter_map(|c| used(c.id)).max();
            match last {
                Some(at) => format!("{} · last used {}", names.join(", "), ago(at, now)),
                None => format!("{} · not used yet", names.join(", ")),
            }
        };
        let prompt = own_account.then(|| setup_prompt::prompt(&instance));
        let address = setup_prompt::address(&instance);
        let render = |rendered: askama::Result<Html>| rendered.map_err(|_| Error::Internal);
        html(AiStatusView {
            tag: render(Html::render(&StatusbarTag {
                id: "app-ai-tag",
                label: "AI",
                title: &tag_title,
                controls: "app-ai-panel",
                state: if connected {
                    "connected"
                } else {
                    "not-connected"
                },
                quiet: !connected,
            }))?,
            manage_link: render(Html::render(&ActionLink {
                id: "app-ai-manage",
                icon: "manage",
                title: "Manage connections",
                href: MANAGE,
                method: "",
                body: Html::text("Manage"),
            }))?,
            all_activity_link: if all_activity {
                render(Html::render(&ActionLink {
                    id: "app-ai-all-activity",
                    icon: "",
                    title: "Every request your AI made",
                    href: ALL_ACTIVITY,
                    method: "",
                    body: Html::text("All activity"),
                }))?
            } else {
                Html::EMPTY
            },
            state_text: if connected {
                "Connected"
            } else {
                "Not connected"
            },
            own_account,
            connected,
            connections,
            activity,
            show_connect: prompt.is_some(),
            prompt_text: prompt.unwrap_or_default(),
            local_only: !instance.public_address(),
            plugin: own_account && crate::offers_plugin(scope, &instance.origin),
            plugin_href: crate::PLUGIN_DOWNLOAD,
            address,
            manage: MANAGE,
        })
    }

    /// The names of the single records the listed requests operated on, as
    /// the reader's own access shows them: `Records::resolve` projects each
    /// kind's label field with the reader's scope and puts the record to its
    /// owner's access check, so a masked, out-of-reach, removed or unmounted
    /// record has no name here and its line says only the kind and count.
    async fn names(
        &self,
        scope: &Scope,
        rows: &[Summary],
    ) -> Result<HashMap<(String, String), String>> {
        let mut wanted: Vec<Reference> = Vec::new();
        for row in rows {
            for target in &row.targets {
                if let (Some(kind), Some(id), 1) = (&target.kind, &target.id, target.count) {
                    let reference = Reference::new(kind.clone(), id.clone());
                    if !wanted.contains(&reference) {
                        wanted.push(reference);
                    }
                }
            }
        }
        if wanted.is_empty() {
            return Ok(HashMap::new());
        }
        Ok(self
            .pages
            .records
            .resolve(scope, &wanted)
            .await?
            .into_iter()
            .filter_map(|resolved| {
                Some((
                    (resolved.reference.kind, resolved.reference.id),
                    resolved.label?,
                ))
            })
            .collect())
    }

    /// One recorded request in words: "Read 12 companies", "Saved a change to
    /// Company Acme Sdn Bhd", "Prepared a change to 3 invoices for you to
    /// review". A request that recorded no record kind (one from before the
    /// kinds were recorded, or an operation no record kind owns) keeps the
    /// operation's registered title, or the generic line for its tool.
    fn what(&self, row: &Summary, names: &HashMap<(String, String), String>) -> String {
        let step = self.step(&row.operation);
        let described = self.described(&row.targets, names);
        match (step, described) {
            (Step::Read, Some(what)) => format!("Read {what}"),
            (Step::Plan, Some(what)) => format!("Prepared a change to {what} for you to review"),
            (Step::Commit, Some(what)) => format!("Saved a change to {what}"),
            (Step::Read, None) => self.titled(row, "Read records"),
            (Step::Plan, None) => "Prepared a change for you to review".to_owned(),
            (Step::Commit, None) => "Saved a change".to_owned(),
            (Step::Other, _) => self.titled(row, "Read records"),
        }
    }
    /// The registered title of a called operation, an unknown key as "Ran
    /// key", `generic` for the generic tools that name no operation.
    fn titled(&self, row: &Summary, generic: &str) -> String {
        let operation = row.operation.as_str();
        let key = row
            .targets
            .first()
            .map(|target| target.operation.as_str())
            .or_else(|| {
                operation
                    .strip_prefix("/api/v1/operations/")
                    .and_then(|rest| rest.strip_suffix("/call"))
            })
            .or_else(|| (!operation.starts_with("mcp.")).then_some(operation));
        match key {
            Some(key) => match self.pages.registry.operations.get(key) {
                Some(registered) => registered.definition.title.to_owned(),
                None => format!("Ran {key}"),
            },
            None => generic.to_owned(),
        }
    }
    /// The records a request operated on, grouped by kind: "Company Acme Sdn
    /// Bhd", "12 customers", "1 invoice and 2 contacts". `None` when no
    /// operation of the request named a record kind.
    fn described(
        &self,
        targets: &[Target],
        names: &HashMap<(String, String), String>,
    ) -> Option<String> {
        let mut groups: Vec<(&str, u32, Option<&str>)> = Vec::new();
        for target in targets {
            let Some(kind) = target.kind.as_deref() else {
                continue;
            };
            match groups.iter_mut().find(|group| group.0 == kind) {
                Some(group) => {
                    group.1 += target.count;
                    group.2 = None;
                }
                None => groups.push((kind, target.count, target.id.as_deref())),
            }
        }
        let parts: Vec<String> = groups
            .into_iter()
            .map(|(kind, count, id)| {
                let (singular, plural) = self.pages.records.kind_nouns(kind);
                let name = id
                    .filter(|_| count == 1)
                    .and_then(|id| names.get(&(kind.to_owned(), id.to_owned())));
                match (count, name) {
                    (1, Some(name)) => format!("{} {name}", capitalised(&singular)),
                    (1, None) => format!("1 {singular}"),
                    (count, _) => format!("{count} {plural}"),
                }
            })
            .collect();
        match parts.as_slice() {
            [] => None,
            [only] => Some(only.clone()),
            [head @ .., last] => Some(format!("{} and {last}", head.join(", "))),
        }
    }
}

impl LiveState<Scope, Error> for AiStatus {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(_) => return Err(Error::Invalid),
            }
            Ok(LiveBody {
                html: self.view(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![
            self.pages.integration.subscribe_agent_activity(),
            self.pages.access.subscribe_connections(),
        ]
    }
}

/// "just now", "5 min ago", "3 hours ago", "yesterday", "4 days ago".
pub(crate) fn ago(at: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(at).unwrap_or(Duration::ZERO).as_secs();
    match secs {
        0..=59 => "just now".to_owned(),
        60..=3599 => format!("{} min ago", secs / 60),
        3600..=7199 => "1 hour ago".to_owned(),
        7200..=86_399 => format!("{} hours ago", secs / 3600),
        86_400..=172_799 => "yesterday".to_owned(),
        172_800..=2_591_999 => format!("{} days ago", secs / 86_400),
        2_592_000..=5_183_999 => "1 month ago".to_owned(),
        _ => format!("{} months ago", secs / 2_592_000),
    }
}
/// Which part of an agent's work a recorded request was.
enum Step {
    Read,
    Plan,
    Commit,
    Other,
}
impl AiStatus {
    /// `/api/v1` records a called operation under its registry key; `/mcp`
    /// records the tool.
    fn step(&self, operation: &str) -> Step {
        match operation {
            "mcp.read" => Step::Read,
            "mcp.plan" | "agent_api.plan" => Step::Plan,
            "mcp.commit" | "agent_api.commit" => Step::Commit,
            other
                if self.pages.registry.operations.contains_key(other)
                    || (other.starts_with("/api/v1/operations/") && other.ends_with("/call")) =>
            {
                Step::Read
            }
            _ => Step::Other,
        }
    }
}
/// Whether the short list shows a request. The protocol's own rows
/// (initialize, ping, listing the tools), looking an action up, reading how
/// it works and opening an app card say nothing about what the AI did for the
/// person, so they stay on the full activity page.
fn listed(row: &Summary) -> bool {
    let operation = row.operation.as_str();
    !(operation.starts_with("mcp.") && !matches!(operation, "mcp.read" | "mcp.plan" | "mcp.commit")
        || operation == "/api/v1/search"
        || operation.starts_with("/api/v1/guides/")
        || operation.starts_with("/api/v1/operations/") && !operation.ends_with("/call"))
}
fn outcome(row: &Summary) -> &'static str {
    match row.response_status {
        Some(401) => "Sign-in needed",
        Some(403) => "Not allowed",
        Some(404) => "Not found",
        Some(429) => "Too many requests",
        Some(400..=499) => "Refused",
        Some(500..) => "Failed",
        _ => "",
    }
}
fn capitalised(word: &str) -> String {
    let mut letters = word.chars();
    letters
        .next()
        .map(|first| first.to_uppercase().chain(letters).collect())
        .unwrap_or_default()
}
