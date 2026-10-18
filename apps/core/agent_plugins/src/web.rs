//! The setup page and its download (Bilimbi `web/plugin_live.ex` and
//! `web/plugin_controller.ex`), one adapter for every harness: the operation
//! names the harness, and the harness supplies its name and copy. Opening the
//! page, choosing a client or downloading grants no access. `AgentPlugins`
//! keeps the self-service gate and re-checks the client at download.
use crate::{harness::PageCopy, package, AgentPlugins, Harness, PluginError};
use askama::Template;
use bilibi_agent_access::PublicClient;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    download::{Download, DownloadFuture, DownloadRequest, FileBody},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    Button, ButtonVariant, Choice, EmptyState, Field, Html, InputKind,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

pub const PAGES: &str = "core/agent_plugins.pages";
pub const FILES: &str = "core/agent_plugins.files";
/// Client IDs are bounded by the setting's validator.
const CLIENT_ID_LIMIT: usize = 255;

#[derive(Clone)]
pub struct Pages {
    pub plugins: Arc<AgentPlugins>,
}

fn html<T: Template>(template: T) -> Result<String> {
    template.render().map_err(|_| Error::Internal)
}
/// Only `client_id`, bounded; anything else is a malformed address.
fn requested(query: &HashMap<String, String>) -> Result<Option<String>> {
    if query.keys().any(|key| key != "client_id") {
        return Err(Error::Invalid);
    }
    match query.get("client_id") {
        Some(id) if id.len() > CLIENT_ID_LIMIT => Err(Error::Invalid),
        Some(id) if id.is_empty() => Ok(None),
        id => Ok(id.cloned()),
    }
}
fn harness(operation: &str) -> Result<&'static dyn Harness> {
    AgentPlugins::harness(operation).map_err(|_| Error::NotFound)
}

impl Page<Scope, Error> for Pages {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let page = PluginPage::new(self.clone(), request.operation, request.query)?;
            Ok(PageBody {
                html: page.view(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let page: Box<dyn LiveState<Scope, Error>> = Box::new(PluginPage::new(
                self.clone(),
                request.operation,
                request.query,
            )?);
            Ok(Some(page))
        })
    }
    fn title(&self, operation: &str) -> Option<&'static str> {
        AgentPlugins::harness(operation)
            .ok()
            .map(|harness| harness.title())
    }
}

impl Download<Scope, Error> for Pages {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let client_id = requested(request.query)?.ok_or(Error::NotFound)?;
            match self
                .plugins
                .download(request.scope, request.operation, &client_id)
                .await
            {
                Ok(archive) => Ok(FileBody::attachment(
                    archive.bytes,
                    "application/zip",
                    archive.filename,
                )),
                Err(PluginError::PersonRequired) => Err(Error::Forbidden),
                Err(PluginError::InvalidClient | PluginError::UnknownHarness) => {
                    Err(Error::NotFound)
                }
                Err(PluginError::HttpsRequired) => Err(Error::Conflict),
                Err(PluginError::Other(error)) => Err(error),
            }
        })
    }
}

/// What the browser reported after a copy click.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Copied {
    Yes,
    Failed,
}
struct PluginPage {
    pages: Pages,
    harness: &'static dyn Harness,
    requested: Option<String>,
    copied: Option<Copied>,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum PluginEvent {
    SelectClient {
        #[serde(default)]
        client_id: String,
    },
    ConnectionCopied {
        text: String,
        copied: bool,
    },
}

/// One copyable value: the `ClipboardCopy` hook reads `data-copy-text`.
pub struct CopyValue {
    pub id: &'static str,
    pub text: String,
}
#[derive(Template)]
#[template(path = "plugin.html")]
struct PluginView {
    harness_name: &'static str,
    copy: &'static PageCopy,
    header: Html,
    person_required: bool,
    no_clients: Html,
    client_field: Option<Field>,
    server: CopyValue,
    client: Option<CopyValue>,
    authorization_url: String,
    token_url: String,
    scope: &'static str,
    copied: Option<bool>,
    https_required: Html,
    client_required: Html,
    download: Html,
}

impl PluginPage {
    fn new(pages: Pages, operation: &str, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            harness: harness(operation)?,
            requested: requested(query)?,
            copied: None,
        })
    }
    /// The requested client when registered; the only client when none was
    /// requested (Bilimbi `handle_params`).
    fn selected<'c>(&self, clients: &'c [PublicClient]) -> Option<&'c PublicClient> {
        match &self.requested {
            Some(id) => clients.iter().find(|client| &client.id == id),
            None if clients.len() == 1 => clients.first(),
            None => None,
        }
    }
    async fn clients(&self, scope: &Scope) -> Result<Option<Vec<PublicClient>>> {
        match self.pages.plugins.clients(scope).await {
            Ok(clients) => Ok(Some(clients)),
            Err(PluginError::Other(error)) => Err(error),
            Err(_) => Ok(None),
        }
    }
    async fn view(&self, scope: &Scope) -> Result<String> {
        let harness = self.harness;
        let render = |rendered: askama::Result<Html>| rendered.map_err(|_| Error::Internal);
        let connections = render(Html::render(&Button {
            variant: ButtonVariant::Secondary,
            id: "plugin-connections",
            href: "/settings/agent-connections",
            body: Html::text("Your connections"),
            ..Button::default()
        }))?;
        let subtitle = format!(
            "Install Bilibi in {} to work with records using your own account and permissions.",
            harness.name()
        );
        let header = render(Html::render(&bilibi_ui::Header {
            title: Html::text(harness.title()),
            subtitle: &subtitle,
            title_actions: Html::EMPTY,
            actions: connections,
        }))?;
        let origin = self.pages.plugins.origin();
        let clients = self.clients(scope).await?;
        let person_required = clients.is_none();
        let clients = clients.unwrap_or_default();
        let selected = self.selected(&clients);
        let https = package::https_origin(origin);
        let empty = |show: bool, id: &str, title: &str, reason: &str| -> Result<Html> {
            if !show {
                return Ok(Html::EMPTY);
            }
            render(Html::render(&EmptyState::missing(title, reason).id(id)))
        };
        let no_clients = empty(
            clients.is_empty(),
            "plugin-no-clients",
            "Administrator setup needed",
            &format!("Ask your platform administrator to register {} under OAuth public clients in Settings, using the exact callback shown by {}.", harness.name(), harness.name()),
        )?;
        let https_required = empty(
            !https,
            "plugin-https-required",
            "HTTPS setup needed",
            "Ask your administrator to set up this site's HTTPS address before you download the plugin.",
        )?;
        let client_required = empty(
            https && !clients.is_empty() && selected.is_none(),
            "plugin-client-required",
            "Choose an OAuth client",
            &format!("Select the client your administrator registered for {} to download its plugin configuration.", harness.name()),
        )?;
        let client_field = (!clients.is_empty()).then(|| {
            Field::named("client_id")
                .id("plugin-client")
                .kind(InputKind::Select)
                .label("OAuth client")
                .prompt(format!(
                    "Choose the client registered for {}",
                    harness.name()
                ))
                .options(
                    clients
                        .iter()
                        .map(|client| {
                            Choice::new(format!("{} ({})", client.name, client.id), &client.id)
                        })
                        .collect(),
                )
                .value(selected.map_or("", |client| client.id.as_str()))
        });
        let download_href = selected.map(|client| {
            format!(
                "{}/download?client_id={}",
                harness.path(),
                encode(&client.id)
            )
        });
        let download = match (&download_href, https) {
            (Some(href), true) => render(Html::render(&Button {
                variant: ButtonVariant::Primary,
                id: "plugin-download",
                href,
                class: "mt-4",
                body: Html::text("Download plugin ZIP"),
                ..Button::default()
            }))?,
            _ => Html::EMPTY,
        };
        html(PluginView {
            harness_name: harness.name(),
            copy: harness.copy(),
            header,
            person_required,
            no_clients,
            client_field,
            server: CopyValue {
                id: "plugin-copy-server",
                text: package::server_url(origin),
            },
            client: selected.map(|client| CopyValue {
                id: "plugin-copy-client",
                text: client.id.clone(),
            }),
            authorization_url: format!("{origin}/oauth/authorize"),
            token_url: format!("{origin}/oauth/token"),
            scope: bilibi_agent_access::OAUTH_SCOPE,
            copied: self.copied.map(|copied| copied == Copied::Yes),
            https_required,
            client_required,
            download,
        })
    }
    /// A copy report counts only for a value this page shows (Bilimbi
    /// `connection_copied`); anything else is ignored.
    async fn copied(&mut self, scope: &Scope, text: &str, copied: bool) -> Result<()> {
        let Some(clients) = self.clients(scope).await? else {
            return Ok(());
        };
        let shown = text == package::server_url(self.pages.plugins.origin())
            || self
                .selected(&clients)
                .is_some_and(|client| client.id == text);
        self.copied = shown.then_some(if copied { Copied::Yes } else { Copied::Failed });
        Ok(())
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

impl LiveState<Scope, Error> for PluginPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.requested = requested(values)?;
                    self.copied = None;
                }
                LiveInput::Event(value) => {
                    match serde_json::from_value::<PluginEvent>(value.clone())
                        .map_err(|_| Error::Invalid)?
                    {
                        PluginEvent::SelectClient { client_id } => {
                            if client_id.len() > CLIENT_ID_LIMIT {
                                return Err(Error::Invalid);
                            }
                            self.requested = (!client_id.is_empty()).then_some(client_id);
                            self.copied = None;
                        }
                        PluginEvent::ConnectionCopied { text, copied } => {
                            self.copied(request.scope, &text, copied).await?
                        }
                    }
                }
            }
            let mut query = HashMap::new();
            if let Some(id) = &self.requested {
                query.insert("client_id".into(), id.clone());
            }
            Ok(LiveBody {
                html: self.view(request.scope).await?,
                query,
                ..LiveBody::default()
            })
        })
    }
}
