//! Module-owned page adapters. Web establishes access before calling an adapter.
//! Framing is presentation only: it never grants a capability or an identity.
pub use crate::upload::Uploads;
use std::{collections::HashMap, future::Future, pin::Pin, sync::Arc};

pub type PageFuture<'a, Error> = Pin<Box<dyn Future<Output = Result<PageBody, Error>> + Send + 'a>>;

pub struct PageRequest<'a, Context> {
    pub operation: &'static str,
    pub scope: Option<&'a Context>,
    pub path: &'a HashMap<String, String>,
    pub query: &'a HashMap<String, String>,
    pub body: &'a [u8],
    pub framed: bool,
}

/// HTML must be rendered/escaped by the owner. Enable live only with a mount adapter.
pub struct PageBody {
    pub html: String,
    pub live: bool,
    /// The document title when it names the record shown (a role's name, a
    /// query's name); `None` defers to `Page::title` for the operation. The
    /// host appends the product suffix, so a title never carries it.
    pub title: Option<String>,
}
pub trait Page<Context, Error>: Send + Sync {
    fn render<'a>(&'a self, request: PageRequest<'a, Context>) -> PageFuture<'a, Error>;
    /// The document title for a discovered page, when its owner supplies one:
    /// the page name alone (`"Roles"`), as Bilimbi's `page_title`; the host adds
    /// " · Business application platform" and falls back to the product name.
    fn title(&self, _operation: &str) -> Option<&'static str> {
        None
    }
    /// Controls the shell shows in its top bar, left of the bell, while this
    /// page is open (Bilimbi's `topbar_actions` slot). Buttons raise events to
    /// the page with `data-event`; the shell omits them from a framed render.
    fn topbar_actions(&self, _operation: &str) -> Option<crate::Html> {
        None
    }
    /// An owner may finish an authorized HTTP action with a 303 redirect.
    /// The host calls this only after `render` succeeds; live updates use LiveBody.redirect.
    fn redirect(&self, _operation: &str) -> Option<&'static str> {
        None
    }

    /// Each mount creates independent state, including for two instances of a panel.
    fn mount<'a>(&'a self, _request: PageRequest<'a, Context>) -> MountFuture<'a, Context, Error> {
        Box::pin(async { Ok(None) })
    }
}
/// Registry service type (Web uses Tenancy Scope and Database Error): contribute its service ID in Route.service.
pub struct PageService<Context, Error>(pub Arc<dyn Page<Context, Error>>);

pub type MountFuture<'a, Context, Error> = Pin<
    Box<dyn Future<Output = Result<Option<Box<dyn LiveState<Context, Error>>>, Error>> + Send + 'a>,
>;
pub type LiveFuture<'a, Error> = Pin<Box<dyn Future<Output = Result<LiveBody, Error>> + Send + 'a>>;

/// Only the mounted owner decodes its events. The host never interprets business payloads.
pub enum LiveInput<'a> {
    Refresh,
    Params(&'a HashMap<String, String>),
    Event(&'a serde_json::Value),
}
pub struct LiveRequest<'a, Context> {
    pub scope: &'a Context,
    pub credential: &'a str,
    pub input: LiveInput<'a>,
}
#[derive(Default)]
pub struct LiveBody {
    pub html: String,
    /// Canonical query values; the host supplies the discovered route's path.
    pub query: HashMap<String, String>,
    /// The route path the page now answers at, when it moved between routes
    /// its own service and operation serve (the workspace opening a saved
    /// layout at `/workspace/{slug}`). The host resolves and guards it and
    /// refuses any other page's route; `None` keeps the mounted path.
    pub path: Option<String>,
    pub notice: String,
    pub rows: usize,
    /// A full browser navigation instead of installing `html`, for a page that
    /// hands the person to an external callback (an OAuth client's redirect).
    pub redirect: Option<String>,
    /// The message the page `redirect` lands on shows (Bilimbi's `put_flash` +
    /// `push_navigate`). The host holds it for the visitor's next navigation to
    /// that path; set it only with a local `redirect` and word it as the source
    /// does (`Some((StatusKind::Success, "Company created successfully.".into()))`).
    pub flash: Option<(crate::StatusKind, String)>,
    /// Pushes to browser hooks, dispatched after the patch as `window` events
    /// `bilibi:<name>` with `detail` (Bilimbi's `push_event`). Names are fixed
    /// by the owner; a hook must ignore detail not addressed to its element.
    pub events: Vec<(&'static str, serde_json::Value)>,
}
/// Server-owned child mounts. Client events cannot pick an arbitrary embed/service.
/// `id` must name the HTML element containing this panel's complete fragment.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PanelSpec {
    pub id: String,
    pub embed: String,
    pub path: HashMap<String, String>,
    pub query: HashMap<String, String>,
}
/// Broadcast after commit. None means tenant-wide; Some(user) means recipient-only.
#[derive(Clone, Debug)]
pub struct Change {
    pub tenant: i64,
    pub user: Option<i64>,
}
pub struct Subscription {
    pub receiver: tokio::sync::broadcast::Receiver<Change>,
}
/// State and subscriptions are dropped on navigation/disconnect. Every call receives
/// freshly guarded scope. Background refresh never records session activity.
pub trait LiveState<Context, Error>: Send {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Context>) -> LiveFuture<'a, Error>;
    /// A page panel may own a stable, non-overlapping query namespace ending
    /// in `_`. Web restores it through Params and merges only its prefixed
    /// LiveBody.query keys into the parent address. Empty keeps local state.
    /// Shell panels never own the page's query.
    fn query_prefix(&self) -> &str {
        ""
    }
    fn panels(&self) -> Vec<PanelSpec> {
        Vec::new()
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        Vec::new()
    }
    /// The file inputs this state accepts (Bilimbi `allow_upload`): the
    /// [`Uploads`] it declared at mount, which the host fills from the
    /// browser's frames and the owner consumes in the event that acts on the
    /// files. `None` refuses every upload aimed at this state.
    fn uploads(&mut self) -> Option<&mut Uploads> {
        None
    }
}
