//! `/system/sessions` (Bilimbi `Session.Web.IndexLive`) and the dashboard
//! stats panel (`Session.Web.DashboardStatsPanel`).
//!
//! The list is platform-global and payload-free. Each row names the signed-in
//! user through the PrincipalDirectory seam: a user in the actor's tenant shows
//! their name, one outside it keeps `User <id>` rather than leaking a name
//! across the boundary. Sorting the user column sorts `user_id`, not names.
//!
//! Terminating confirms through the shared dialog: the request holds the
//! listed session whose consequence the dialog states, and `terminate` acts on
//! that held session, never on a client-supplied id. Rows are keyed by a
//! digest of the session id, so the bearer token never reaches the operator's
//! DOM. The caller's own session is refused before any dialog opens; its
//! control is never rendered, so only a forged event gets there. Every command
//! re-asks Authz (`authorize_operator`, logged) before it acts; a refusal is a
//! flash, never a page-clearing error.
use crate::{
    contributions::PATH, ListQuery, Sessions, SortBy, Summary, Termination, LIST, MANAGE,
    PAGE_SIZES,
};
use askama::Template;
use bilibi_authz::{authorize_operator, effective_capabilities};
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_principal_directory::{PrincipalDirectory, PrincipalKind, PrincipalRef, Query};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::{InvalidPageSize, ParamNames, FILTERS_FORM},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Card, ColumnType, ColumnsOutcome, ConfirmDialog,
    FilterToolbar, FlashMap, FlexRow, FlexTable, Header, Html, IconButton, IconKind, ListColumns,
    ListSpec, ListState, PageCounts, Pagination, SortDir, StatItem, StatStrip, StatusKind,
    TableEmpty, ToolbarControl, NO_FLASH,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const DENIED: &str = "You do not have permission to terminate sessions.";
const CURRENT: &str = "You cannot terminate your current session.";
const USER_AGENT_SHOWN: usize = 80;

/// Bilimbi's query names (`search`, `sort`, `dir`, `page`, `per_page`), with
/// defaults left out of the URL as its `sessions_path/1` does.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("user_id".into(), SortDir::Asc),
            ("ip_address".into(), SortDir::Asc),
            ("user_agent".into(), SortDir::Asc),
            ("last_activity".into(), SortDir::Desc),
        ]),
        default_sort: Some("last_activity".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "per_page".into(),
        page_size_aliases: vec!["perPage".into(), "page_size".into()],
        invalid_page_size: InvalidPageSize::Default,
        param_names: ParamNames {
            search: "search".into(),
            page: "page".into(),
            sort_by: "sort".into(),
            sort_dir: "dir".into(),
        },
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("session list spec")
});

/// The list's own columns (Bilimbi `@builtins`): the sortable ones keep the
/// source's sort keys and button ids.
fn builtins() -> Vec<BuiltinColumn> {
    let sortable = |id: &str, label: &str, kind| {
        let mut column = BuiltinColumn::new(id, label).sortable(id);
        column.kind = kind;
        column.sort_id = Some(format!("sessions-sort-{id}"));
        column
    };
    vec![
        sortable("user_id", "User", ColumnType::String),
        sortable("ip_address", "IP Address", ColumnType::String),
        sortable("user_agent", "User Agent", ColumnType::String),
        sortable("last_activity", "Last Activity", ColumnType::Datetime),
        BuiltinColumn::new("status", "Status"),
    ]
}

struct Core {
    db: Db,
    sessions: Arc<Sessions>,
    mutations: Arc<MutationBoundary>,
    directory: Arc<PrincipalDirectory>,
}
pub(crate) struct Pages(Arc<Core>);
impl Pages {
    pub fn new(
        db: Db,
        sessions: Arc<Sessions>,
        mutations: Arc<MutationBoundary>,
        directory: Arc<PrincipalDirectory>,
    ) -> Self {
        Self(Arc::new(Core {
            db,
            sessions,
            mutations,
            directory,
        }))
    }
}

#[derive(Template)]
#[template(path = "sessions.html")]
struct IndexView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}

struct Index {
    core: Arc<Core>,
    state: ListState,
    /// The column arrangement and row height, kept while mounted.
    columns: ListColumns,
    /// Whether this render reads the rows anew, so the columns forget an
    /// unfinished add box (Bilimbi `ListColumns.load/3` on `load_page`).
    rows_changed: bool,
    /// The rows last shown, by key: a termination request names one of these.
    rows: Vec<Summary>,
    pending: Option<Summary>,
    flash: Option<(StatusKind, String)>,
}
impl Index {
    fn new(core: Arc<Core>, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            core,
            state: parse(query)?,
            columns: ListColumns::mount("sessions", builtins()).map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            pending: None,
            flash: None,
        })
    }
}
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn sort_by(state: &ListState) -> SortBy {
    match state.sort_by.as_deref() {
        Some("user_id") => SortBy::UserId,
        Some("ip_address") => SortBy::IpAddress,
        Some("user_agent") => SortBy::UserAgent,
        _ => SortBy::LastActivity,
    }
}
/// Bilimbi's `relative_activity/1`, pluralised per unit as Belimbing's
/// `diffForHumans()` is: a session last seen 30 hours ago reads "1 day ago".
fn relative_activity(seconds: i64) -> String {
    let delta = seconds.max(0);
    let ago = |count: i64, unit: &str| {
        if count == 1 {
            format!("1 {unit} ago")
        } else {
            format!("{count} {unit}s ago")
        }
    };
    if delta < 60 {
        "just now".into()
    } else if delta < 3_600 {
        ago(delta / 60, "minute")
    } else if delta < 86_400 {
        ago(delta / 3_600, "hour")
    } else {
        ago(delta / 86_400, "day")
    }
}
fn truncate_agent(agent: &str) -> String {
    if agent.chars().count() > USER_AGENT_SHOWN {
        let shown: String = agent.chars().take(USER_AGENT_SHOWN).collect();
        format!("{shown}...")
    } else {
        agent.to_owned()
    }
}
/// The dialog names the session the way its row does, so the operator can
/// match the two: who holds it and from where.
fn session_name(session: &Summary, names: &HashMap<i64, String>) -> String {
    format!(
        "The {} session from {}",
        user_label(session, names),
        session
            .ip_address
            .as_deref()
            .unwrap_or("an unknown address")
    )
}
fn user_label(session: &Summary, names: &HashMap<i64, String>) -> String {
    names
        .get(&session.user_id)
        .cloned()
        .unwrap_or_else(|| format!("User {}", session.user_id))
}

impl Index {
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    /// The logged operator decision for one command. A refusal is a flash.
    async fn allowed(&mut self, scope: &Scope) -> Result<bool> {
        let client = self.core.db.get().await?;
        match authorize_operator(&**client, scope, MANAGE).await {
            Ok(_) => Ok(true),
            Err(Error::Forbidden) => {
                self.flash(StatusKind::Error, DENIED);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self.state.apply_filters(
                &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ),
            "sort" => {
                if !self.state.next_sort(get("sort").unwrap_or_default()) {
                    return Err(Error::Invalid);
                }
            }
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(column) => {
                    if !self.state.next_sort(&column) {
                        return Err(Error::Invalid);
                    }
                }
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| Some(kind.name()) == get("key"))
                {
                    self.flash = None;
                }
            }
            "request_terminate" => {
                let key = get("key").unwrap_or_default().to_owned();
                if !self.allowed(scope).await? {
                    return Ok(());
                }
                let client = self.core.db.get().await?;
                let current = self.core.sessions.identify(&**client, scope).await?;
                let session = self.rows.iter().find(|row| row.key() == key).cloned();
                match session {
                    Some(session) if session.id == current => {
                        self.flash(StatusKind::Error, CURRENT)
                    }
                    Some(session) => {
                        self.flash = None;
                        self.pending = Some(session);
                    }
                    // Gone since the list was drawn: the redraw shows that.
                    None => {}
                }
            }
            "cancel_terminate" => self.pending = None,
            "terminate" => {
                // The refusal comes first, so a revoked operator hears it even
                // from a forged event.
                if !self.allowed(scope).await? {
                    return Ok(());
                }
                let Some(session) = self.pending.take() else {
                    return Ok(());
                };
                let mut client = self.core.db.get().await?;
                let mut tx = self
                    .core
                    .mutations
                    .begin(&mut client, scope.mutation_context("session.terminate"))
                    .await?;
                let current = self.core.sessions.identify(tx.sql(), scope).await?;
                let outcome = self
                    .core
                    .sessions
                    .terminate(&mut tx, &session.id, &current)
                    .await;
                match outcome {
                    Ok(Termination::Terminated) => {
                        tx.commit().await?;
                        self.flash(StatusKind::Success, "Session terminated.");
                    }
                    Ok(Termination::NotFound) => {
                        tx.commit().await?;
                        self.flash(StatusKind::Info, "That session had already ended.");
                    }
                    Ok(Termination::CurrentSession) => self.flash(StatusKind::Error, CURRENT),
                    Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                    Err(_) => self.flash(
                        StatusKind::Error,
                        "The session store is unavailable; nothing was terminated.",
                    ),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let core = self.core.clone();
        let client = core.db.get().await?;
        let current = core.sessions.identify(&**client, scope).await?;
        let can_manage = effective_capabilities(&**client, scope)
            .await?
            .contains(MANAGE);
        let mut query = ListQuery {
            search: Some(self.state.search.clone()),
            page: self.state.page,
            page_size: self.state.page_size,
            sort_by: sort_by(&self.state),
            descending: self.state.sort_dir == Some(SortDir::Desc),
        };
        let mut page = core.sessions.list(&**client, &query).await?;
        let mut counts = PageCounts::new(query.page, query.page_size, page.total_entries);
        if self.state.clamp_to_last_page(counts.total_pages, true) {
            query.page = self.state.page;
            page = core.sessions.list(&**client, &query).await?;
            counts = PageCounts::new(query.page, query.page_size, page.total_entries);
        }
        let names = self.names(&client, scope, &page.entries).await?;
        self.rows = page.entries.clone();
        if self.pending.as_ref().is_some_and(|held| held.id == current) {
            self.pending = None;
        }

        let header = render(Header {
            title: Html::text("Sessions"),
            subtitle: "See who is signed in and sign them out.",
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "sessions-filters",
            vec![ToolbarControl::search(
                "sessions-search",
                "search",
                "Search sessions",
                &form["search"],
                "Search by IP address or user agent...",
            )],
        ))?;
        if self.rows_changed {
            self.columns.load(&page.entries, Summary::key);
        }
        let mut rows = Vec::with_capacity(page.entries.len());
        for session in &page.entries {
            let key = session.key();
            let is_current = session.id == current;
            let agent = session.user_agent.as_deref().unwrap_or("—");
            let status = if is_current {
                render(Badge::status(BadgeKind::Success, "Current"))?
            } else {
                Html::EMPTY
            };
            let actions = if can_manage && !is_current {
                let id = format!("sessions-terminate-{key}");
                render(IconButton {
                    id: &id,
                    kind: IconKind::Danger,
                    event: "request_terminate",
                    name: "key",
                    value: &key,
                    ..IconButton::new("x-circle", "Terminate session")
                })?
            } else {
                Html::EMPTY
            };
            let mut row = FlexRow::new(key.clone())
                .slot("user_id", Html::text(&user_label(session, &names)))
                .slot(
                    "ip_address",
                    Html::raw(format!(
                        "<span class=\"tabular-nums text-ink-muted\">{}</span>",
                        Html::text(session.ip_address.as_deref().unwrap_or("—"))
                    )),
                )
                .slot(
                    "user_agent",
                    Html::raw(format!(
                        "<span class=\"max-w-xs truncate text-ink-muted\" title=\"{}\">{}</span>",
                        Html::text(agent),
                        Html::text(&truncate_agent(agent))
                    )),
                )
                .slot(
                    "last_activity",
                    Html::raw(format!(
                        "<span class=\"text-ink-muted\">{}</span>",
                        Html::text(&relative_activity(session.idle_seconds))
                    )),
                )
                .slot("status", status);
            row.dom_id = Some(format!("sessions-{key}"));
            row.actions = actions;
            rows.push(row);
        }
        let mut table = FlexTable::new("sessions", self.columns.flex_columns(), rows)
            .sorted(self.state.sort_by.as_deref(), self.state.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Sessions";
        table.framed = false;
        table.actions = can_manage;
        if page.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text("No sessions found.")));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "sessions-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "sessions-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.pending {
            Some(session) => {
                let consequence = format!("{} will be terminated.", session_name(session, &names));
                render(ConfirmDialog {
                    id: "terminate-session-confirm",
                    consequence: &consequence,
                    detail: "Whoever holds it is signed out on that device at once and must sign in again. This cannot be undone.",
                    confirm: "Terminate",
                    working: "Terminating…",
                    confirm_event: "terminate",
                    cancel_event: "cancel_terminate",
                })?
            }
            None => Html::EMPTY,
        };
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        IndexView {
            flash,
            header,
            toolbar,
            card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// Names through the directory, which owns provider resolution and the
    /// candidate ceiling: a page is at most 300 rows, far below it, so an
    /// error here is a defect, not a state to recover from.
    async fn names(
        &self,
        client: &tokio_postgres::Client,
        scope: &Scope,
        rows: &[Summary],
    ) -> Result<HashMap<i64, String>> {
        let mut candidates: Vec<PrincipalRef> = rows
            .iter()
            .map(|row| PrincipalRef::new(PrincipalKind::User, row.user_id))
            .collect::<std::result::Result<_, _>>()
            .map_err(|_| Error::Internal)?;
        candidates.sort_by_key(|principal| principal.id());
        candidates.dedup();
        if candidates.is_empty() {
            return Ok(HashMap::new());
        }
        let ranked = self
            .core
            .directory
            .rank(client, scope, &candidates, &Query::default())
            .await
            .map_err(|_| Error::Internal)?;
        Ok(ranked
            .named
            .into_iter()
            .map(|named| (named.principal.id(), named.name))
            .collect())
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            // Every input reads the rows again except a column operation
            // that only changes what is on screen, which `event` says.
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.rows.len(),
                ..LiveBody::default()
            })
        })
    }
}

/// The dashboard widget: how many durable sessions exist right now. The count
/// is platform-wide, read again when the dashboard counts a refresh and only
/// while `admin.system.session.list` is effective; otherwise an em dash and no
/// query. Links to the list unless the account is customizing the layout.
struct Panel {
    core: Arc<Core>,
    args: bilibi_dashboard::PanelArgs,
    cache: bilibi_dashboard::PanelCache,
}
impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if !matches!(request.input, LiveInput::Refresh) {
                return Err(Error::Invalid);
            }
            if let Some(html) = self.cache.fresh(&self.args) {
                return Ok(LiveBody {
                    html: html.to_owned(),
                    ..LiveBody::default()
                });
            }
            let client = self.core.db.get().await?;
            let allowed = effective_capabilities(&**client, request.scope)
                .await?
                .contains(LIST);
            let count = if allowed {
                self.core.sessions.count(&**client).await?.to_string()
            } else {
                "—".into()
            };
            let id = self.args.id();
            let html = StatStrip {
                id: &id,
                title: "Sessions",
                href: if allowed && !self.args.editing {
                    PATH
                } else {
                    ""
                },
                items: vec![
                    StatItem::number("Open", count),
                    StatItem::text("Store", "Durable"),
                    StatItem::text("Scope", "Platform"),
                ],
            }
            .render()
            .map_err(|_| Error::Internal)?;
            Ok(LiveBody {
                html: self.cache.store(html),
                ..LiveBody::default()
            })
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Sessions")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut index = Index::new(self.0.clone(), request.query)?;
            let html = index.render(scope).await?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "index" => Box::new(Index::new(self.0.clone(), request.query)?),
                "dashboard-stats" => Box::new(Panel {
                    core: self.0.clone(),
                    args: bilibi_dashboard::PanelArgs::from_request(&request)?,
                    cache: bilibi_dashboard::PanelCache::default(),
                }),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}
