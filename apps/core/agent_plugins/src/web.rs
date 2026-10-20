//! The MCP settings page and the ChatGPT plugin download (Bilimbi
//! `web/plugin_live.ex` and `web/plugin_controller.ex`). The page is one tab
//! for any MCP app. The download operation names the harness. Opening the
//! page, choosing an app or downloading grants no access. `AgentPlugins`
//! keeps the self-service gate and re-checks the client at download.
use crate::{package, AgentPlugins, PluginError};
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
    Choice, EmptyState, Field, Html, InputKind,
};
use serde::Deserialize;
use std::{collections::HashMap, sync::Arc};

pub const PAGES: &str = "core/agent_plugins.pages";
pub const FILES: &str = "core/agent_plugins.files";
/// The settings page operation. A download operation is a harness key instead.
const PAGE: &str = "mcp";
/// `docs/mcp-setup.md` on the default branch. The page links here so ChatGPT
/// steps stay out of the tab.
const SETUP_GUIDES: &str = "https://github.com/BelimbingApp/bilibi/blob/main/docs/mcp-setup.md";
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
fn page(operation: &str) -> Result<()> {
    if operation == PAGE {
        Ok(())
    } else {
        Err(Error::NotFound)
    }
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
        (operation == PAGE).then_some("My Settings")
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
                    &archive.filename,
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
#[template(
    source = r#"<ul class="mt-1 list-disc space-y-0.5 pl-4"><li>Use any AI app that supports MCP. Add the server URL from this page in its MCP settings.</li><li>Sign in to Bilibi when the app asks, and approve the connection.</li><li>The agent can do what you can do in Bilibi, nothing more.</li><li>End access any time in Agent connections.</li></ul>"#,
    ext = "html"
)]
struct Hints;

#[derive(Template)]
#[template(path = "plugin.html")]
struct PluginView {
    header: Html,
    hints: Html,
    person_required: bool,
    no_clients: Html,
    client_field: Option<Field>,
    server: CopyValue,
    client: Option<CopyValue>,
    authorization_url: String,
    token_url: String,
    scope: &'static str,
    copied: Option<bool>,
    guides_href: &'static str,
}

impl PluginPage {
    fn new(pages: Pages, operation: &str, query: &HashMap<String, String>) -> Result<Self> {
        page(operation)?;
        Ok(Self {
            pages,
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
        let render = |rendered: askama::Result<Html>| rendered.map_err(|_| Error::Internal);
        let hints = render(Html::render(&Hints))?;
        let header = render(Html::render(&bilibi_ui::Header {
            title: Html::text("My Settings"),
            ..bilibi_ui::Header::default()
        }))?;
        let origin = self.pages.plugins.origin();
        let clients = self.clients(scope).await?;
        let person_required = clients.is_none();
        let clients = clients.unwrap_or_default();
        let selected = self.selected(&clients);
        let no_clients = if clients.is_empty() {
            render(Html::render(
                &EmptyState::missing(
                    "No app registered",
                    "Ask an admin to register the app under OAuth public clients in Settings, using the callback the app shows.",
                )
                .id("mcp-no-clients"),
            ))?
        } else {
            Html::EMPTY
        };
        let client_field = (!clients.is_empty()).then(|| {
            Field::named("client_id")
                .id("mcp-app")
                .kind(InputKind::Select)
                .label("App")
                .prompt("Choose an app")
                .options(
                    clients
                        .iter()
                        .map(|client| Choice::new(&client.name, &client.id))
                        .collect(),
                )
                .value(selected.map_or("", |client| client.id.as_str()))
        });
        html(PluginView {
            header,
            hints,
            person_required,
            no_clients,
            client_field,
            server: CopyValue {
                id: "mcp-copy-server",
                text: package::server_url(origin),
            },
            client: selected.map(|client| CopyValue {
                id: "mcp-copy-client",
                text: client.id.clone(),
            }),
            authorization_url: format!("{origin}/oauth/authorize"),
            token_url: format!("{origin}/oauth/token"),
            scope: bilibi_agent_access::OAUTH_SCOPE,
            copied: self.copied.map(|copied| copied == Copied::Yes),
            guides_href: SETUP_GUIDES,
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
