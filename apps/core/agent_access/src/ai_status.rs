//! The status bar's AI tag and the panel it opens: the `shell.ai` embed
//! (`contributions.rs`), rendered by Web in the status bar's centre and
//! mounted as the shell panel `#app-ai`. It shows the signed-in person their
//! own AI: whether an AI app is connected, which ones and when they were last
//! used, what they read and did, and, when none is connected, the same prompt
//! and address `/settings/ai-connections` offers. Everything here is the
//! person's own and tenant-bound (`AgentAccess::list_connections`,
//! `Integration::own_agent_activity`), so no capability is needed; an
//! impersonating or agent session sees only that it must use its own account.
//! No Bilimbi counterpart.
use crate::{
    setup_prompt,
    web::{html, refusal, Pages},
};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_integration::Summary;
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

pub(crate) struct AiStatus {
    pages: Pages,
}

/// One live connection as the panel lists it.
struct ConnectionView {
    id: i64,
    name: String,
    last_used: String,
    ends: String,
}
/// One request the person's AI made, in words.
struct ActivityView {
    id: String,
    when: String,
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
    manage: &'static str,
    /// The link to `/settings/ai-connections` beside the connections heading.
    manage_link: Html,
}

impl AiStatus {
    pub(crate) fn new(pages: Pages) -> Self {
        Self { pages }
    }
    pub(crate) async fn view(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let client = &**client;
        let instance = Instance::resolve(&self.pages.settings, client).await?;
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
        let last_use = if own_account {
            self.pages
                .integration
                .own_agent_last_use(client, scope)
                .await?
        } else {
            Default::default()
        };
        let mut connections: Vec<_> = connections
            .into_iter()
            .filter(|row| row.revoked_at.is_none() && row.expires_at > now)
            .map(|row| {
                let used = last_use.get(&row.id.to_string()).copied();
                ConnectionView {
                    id: row.id,
                    name: row.agent_id,
                    last_used: match used {
                        Some(at) => format!("Last used {}", ago(at, now)),
                        None => "Not used yet".to_owned(),
                    },
                    ends: ends(row.expires_at, now),
                }
            })
            .collect();
        // The most recently used first, then the newest grant.
        connections.sort_by(|a, b| {
            let used = |view: &ConnectionView| last_use.get(&view.id.to_string()).copied();
            used(b).cmp(&used(a)).then(b.id.cmp(&a.id))
        });
        let activity = if own_account && !connections.is_empty() {
            self.pages
                .integration
                .own_agent_activity(client, scope, FETCH)
                .await?
                .into_iter()
                .filter_map(|row| activity(&row, &self.pages.registry, &apps, now))
                .take(RECENT)
                .collect()
        } else {
            Vec::new()
        };
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
            let last = connections
                .iter()
                .filter_map(|c| last_use.get(&c.id.to_string()))
                .max()
                .copied();
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
/// "Ends in 89 days", "Ends tomorrow", "Ends today".
fn ends(at: SystemTime, now: SystemTime) -> String {
    let days = at.duration_since(now).unwrap_or(Duration::ZERO).as_secs() / 86_400;
    match days {
        0 => "Ends today".to_owned(),
        1 => "Ends tomorrow".to_owned(),
        _ => format!("Ends in {days} days"),
    }
}
/// One recorded request in words, or `None` for the protocol handshake rows
/// (initialize, ping, listing the tools), which say nothing about what the
/// AI did for the person. A called operation is named by its registered
/// title ("Read a company"); an operation the registry no longer knows keeps
/// its key. `apps` maps each of the person's connection ids to the app name
/// and whether it was revoked.
fn activity(
    row: &Summary,
    registry: &bilibi_agent_api::Registry,
    apps: &HashMap<String, (String, bool)>,
    now: SystemTime,
) -> Option<ActivityView> {
    let operation = row.operation.as_str();
    let called = |key: &str| match registry.operations.get(key) {
        Some(registered) => registered.definition.title.to_owned(),
        None => format!("Ran {key}"),
    };
    let what = match operation {
        "mcp.search" | "/api/v1/search" => "Looked up what it can do here".to_owned(),
        "mcp.describe" | "mcp.guide" => "Read how an action works".to_owned(),
        "mcp.read" => "Read records".to_owned(),
        "mcp.plan" | "agent_api.plan" => "Prepared a change for you to review".to_owned(),
        "mcp.commit" | "agent_api.commit" => "Saved a change".to_owned(),
        "mcp.resources/read" => "Opened an app card".to_owned(),
        other if other.starts_with("mcp.") => return None,
        other if other.starts_with("/api/v1/operations/") => {
            if other.ends_with("/call") {
                called(
                    other
                        .trim_start_matches("/api/v1/operations/")
                        .trim_end_matches("/call"),
                )
            } else {
                "Read how an action works".to_owned()
            }
        }
        other if other.starts_with("/api/v1/guides/") => "Read how an action works".to_owned(),
        other => called(other),
    };
    let outcome = match row.response_status {
        Some(401) => "Sign-in needed",
        Some(403) => "Not allowed",
        Some(404) => "Not found",
        Some(429) => "Too many requests",
        Some(400..=499) => "Refused",
        Some(500..) => "Failed",
        _ => "",
    };
    let app = match row.connection_id.as_ref().and_then(|id| apps.get(id)) {
        Some((name, true)) => format!("{name} (disconnected)"),
        Some((name, false)) => name.clone(),
        None => String::new(),
    };
    Some(ActivityView {
        id: row.id.clone(),
        when: ago(row.occurred_at, now),
        what,
        app,
        outcome,
    })
}
