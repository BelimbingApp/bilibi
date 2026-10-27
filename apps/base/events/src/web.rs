//! `/system/events` (`base/events.pages`, operation `index`): the tenant's
//! event subscribers, the last event each was handed and its gap, with
//! a replay that closes it. No Bilimbi counterpart (Bilimbi has no domain
//! events); the list follows Bilimbi `Session.Web.IndexLive` as Sessions does.
//!
//! A subscriber's gap is read with the page: events of a name it reads that it
//! has no delivery for (recorded while it was unmounted, or before it was
//! first mounted) and deliveries still pending. Replay confirms through the
//! shared dialog: the request holds the row's subscriber and the event its gap
//! starts at, and `replay` acts on that held request, never on a client value.
//! The facade rechecks `admin.events.replay` and the tenant's step-up policy
//! in its own transaction; a refusal is a flash, a step-up the host's prompt.
use crate::{Events, SubscriberState, REPLAY};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Card, ColumnType, ColumnsOutcome, ConfirmDialog,
    FilterToolbar, FlashMap, FlexRow, FlexTable, Header, Html, IconButton, IconContext, IconKind,
    Instant, ListColumns, ListSpec, ListState, Pagination, RowMode, StatusKind, TableEmpty,
    ToolbarControl, NO_FLASH,
};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

pub const PATH: &str = "/system/events";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission to replay events.";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("event subscriber list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let mut last = BuiltinColumn::new("last_delivered", "Last delivered");
    last.kind = ColumnType::Datetime;
    vec![
        BuiltinColumn::new("subscriber", "Subscriber"),
        BuiltinColumn::new("status", "Status"),
        BuiltinColumn::new("events", "Events read"),
        last,
        BuiltinColumn::new("pending", "Pending"),
        BuiltinColumn::new("missed", "Missed"),
    ]
}

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub events: Arc<Events>,
    pub contracts: Arc<PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}<span class="whitespace-nowrap text-ink-muted"><span class="tabular-nums">#{{ event }}</span> {% call flex::instant(id=id, i=i) %}{% endcall %}</span>"#,
    ext = "html"
)]
struct LastCell<'a> {
    id: &'a str,
    event: i64,
    i: Instant,
}

#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums {% if count > 0 %}text-ink{% else %}text-ink-muted{% endif %}">{{ count }}</span>{% if let Some(note) = note %}<span class="block text-xs text-ink-muted">{{ note }}</span>{% endif %}"#,
    ext = "html"
)]
struct CountCell {
    count: i64,
    note: Option<String>,
}

#[derive(Template)]
#[template(
    source = r#"<span class="font-mono text-xs text-ink">{{ text }}</span>"#,
    ext = "html"
)]
struct KeyCell<'a> {
    text: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"{% for name in names %}<span class="block font-mono text-xs text-ink-muted">{{ name }}</span>{% endfor %}"#,
    ext = "html"
)]
struct NamesCell<'a> {
    names: &'a [&'a str],
}

#[derive(Template)]
#[template(path = "events.html")]
struct IndexView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
    replay_note: bool,
}

/// A replay the person asked for and has not yet confirmed.
struct Request {
    subscriber: String,
    from: i64,
    missed: i64,
    pending: i64,
}

struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    /// The subscribers last shown: a replay request names one of these.
    rows: Vec<SubscriberState>,
    request: Option<Request>,
    flash: Option<(StatusKind, String)>,
}

impl Index {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: parse(query)?,
            columns: ListColumns::mount("event-subscribers", builtins())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            request: None,
            flash: None,
        })
    }
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self.state.apply_filters(
                &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
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
            "request_replay" => {
                let key = get("key").unwrap_or_default();
                // Gone or closed since the list was drawn: the redraw shows that.
                if let Some(row) = self.rows.iter().find(|row| row.subscriber == key) {
                    if let (true, Some(from)) = (row.mounted, row.gap_from()) {
                        self.flash = None;
                        self.request = Some(Request {
                            subscriber: row.subscriber.clone(),
                            from,
                            missed: row.missed,
                            pending: row.pending,
                        });
                    }
                }
            }
            "cancel_replay" => self.request = None,
            "replay" => {
                let Some(request) = self.request.take() else {
                    return Ok(());
                };
                let key = request.subscriber;
                // The `base.events.replay` contract, as agents call it.
                let call = Call {
                    scope,
                    credential,
                    service: crate::PAGES,
                    event: "replay",
                    contract: "base.events.replay",
                };
                let outcome = self
                    .pages
                    .contracts
                    .submit(
                        &call,
                        json!({"subscriber": key, "from_event": request.from, "limit": 1000}),
                    )
                    .await
                    .map(|result| {
                        let replay = &result["result"];
                        let count = |field: &str| replay[field].as_i64().unwrap_or_default();
                        (count("pending"), count("created"), replay["next"].as_i64())
                    });
                match outcome {
                    Ok((0, _, _)) => self.flash(
                        StatusKind::Info,
                        format!("Nothing to replay: every event from #{} on is already delivered to {key}.", request.from),
                    ),
                    Ok((pending, created, next)) => {
                        let mut message = format!(
                            "{pending} events handed to {key} ({created} new deliveries). Delivery runs in the background."
                        );
                        if let Some(next) = next {
                            message.push_str(&format!(" More events remain from #{next}: replay again."));
                        }
                        self.flash(StatusKind::Success, message);
                    }
                    // Base Step-up: the host sends the browser to confirm and back.
                    Err(ContractError::Backend(
                        error @ (Error::StepUpRequired { .. } | Error::Unauthorized),
                    )) => return Err(error),
                    Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                        self.flash(StatusKind::Error, DENIED)
                    }
                    Err(ContractError::InvalidInput(fields)) if fields.contains_key("subscriber") => self.flash(
                        StatusKind::Error,
                        format!("{key} is not mounted, so nothing was replayed."),
                    ),
                    Err(_) => self.flash(
                        StatusKind::Error,
                        "The event store is unavailable; nothing was replayed.",
                    ),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let locale = pages.locales.resolve(&**client, scope, None).await?.locale;
        let display = pages
            .display
            .display(&**client, scope, None, locale)
            .await?;
        let can_replay = effective_capabilities(&**client, scope)
            .await?
            .contains(REPLAY);
        let mut all = pages.events.subscriber_states(&**client, scope).await?;
        let search = self.state.search.to_lowercase();
        if !search.is_empty() {
            all.retain(|row| row.subscriber.to_lowercase().contains(&search));
        }
        let page = self.state.paginate(&all);
        let (entries, counts) = (page.entries.to_vec(), page.counts);
        self.rows = entries;
        if self.rows_changed {
            self.columns.load(&self.rows, |row| row.subscriber.clone());
        }
        let header = render(Header {
            title: Html::text("Events"),
            subtitle: "Check that each part of the system has received its events.",
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "events-pin",
                nav_pin: "nav-admin-system-events",
                ..IconButton::new("bilimbi-pin", "Pin Events to sidebar")
            })?,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "events-filters",
            vec![ToolbarControl::search(
                "events-search",
                "search",
                "Search subscribers",
                &form["search"],
                "Search by subscriber...",
            )],
        ))?;
        let compact = self.columns.mode() == RowMode::Compact;
        let mut rows = Vec::with_capacity(self.rows.len());
        let mut replay_note = false;
        for state in &self.rows {
            let dom = state.subscriber.replace(['/', '.', '_'], "-");
            let gap = state.gap_from();
            let status = if !state.mounted {
                render(Badge::status(BadgeKind::Neutral, "Not mounted"))?
            } else if gap.is_some() {
                render(Badge::status(BadgeKind::Warning, "Gap"))?
            } else {
                render(Badge::status(BadgeKind::Success, "Up to date"))?
            };
            let last = match &state.last_delivered {
                Some(last) => {
                    let id = format!("events-last-{dom}");
                    render(LastCell {
                        id: &id,
                        event: last.event_id,
                        i: instant(&display, last.delivered_at),
                    })?
                }
                None => Html::text("—"),
            };
            let pending_note = (!compact && state.failing > 0).then(|| {
                format!(
                    "{} failing, from #{}",
                    state.failing,
                    state.first_pending.unwrap_or_default()
                )
            });
            let missed_note = (!compact)
                .then_some(state.first_missed)
                .flatten()
                .map(|first| format!("from #{first}"));
            let actions = if can_replay && state.mounted && gap.is_some() {
                let id = format!("events-replay-{dom}");
                render(IconButton {
                    id: &id,
                    kind: IconKind::Neutral,
                    event: "request_replay",
                    name: "key",
                    value: &state.subscriber,
                    ..IconButton::new("refresh", "Replay events")
                })?
            } else {
                replay_note |= state.mounted && gap.is_some();
                Html::EMPTY
            };
            let mut row = FlexRow::new(state.subscriber.clone())
                .slot(
                    "subscriber",
                    render(KeyCell {
                        text: &state.subscriber,
                    })?,
                )
                .slot("status", status)
                .slot(
                    "events",
                    render(NamesCell {
                        names: &state.events,
                    })?,
                )
                .slot("last_delivered", last)
                .slot(
                    "pending",
                    render(CountCell {
                        count: state.pending,
                        note: pending_note,
                    })?,
                )
                .slot(
                    "missed",
                    render(CountCell {
                        count: state.missed,
                        note: missed_note,
                    })?,
                );
            row.dom_id = Some(format!("events-{dom}"));
            row.actions = actions;
            rows.push(row);
        }
        let mut table = FlexTable::new("event-subscribers", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Event subscribers";
        table.framed = false;
        table.actions = can_replay;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No event deliveries yet."
            } else {
                "No subscribers match this search."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "events-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "events-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.request {
            Some(request) => {
                let consequence = format!(
                    "Replay events to {} from event #{}?",
                    request.subscriber, request.from
                );
                let detail = format!(
                    "{} missed events will be sent and {} waiting deliveries retried. Previously delivered events are skipped. The subscriber will act on each replayed event as if it just happened.",
                    request.missed, request.pending
                );
                render(ConfirmDialog {
                    id: "events-replay-confirm",
                    consequence: &consequence,
                    detail: &detail,
                    confirm: "Replay",
                    working: "Replaying…",
                    confirm_event: "replay",
                    cancel_event: "cancel_replay",
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
            replay_note,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

fn instant(display: &DisplayContext, at: Timestamp) -> Instant {
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    )
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
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

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Events")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut index = Index::new(self.clone(), request.query)?;
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
            if request.operation != "index" {
                return Ok(None);
            }
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(Index::new(self.clone(), request.query)?);
            Ok(Some(state))
        })
    }
}
