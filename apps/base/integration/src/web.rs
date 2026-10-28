//! The `base/integration.pages` adapter: `/system/api-exchanges` (operation
//! `exchanges`) and `/system/api-exchanges/{id}` (`exchange`), ported from
//! Bilimbi `apps/base/integration/lib/integration/web/{index,show}_live.ex`.
//!
//! The list is the shared flexible table over the built-in columns hosted by
//! Grid `PageColumns` (the arrangement and row height are remembered per
//! account and page, and an address that carries them wins), bounded URL state
//! (`ListState`), the direction tabs, the outcome select, one search and the
//! period filter, whose calendar days follow the actor's display mode and, in
//! local mode, the zone the `BrowserTimeZone` hook reports. No Grid table is
//! declared for exchanges, so the add box offers only removed built-ins, as
//! Bilimbi's does. The route guard checks the list capability on every request
//! and live event; the operator scope sees every tenant and the system rows,
//! with the same caution Bilimbi shows.
//!
//! The detail page shows the metadata, inspects the retained payload only on
//! an explicit click behind its own capability, and deletes through a confirm
//! dialog. A refusal is a flash, never a page-clearing error.
use crate::{Direction, Filter, Integration, Outcome, Payload, Summary, DELETE, PAYLOAD};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{
    filter::{self as period, PeriodFilter},
    Clock, DisplayContext, DisplayPolicy, Format, Period, Precision,
};
use bilibi_grid::{Outcome as GridOutcome, PageColumns, PageViews};
use bilibi_locale::Locales;
use bilibi_tenancy::{ClientIdentity, Scope, SelfReport};
use bilibi_ui::{
    choices,
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BuiltinColumn, ColumnType, ConfirmDialog, EmptyState, Facts, FilterToolbar, FlashMap,
    FlexRow, Html, Instant, ListSpec, ListState, PageCounts, Pagination, RowMode, StatusKind, Tab,
    TableEmpty, Tabs, ToolbarControl, NO_FLASH,
};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
    time::{SystemTime, UNIX_EPOCH},
};

pub const INDEX_PATH: &str = "/system/api-exchanges";
const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
const OUTCOMES: [&str; 5] = ["", "success", "http_error", "connection_error", "unknown"];
const DIRECTIONS: [(&str, &str); 2] = [("outgoing", "Outgoing"), ("incoming", "Incoming")];
/// The period's URL keys, kept beside the list's own so filtering preserves
/// the range and the range preserves the filters.
const PERIOD_KEYS: [&str; 3] = ["period", "from", "to"];

/// Bilimbi's spec: no sortable column (the log is newest first), page sizes
/// 25/50/100/300, `direction` and `status` filters with the source's names.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        filters: BTreeMap::from([
            (
                "direction".into(),
                bilibi_ui::Filter::one_of(&["incoming", "outgoing"], "outgoing"),
            ),
            ("status".into(), bilibi_ui::Filter::one_of(&OUTCOMES, "")),
        ]),
        ..ListSpec::default()
    }
    .shared()
    .expect("exchange list spec")
});

/// Bilimbi's built-in columns, in its order and with its labels and types.
fn builtins() -> Vec<BuiltinColumn> {
    let typed = |id: &str, label: &str, kind: ColumnType| {
        let mut column = BuiltinColumn::new(id, label);
        column.kind = kind;
        column
    };
    vec![
        typed("occurred_at", "Time", ColumnType::Datetime),
        BuiltinColumn::new("operation", "Operation"),
        BuiltinColumn::new("method", "Method"),
        BuiltinColumn::new("host", "Host / path"),
        typed("status", "Status", ColumnType::Integer),
        typed("duration_ms", "Duration (ms)", ColumnType::Integer),
        BuiltinColumn::new("actor", "Person / agent"),
        BuiltinColumn::new("self_report", SelfReport::LABEL),
        BuiltinColumn::new("client_identity", ClientIdentity::LABEL),
    ]
}

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub integration: Arc<Integration>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub views: Arc<PageViews>,
}

impl Pages {
    async fn context(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
    ) -> Result<DisplayContext> {
        let locale = self.locales.resolve(client, scope, None).await?.locale;
        self.display.display(client, scope, None, locale).await
    }
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

fn parse(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}

/// The period keys as the URL carries them, bounded; `Period::parse` decides
/// what they mean against the request clock.
fn period_params(values: &HashMap<String, String>) -> HashMap<String, String> {
    PERIOD_KEYS
        .iter()
        .filter_map(|key| {
            let value = values.get(*key)?;
            (value.len() <= 32).then(|| ((*key).to_owned(), value.clone()))
        })
        .collect()
}

fn query_string(pairs: &BTreeMap<String, String>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    for (key, value) in pairs {
        query.append_pair(key, value);
    }
    query.finish()
}

fn instant(display: &DisplayContext, at: SystemTime) -> Result<Instant> {
    let at = Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Ok(Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    ))
}

/// The agent's own claim, or that the claim it sent was malformed and dropped.
fn self_report(exchange: &Summary) -> Html {
    if exchange.agent_self_report_malformed {
        return Html::text("Malformed, dropped");
    }
    dash(exchange.agent_self_report.as_deref())
}
fn dash(value: Option<&str>) -> Html {
    Html::text(value.unwrap_or("—"))
}

fn reason(error: &Error) -> &'static str {
    match error {
        Error::Forbidden => "forbidden",
        Error::NotFound => "not_found",
        Error::Invalid => "invalid",
        _ => "unavailable",
    }
}

/// Bilimbi's `<.datetime>` tag through the shared instant markup.
#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::instant(id=id, i=i) %}{% endcall %}"#,
    ext = "html"
)]
struct TimeTag<'a> {
    id: &'a str,
    i: Instant,
}

#[derive(Template)]
#[template(source = r#"<a href="{{ href }}">{{ text }}</a>"#, ext = "html")]
struct OperationLink<'a> {
    href: String,
    text: &'a str,
}

/// Host (or the endpoint when there is none) over the path in normal rows.
#[derive(Template)]
#[template(
    source = r#"{{ host }}{% if let Some(path) = path %}<span class="block text-xs text-muted">{{ path }}</span>{% endif %}"#,
    ext = "html"
)]
struct HostCell<'a> {
    host: &'a str,
    path: Option<&'a str>,
}

#[derive(Template)]
#[template(path = "exchanges.html")]
struct IndexView {
    tabs: Html,
    toolbar: Html,
    period: Html,
    range_error: Option<&'static str>,
    operator: bool,
    table: Html,
    pagination: Html,
}

#[derive(Template)]
#[template(path = "exchange.html")]
struct ShowView<'a> {
    flash: FlashMap<'a>,
    exchange_id: &'a str,
    all_link: Html,
    facts: Html,
    can_payload: bool,
    can_delete: bool,
    payload: Option<&'a str>,
    payload_restricted: EmptyState,
    delete_restricted: EmptyState,
    dialog: Html,
}

/// The mounted list.
struct Index {
    pages: Pages,
    list: ListState,
    period_params: HashMap<String, String>,
    /// What `Period::parse` resolved at the last render, for the URL.
    period: Option<Period>,
    filter: PeriodFilter,
    browser_zone: Option<String>,
    /// The column arrangement, remembered per account (Bilimbi `PageColumns`).
    columns: PageColumns,
    /// The rows changed since the column picker last saw them.
    reload: bool,
}

impl Index {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        let period_params = period_params(query);
        // The real clock needs the actor's display policy, read at render;
        // until then the filter carries the UTC reading of the same keys.
        let clock = DisplayContext::anonymous().clock(None, Timestamp::now());
        let period = Period::parse(|key| period_params.get(key).cloned(), clock.today);
        let views = pages.views.clone();
        Ok(Self {
            pages,
            list: parse(query)?,
            period_params,
            period: None,
            filter: PeriodFilter::new("exchange-period", period, &clock),
            browser_zone: None,
            columns: PageColumns::mount(views, "integration-exchanges", builtins())
                .and_then(|columns| columns.with_record_slots(fields::ALL))
                .map_err(|_| Error::Internal)?,
            reload: true,
        })
    }

    /// The address the page opened with: the list state and, through
    /// `PageColumns`, the arrangement it carries or the account's memory.
    async fn open(&mut self, scope: &Scope, query: &HashMap<String, String>) -> Result<()> {
        self.columns.from_params(scope, query).await
    }

    async fn clock(&self, scope: &Scope) -> Result<Clock> {
        let client = self.pages.db.get().await?;
        let display = self.pages.context(&**client, scope).await?;
        Ok(display.clock(self.browser_zone.as_deref(), Timestamp::now()))
    }

    fn query(&self) -> HashMap<String, String> {
        let mut query = self.list.to_params();
        query.extend(self.columns.params());
        if let Some(period) = &self.period {
            query.extend(
                period
                    .params()
                    .into_iter()
                    .map(|(key, value)| (key.to_owned(), value)),
            );
        }
        query
    }

    fn tab_href(&self, direction: &str) -> String {
        let mut pairs: BTreeMap<String, String> = self.query().into_iter().collect();
        pairs.insert("direction".into(), direction.into());
        pairs.insert(self.list.spec().param_names.page.clone(), "1".into());
        format!("{INDEX_PATH}?{}", query_string(&pairs))
    }

    fn row(&self, display: &DisplayContext, exchange: &Summary) -> Result<FlexRow> {
        let compact = self.columns.mode() == RowMode::Compact;
        // The projected row for this key, if the columns loaded one; the page
        // fills its own slots beside any walked cells.
        let mut row = self
            .columns
            .rows
            .iter()
            .find(|row| row.key == exchange.id)
            .cloned()
            .unwrap_or_else(|| FlexRow::new(exchange.id.clone()));
        row.dom_id = Some(format!("exchange-{}", exchange.id));
        row.slots.insert(
            "occurred_at".into(),
            render(TimeTag {
                id: &format!("exchange-time-{}", exchange.id),
                i: instant(display, exchange.occurred_at)?,
            })?,
        );
        row.slots.insert(
            "operation".into(),
            render(OperationLink {
                href: format!("{INDEX_PATH}/{}", exchange.id),
                text: &exchange.operation,
            })?,
        );
        row.slots
            .insert("method".into(), dash(exchange.method.as_deref()));
        row.slots.insert(
            "host".into(),
            render(HostCell {
                host: exchange.host.as_deref().unwrap_or(&exchange.endpoint),
                path: (!compact).then_some(exchange.path.as_str()),
            })?,
        );
        row.slots.insert(
            "status".into(),
            Html::text(
                &exchange
                    .response_status
                    .map_or_else(|| exchange.outcome.clone(), |status| status.to_string()),
            ),
        );
        row.slots.insert(
            "duration_ms".into(),
            dash(exchange.duration_ms.map(|ms| ms.to_string()).as_deref()),
        );
        let user = exchange.user_id.map(|id| id.to_string());
        row.slots.insert(
            "actor".into(),
            Html::text(
                user.as_deref()
                    .or(exchange.agent_id.as_deref())
                    .unwrap_or("System"),
            ),
        );
        row.slots.insert(
            "client_identity".into(),
            dash(exchange.client_identity.as_deref()),
        );
        row.slots
            .insert("self_report".into(), self_report(exchange));
        if self.columns.columns.iter().any(|shown| {
            fields::ALL
                .iter()
                .any(|field| field.column == Some(shown.id.as_str()))
                && !row.slots.contains_key(&shown.id)
        }) {
            for (field, value) in exchange_facts(display, exchange, "exchange-time")? {
                if let Some(column) = field.column {
                    row.slots.entry(column.into()).or_insert(value);
                }
            }
        }
        Ok(row)
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let display = self.pages.context(&**client, scope).await?;
        let clock = display.clock(self.browser_zone.as_deref(), Timestamp::now());
        let period = Period::parse(|key| self.period_params.get(key).cloned(), clock.today);
        self.period = Some(period);
        self.filter.set(period, &clock);
        // An unresolvable range lists nothing, as the source does, and says why.
        let (since, until, range_error) = match period.bounds(&clock.timezone) {
            Ok((since, until)) => (since.into(), until.into(), None),
            Err(error) => (UNIX_EPOCH, UNIX_EPOCH, Some(error)),
        };
        let filter = Filter {
            direction: if self.list.filters["direction"] == "incoming" {
                Direction::Incoming
            } else {
                Direction::Outgoing
            },
            outcome: Outcome::parse(&self.list.filters["status"]),
            search: self.list.search.clone(),
            since: Some(since),
            until: Some(until),
            ..Filter::default()
        };
        let list = |page: u64, size: u64| {
            self.pages
                .integration
                .list(&**client, scope, &filter, page as i64, size as i64)
        };
        let mut page = list(self.list.page, self.list.page_size).await?;
        // An out-of-range page (a stale link, a narrower filter) lands on the
        // last real page instead of an empty one.
        if self
            .list
            .clamp_to_last_page(page.total_pages.max(0) as u64, true)
        {
            page = list(self.list.page, self.list.page_size).await?;
        }
        let operator = match bilibi_tenancy::prove_operator(&**client, scope).await {
            Ok(_) => true,
            Err(Error::Forbidden) => false,
            Err(error) => return Err(error),
        };
        if self.reload {
            let keys: Vec<Value> = page.entries.iter().map(|row| json!(row.id)).collect();
            self.columns.load(scope, &keys, &display).await?;
            self.reload = false;
        }
        let hrefs: Vec<(String, &str, &str)> = DIRECTIONS
            .iter()
            .map(|(id, label)| (self.tab_href(id), *id, *label))
            .collect();
        let tabs = render(Tabs {
            id: "exchange-tabs",
            label: "Exchange direction",
            class: "mb-2",
            tabs: hrefs
                .iter()
                .map(|(href, id, label)| {
                    Tab::link(href, label, self.list.filters["direction"] == *id)
                })
                .collect(),
        })?;
        let form = self.list.filters_form();
        let mut toolbar = FilterToolbar::new(
            "exchange-filters",
            vec![
                ToolbarControl::search(
                    "exchange-search",
                    "search",
                    "Search API exchanges",
                    &form["search"],
                    "Host, path, operation, status or person/agent ID",
                ),
                ToolbarControl::select(
                    "exchange-status",
                    "status",
                    "Outcome",
                    &form["status"],
                    choices(&[
                        ("All outcomes", ""),
                        ("Success", "success"),
                        ("HTTP error", "http_error"),
                        ("Connection error", "connection_error"),
                        ("Unknown", "unknown"),
                    ]),
                ),
            ],
        );
        toolbar.class = "mb-0 min-w-0 flex-1";
        let mut rows = Vec::with_capacity(page.entries.len());
        for exchange in &page.entries {
            rows.push(self.row(&display, exchange)?);
        }
        let mut table = self.columns.flex_table("exchanges", rows);
        table.framed = false;
        table.caption = "API exchanges";
        table.empty = Some(TableEmpty::State(EmptyState::missing(
            "No API exchanges",
            "No calls match these filters.",
        )));
        let view = IndexView {
            tabs,
            toolbar: render(toolbar)?,
            period: render(self.filter.clone())?,
            range_error,
            operator,
            table: render(table)?,
            pagination: render(Pagination::new(
                "exchanges-pagination",
                PageCounts::new(
                    self.list.page,
                    self.list.page_size,
                    page.total_entries.max(0) as u64,
                ),
                &PAGE_SIZES,
            ))?,
        };
        view.render().map_err(|_| Error::Internal)
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let kind = get("type").ok_or(Error::Invalid)?;
        match kind {
            "filters" => {
                self.list.apply_filters(
                    &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                        .map_err(Error::refused)?,
                );
                self.reload = true;
            }
            "page" => {
                self.list.put_page(get("page"));
                self.reload = true;
            }
            "grid" => {
                // No column sorts: the log stays newest first. A changed
                // arrangement is already remembered; the rows load again
                // under it.
                match self
                    .columns
                    .handle(
                        scope,
                        &params::string_values_checked(value, params::FIELD_LIMIT)
                            .map_err(Error::refused)?,
                    )
                    .await?
                {
                    GridOutcome::Patch => self.reload = true,
                    GridOutcome::Update | GridOutcome::Noop | GridOutcome::Sort(_) => {}
                }
            }
            "browser_timezone" => {
                self.browser_zone = get("timezone")
                    .filter(|zone| zone.len() <= 64)
                    .map(str::to_owned);
                self.reload = true;
            }
            _ => {
                let clock = self.clock(scope).await?;
                match self
                    .filter
                    .event_checked(kind, value, &clock)
                    .map_err(Error::refused)?
                {
                    Some(period::Outcome::Applied(period)) => {
                        self.period_params = period
                            .params()
                            .into_iter()
                            .map(|(key, value)| (key.to_owned(), value))
                            .collect();
                        self.list.put_page(Some("1"));
                        self.reload = true;
                    }
                    Some(period::Outcome::Update) => {}
                    None => return Err(Error::Invalid),
                }
            }
        }
        Ok(())
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list = parse(values)?;
                    self.period_params = period_params(values);
                    self.open(request.scope, values).await?;
                    self.reload = true;
                }
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.query(),
                ..LiveBody::default()
            })
        })
    }
}

/// The mounted detail page.
struct Show {
    pages: Pages,
    id: String,
    payload: Option<String>,
    confirming: bool,
    flash: Option<(StatusKind, String)>,
}

impl Show {
    fn new(pages: Pages, id: String) -> Self {
        Self {
            pages,
            id,
            payload: None,
            confirming: false,
            flash: None,
        }
    }

    fn flash(&mut self, kind: StatusKind, message: String) {
        self.flash = Some((kind, message));
    }

    fn facts(display: &DisplayContext, exchange: &Summary) -> Result<Html> {
        render(Facts {
            id: "exchange-facts",
            items: exchange_facts(display, exchange, "exchange-time")?
                .into_iter()
                .map(|(field, value)| field.fact(value))
                .collect(),
        })
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        let exchange = self
            .pages
            .integration
            .get(&**client, scope, &self.id)
            .await?;
        let display = self.pages.context(&**client, scope).await?;
        let can_payload = bilibi_authz::decide(&**client, scope, PAYLOAD)
            .await?
            .allowed;
        let can_delete = bilibi_authz::decide(&**client, scope, DELETE)
            .await?
            .allowed;
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let dialog = if self.confirming {
            render(ConfirmDialog {
                id: "exchange-delete-confirm",
                consequence: "This exchange and any retained payload will be permanently deleted.",
                detail: "The record cannot be restored.",
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?
        } else {
            Html::EMPTY
        };
        ShowView {
            flash,
            exchange_id: &exchange.id,
            all_link: Html::raw(format!(r#"<a href="{INDEX_PATH}">All exchanges</a>"#)),
            facts: Self::facts(&display, &exchange)?,
            can_payload,
            can_delete,
            payload: self.payload.as_deref(),
            payload_restricted: EmptyState::missing(
                "Payload inspection restricted",
                "Ask an administrator for the payload inspection permission.",
            ),
            delete_restricted: EmptyState::missing(
                "Deletion restricted",
                "Ask an administrator for the exchange deletion permission.",
            ),
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    fn pretty(payload: &Payload) -> Result<String> {
        serde_json::to_string_pretty(&json!({
            "request_headers": payload.request_headers,
            "request_body": payload.request_body,
            "response_headers": payload.response_headers,
            "response_body": payload.response_body,
        }))
        .map_err(|_| Error::Internal)
    }

    /// `Some(path)` is a full navigation after a completed deletion.
    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<Option<String>> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "payload" => {
                let client = self.pages.db.get().await?;
                match self
                    .pages
                    .integration
                    .payload(&**client, scope, &self.id)
                    .await
                {
                    Ok(payload) => self.payload = Some(Self::pretty(&payload)?),
                    Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                    Err(error) => self.flash(
                        StatusKind::Error,
                        format!("Payload unavailable: {}.", reason(&error)),
                    ),
                }
            }
            "confirm_delete" => self.confirming = true,
            "cancel_delete" => self.confirming = false,
            "delete" => {
                self.confirming = false;
                let mut client = self.pages.db.get().await?;
                match self
                    .pages
                    .integration
                    .delete(&mut client, scope, &self.id)
                    .await
                {
                    Ok(()) => {
                        self.flash(StatusKind::Success, "Exchange deleted.".into());
                        return Ok(Some(INDEX_PATH.to_owned()));
                    }
                    Err(Error::Unauthorized) => return Err(Error::Unauthorized),
                    Err(error) => self.flash(
                        StatusKind::Error,
                        format!("Exchange could not be deleted: {}.", reason(&error)),
                    ),
                }
            }
            "clear-flash" => self.flash = None,
            _ => return Err(Error::Invalid),
        }
        Ok(None)
    }
}

impl LiveState<Scope, Error> for Show {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    if let Some(to) = self.event(request.scope, value).await? {
                        return Ok(LiveBody {
                            redirect: Some(to),
                            flash: self.flash.take(),
                            ..LiveBody::default()
                        });
                    }
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
}

/// The exchange id of `/system/api-exchanges/{id}`; anything unshaped is the
/// same not-found answer as a row another tenant owns.
fn exchange_id(request: &PageRequest<'_, Scope>) -> Result<String> {
    request
        .path
        .get("id")
        .filter(|id| !id.is_empty() && id.len() <= 40)
        .cloned()
        .ok_or(Error::NotFound)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "exchanges" => "API Exchanges",
            "exchange" => "API Exchange",
            _ => return None,
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = match request.operation {
                "exchanges" => {
                    let mut index = Index::new(self.clone(), request.query)?;
                    index.open(scope, request.query).await?;
                    index.render(scope).await?
                }
                "exchange" => {
                    Show::new(self.clone(), exchange_id(&request)?)
                        .render(scope)
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
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "exchanges" => {
                    let scope = request.scope.ok_or(Error::Unauthorized)?;
                    let mut index = Index::new(self.clone(), request.query)?;
                    index.open(scope, request.query).await?;
                    Box::new(index)
                }
                "exchange" => Box::new(Show::new(self.clone(), exchange_id(&request)?)),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}

// Bilimbi Integration.ShowLive metadata, shared with IndexLive columns.
bilibi_ui::record_fields! { pub(crate) mod fields {
    DIRECTION: bilibi_ui::RecordField::new("direction", "Direction"),
    OPERATION: bilibi_ui::RecordField::new("operation", "Operation"),
    METHOD: bilibi_ui::RecordField::new("method", "Method"),
    ENDPOINT: bilibi_ui::RecordField::new("endpoint", "Endpoint"),
    OUTCOME: bilibi_ui::RecordField::new("outcome", "Outcome"),
    STATUS: bilibi_ui::RecordField::new("status", "HTTP status").excluded("The default Status column shows it"),
    DURATION_MS: bilibi_ui::RecordField::new("duration_ms", "Duration").excluded("The default Duration (ms) column shows it"),
    OCCURRED_AT: bilibi_ui::RecordField::new("occurred_at", "Time"),
    TENANT: bilibi_ui::RecordField::new("tenant", "Tenant"),
    OWNER: bilibi_ui::RecordField::new("owner", "Owner"),
    USER_ID: bilibi_ui::RecordField::new("user_id", "Person"),
    IMPERSONATOR: bilibi_ui::RecordField::new("impersonator", "Impersonator"),
    CONNECTION: bilibi_ui::RecordField::new("connection", "Connection"),
    AGENT: bilibi_ui::RecordField::new("agent", "Agent"),
    RUN: bilibi_ui::RecordField::new("run", "Run"),
    CLIENT_IDENTITY: bilibi_ui::RecordField::new("client_identity", bilibi_tenancy::ClientIdentity::LABEL),
    SELF_REPORT: bilibi_ui::RecordField::new("self_report", bilibi_tenancy::SelfReport::LABEL),
    ERROR: bilibi_ui::RecordField::new("error", "Error"),
} }

fn exchange_facts(
    display: &DisplayContext,
    exchange: &Summary,
    time_id: &str,
) -> Result<Vec<(bilibi_ui::RecordField, Html)>> {
    let owner = match (&exchange.owner_type, exchange.owner_id) {
        (None, None) => "—".to_owned(),
        (kind, id) => format!(
            "{} {}",
            kind.as_deref().unwrap_or("—"),
            id.map(|id| id.to_string()).unwrap_or_default()
        )
        .trim_end()
        .to_owned(),
    };
    let text = |value: Option<String>| dash(value.as_deref());
    Ok(vec![
        (fields::get("direction"), Html::text(&exchange.direction)),
        (fields::OPERATION, Html::text(&exchange.operation)),
        (fields::METHOD, dash(exchange.method.as_deref())),
        (fields::ENDPOINT, Html::text(&exchange.endpoint)),
        (fields::OUTCOME, Html::text(&exchange.outcome)),
        (
            fields::STATUS,
            text(exchange.response_status.map(|v| v.to_string())),
        ),
        (
            fields::DURATION_MS,
            text(exchange.duration_ms.map(|v| format!("{v} ms"))),
        ),
        (
            fields::OCCURRED_AT,
            render(TimeTag {
                id: time_id,
                i: instant(display, exchange.occurred_at)?,
            })?,
        ),
        (
            fields::TENANT,
            Html::text(
                &exchange
                    .tenant_id
                    .map_or_else(|| "Unknown / system".to_owned(), |id| id.to_string()),
            ),
        ),
        (fields::OWNER, Html::text(&owner)),
        (
            fields::USER_ID,
            text(exchange.user_id.map(|v| v.to_string())),
        ),
        (
            fields::IMPERSONATOR,
            text(exchange.impersonator_id.map(|v| v.to_string())),
        ),
        (fields::CONNECTION, dash(exchange.connection_id.as_deref())),
        (fields::AGENT, dash(exchange.agent_id.as_deref())),
        (fields::RUN, dash(exchange.run_id.as_deref())),
        (
            fields::CLIENT_IDENTITY,
            dash(exchange.client_identity.as_deref()),
        ),
        (fields::SELF_REPORT, self_report(exchange)),
        (fields::ERROR, dash(exchange.error_class.as_deref())),
    ])
}
