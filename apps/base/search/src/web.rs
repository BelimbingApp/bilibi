//! The search box's records (the `shell.search.records` panel), the `/search`
//! results page and the person's own Search settings tab (`/settings/search`).
//! No Bilimbi counterpart. Every search goes through the `base.search.records`
//! contract (ADR 01), as an agent's does, so each shows only records the
//! reader may open; these own presentation only.
use crate::{
    contributions::{MAX_PAGE, RECORDS},
    Count, Found, Hit, Search,
};
use askama::Template;
use bilibi_agent_api::{page::PageContracts, Error as ContractError};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    flex_table::{Cell, ColumnKind, Instant},
    list_state::{InvalidPageSize, ParamNames, FILTERS_FORM},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Card, Choice, EmptyState, Field, FilterToolbar,
    FlashGroup, FlashMap, FlexRow, FlexTable, Form, Header, Html, InputKind, ListColumns, ListSpec,
    ListState, PageCounts, Pagination, RecordGroup, RecordHit, RecordLink, SearchRecords,
    SettingsTab, TableEmpty, ToolbarControl,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::{Arc, OnceLock},
    time::SystemTime,
};

const SIZES: [u64; 3] = [25, 50, 100];
const SAVED: &str = "Search settings saved.";

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// A state as a person reads it: `retired` is "Retired".
fn state_label(state: &str) -> String {
    let mut chars = state.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// Where every result for `text` opens.
fn results_href(text: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("q", text)
        .finish();
    format!("/search?{query}")
}

/// `base.search.records`' answer (`contributions::answer`), read back.
#[derive(Deserialize)]
struct Answer {
    query: String,
    too_short: bool,
    min_chars: usize,
    too_broad: bool,
    #[serde(default)]
    counts: Vec<AnswerCount>,
    hits: Vec<AnswerHit>,
}
#[derive(Deserialize)]
struct AnswerCount {
    kind: String,
    matched: u64,
}
#[derive(Deserialize)]
struct AnswerHit {
    kind: String,
    id: String,
    title: String,
    detail: Vec<String>,
    state: Option<String>,
    link: String,
    updated: Option<String>,
}
impl Answer {
    fn found(self, search: &Search) -> Result<Found> {
        let kind = |id: &str| search.kind(id).ok_or(Error::Internal);
        Ok(Found {
            text: self.query,
            hits: self
                .hits
                .into_iter()
                .map(|hit| {
                    Ok(Hit {
                        kind: kind(&hit.kind)?,
                        updated: hit
                            .updated
                            .map(|at| at.parse::<jiff::Timestamp>().map(SystemTime::from))
                            .transpose()
                            .map_err(|_| Error::Internal)?,
                        id: hit.id,
                        title: hit.title,
                        detail: hit.detail,
                        state: hit.state,
                        href: hit.link,
                    })
                })
                .collect::<Result<_>>()?,
            counts: self
                .counts
                .into_iter()
                .map(|count| {
                    Ok(Count {
                        kind: kind(&count.kind)?,
                        matched: count.matched,
                    })
                })
                .collect::<Result<_>>()?,
            min_chars: self.min_chars,
            short: self.too_short,
            broad: self.too_broad,
        })
    }
}

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub struct Pages {
    db: Db,
    search: Arc<Search>,
    contracts: Arc<PageContracts>,
    display: Arc<DisplayPolicy>,
    locales: Arc<Locales>,
}

impl Pages {
    pub fn new(
        db: Db,
        search: Arc<Search>,
        contracts: Arc<PageContracts>,
        display: Arc<DisplayPolicy>,
        locales: Arc<Locales>,
    ) -> Self {
        Self {
            db,
            search,
            contracts,
            display,
            locales,
        }
    }

    /// One search through the contract, its answer read back into hits.
    async fn found(&self, scope: &Scope, input: Value) -> Result<Found> {
        let mut reply = self
            .contracts
            .read(scope, RECORDS, &input)
            .await
            .map_err(|error| match error {
                ContractError::Backend(error) => error,
                ContractError::Forbidden => Error::Forbidden,
                _ => Error::Internal,
            })?;
        let answer: Answer =
            serde_json::from_value(reply["result"].take()).map_err(|_| Error::Internal)?;
        answer.found(&self.search)
    }

    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }

    /// The records panel for `query` (none on a first render): what the
    /// search box shows between Pages and Account. A person who may not
    /// search records gets the panel's root, empty and hidden.
    async fn records(&self, scope: &Scope, query: Option<&str>) -> Result<String> {
        let off = || render(SearchRecords::default()).map(|html| html.to_string());
        let Some(min_chars) = self.search.offered(scope).await? else {
            return off();
        };
        // Echo the query as the search box sent it, so it can tell this
        // answer from one still on its way.
        let query: String = query
            .unwrap_or_default()
            .chars()
            .take(crate::MAX_TEXT)
            .collect();
        let found = if query.trim().is_empty() {
            Found::default()
        } else {
            match self
                .found(scope, json!({"q": query, "mode": "suggest"}))
                .await
            {
                Ok(found) => found,
                // Revoked since the panel was drawn.
                Err(Error::Forbidden) => return off(),
                Err(error) => return Err(error),
            }
        };
        let mut groups: Vec<RecordGroup> = Vec::new();
        for hit in &found.hits {
            let at = match groups.iter().position(|group| group.id == hit.kind.id) {
                Some(at) => at,
                None => {
                    groups.push(RecordGroup {
                        id: hit.kind.id.to_owned(),
                        label: hit.kind.label.to_owned(),
                        hits: Vec::new(),
                    });
                    groups.len() - 1
                }
            };
            groups[at].hits.push(RecordHit {
                title: hit.title.clone(),
                detail: hit.detail.join(" · "),
                state: hit.state.as_deref().map(state_label).unwrap_or_default(),
                href: hit.href.clone(),
            });
        }
        let all = if found.hits.is_empty() {
            String::new()
        } else {
            results_href(&found.text)
        };
        render(SearchRecords {
            enabled: true,
            query: &query,
            min_chars,
            groups,
            all_href: &all,
            notice: if found.broad {
                "Too many records match. Add another word."
            } else {
                ""
            },
        })
        .map(|html| html.to_string())
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "results" => Some("Search"),
            "preferences" => Some("Search settings"),
            _ => None,
        }
    }

    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = match request.operation {
                "records" => self.records(scope, None).await?,
                "results" => {
                    Results::new(self.clone(), request.query, request.framed)?
                        .view(scope)
                        .await?
                        .html
                }
                "preferences" => preferences(self, scope, false).await?,
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
                "records" => Box::new(Records {
                    pages: self.clone(),
                    html: None,
                }),
                "results" => Box::new(Results::new(self.clone(), request.query, request.framed)?),
                "preferences" => Box::new(Preferences {
                    pages: self.clone(),
                    saved: false,
                }),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}

// ---- The search box's records ------------------------------------------------

/// One socket's records panel: the rows for the last query the box sent.
struct Records {
    pages: Pages,
    html: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum RecordsEvent {
    Query { q: String },
}

impl LiveState<Scope, Error> for Records {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let html = match request.input {
                // The shell's refreshes keep the rows; the next query reads
                // afresh, so a revoked kind leaves on the next keystroke.
                LiveInput::Refresh => match self.html.clone() {
                    Some(html) => html,
                    None => self.pages.records(request.scope, None).await?,
                },
                LiveInput::Params(_) => return Err(Error::Invalid),
                LiveInput::Event(value) => {
                    let RecordsEvent::Query { q } =
                        RecordsEvent::deserialize(value).map_err(|_| Error::Invalid)?;
                    self.pages.records(request.scope, Some(&q)).await?
                }
            };
            self.html = Some(html.clone());
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
}

// ---- /search -------------------------------------------------------------------

/// The results list's query names: `q`, `kind`, `page`, `per_page`. Built once
/// from the installed kinds, since the kind filter accepts only those.
fn spec(search: &Search) -> Arc<ListSpec> {
    static SPEC: OnceLock<Arc<ListSpec>> = OnceLock::new();
    SPEC.get_or_init(|| {
        let kinds: Vec<&str> = std::iter::once("all")
            .chain(search.kinds().iter().map(|kind| kind.id))
            .collect();
        ListSpec {
            page_sizes: SIZES.to_vec(),
            default_page_size: SIZES[0],
            page_size_param: "per_page".into(),
            invalid_page_size: InvalidPageSize::Default,
            filters: BTreeMap::from([("kind".into(), bilibi_ui::Filter::one_of(&kinds, "all"))]),
            param_names: ParamNames {
                search: "q".into(),
                ..ParamNames::default()
            },
            omit_blank: vec!["search".into()],
            omit_defaults: true,
            ..ListSpec::default()
        }
        .shared()
        .expect("search results list spec")
    })
    .clone()
}

fn columns() -> Result<ListColumns> {
    ListColumns::mount(
        "search-results",
        vec![
            BuiltinColumn::new("record", "Record"),
            BuiltinColumn::new("kind", "Kind"),
            BuiltinColumn::new("detail", "Details"),
            BuiltinColumn::new("updated", "Updated"),
        ],
    )
    .map_err(|_| Error::Internal)
}

fn instant(display: &DisplayContext, at: SystemTime) -> Result<Instant> {
    let at = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Ok(Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    ))
}

/// A row key as an element id: letters, digits, `_` and `-`.
fn dom(key: &str) -> String {
    key.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Why the results list is empty, and what to do next.
fn empty(found: &Found, text: &str) -> EmptyState {
    let (title, reason) = if text.trim().is_empty() {
        (
            "Search for a record".to_owned(),
            "Type a name, code or other word.",
        )
    } else if found.short {
        (format!("Type at least {} characters", found.min_chars), "")
    } else if found.broad {
        (
            "Too many records match".to_owned(),
            "Add another word to narrow the search.",
        )
    } else {
        (
            format!("No records match “{}”", found.text),
            "Check the spelling, or use fewer words.",
        )
    };
    EmptyState::missing(title, reason).id("search-results-empty")
}

struct Results {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    framed: bool,
}

impl Results {
    fn new(pages: Pages, query: &HashMap<String, String>, framed: bool) -> Result<Self> {
        Ok(Self {
            list: ListState::parse(query, spec(&pages.search)).map_err(|_| Error::Internal)?,
            columns: columns()?,
            pages,
            framed,
        })
    }

    fn kind(&self) -> Option<&str> {
        Some(self.list.filters["kind"].as_str()).filter(|kind| *kind != "all")
    }

    async fn find(&self, scope: &Scope) -> Result<Found> {
        // A page past the operation's last reads that one; the view then
        // clamps to the real last page.
        let input = json!({
            "q": self.list.search,
            "kind": self.kind().unwrap_or_default(),
            "page": self.list.page.min(MAX_PAGE),
            "page_size": self.list.page_size,
        });
        self.pages.found(scope, input).await
    }

    fn row(&self, display: &DisplayContext, hit: &Hit) -> Result<FlexRow> {
        let mut record = render(RecordLink {
            workspace: self.framed,
            kind: hit.kind.owner,
            record_id: &hit.id,
            href: &hit.href,
            class: "font-medium text-ink-strong hover:underline",
            body: Html::text(&hit.title),
        })?;
        if let Some(state) = &hit.state {
            record.push(&render(Badge {
                kind: BadgeKind::Neutral,
                dot: false,
                class: "ml-1.5",
                body: Html::text(&state_label(state)),
            })?);
        }
        let key = format!("{}-{}", hit.kind.id, hit.id);
        let mut row = FlexRow::new(&key)
            .slot("record", record)
            .slot("kind", Html::text(hit.kind.label))
            .slot("detail", Html::text(&hit.detail.join(" · ")));
        if let Some(at) = hit.updated {
            row = row.cell("updated", Cell::instant(instant(display, at)?));
        }
        row.dom_id = Some(format!("search-result-{}", dom(&key)));
        Ok(row)
    }

    async fn view(&mut self, scope: &Scope) -> Result<LiveBody> {
        let mut found = self.find(scope).await?;
        let total = found.total(self.kind());
        if self
            .list
            .clamp_to_last_page(total.div_ceil(self.list.page_size), true)
        {
            found = self.find(scope).await?;
        }
        let display = self.pages.display(scope).await?;
        let mut body = render(Header {
            title: Html::text("Search"),
            ..Header::default()
        })?;
        let form = self.list.filters_form();
        let count =
            |matched: u64, capped: bool| format!("{matched}{}", if capped { "+" } else { "" });
        let searched = !found.counts.is_empty();
        let mut kinds = vec![Choice::new(
            if searched {
                format!(
                    "All kinds ({})",
                    count(found.total(None), found.counts.iter().any(|c| c.capped()))
                )
            } else {
                "All kinds".to_owned()
            },
            "all",
        )];
        kinds.extend(found.counts.iter().map(|c| {
            Choice::new(
                format!("{} ({})", c.kind.label, count(c.matched, c.capped())),
                c.kind.id,
            )
        }));
        // A kind the address names stays selectable, counted or not.
        if let Some(kind) = self.kind().and_then(|id| self.pages.search.kind(id)) {
            if !found.counts.iter().any(|c| c.kind.id == kind.id) {
                kinds.push(Choice::new(kind.label, kind.id));
            }
        }
        body.push(&render(FilterToolbar::new(
            "search-filters",
            vec![
                ToolbarControl::search(
                    "search-query",
                    "search",
                    "Search records",
                    &form["search"],
                    "Search by name, code or other words...",
                ),
                ToolbarControl::select("search-kind", "kind", "Kind", &form["kind"], kinds),
            ],
        ))?);
        let rows = found
            .hits
            .iter()
            .map(|hit| self.row(&display, hit))
            .collect::<Result<Vec<_>>>()?;
        let mut views = self.columns.flex_columns();
        if let Some(updated) = views.iter_mut().find(|column| column.id == "updated") {
            updated.kind = ColumnKind::Field;
        }
        let mut table = FlexTable::new("search-results", views, rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Search results";
        table.framed = false;
        if found.hits.is_empty() {
            table.empty = Some(TableEmpty::State(empty(&found, &self.list.search)));
        }
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "search-results-pagination",
            PageCounts::new(self.list.page, self.list.page_size, total),
            &SIZES,
        ))?);
        body.push(&render(Card {
            id: "search-results-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        let html = render(bilibi_ui::Page {
            id: "search-results-page",
            body,
            ..bilibi_ui::Page::default()
        })?
        .to_string();
        Ok(LiveBody {
            html,
            query: self.list.to_params(),
            rows: found.hits.len(),
            ..LiveBody::default()
        })
    }
}

impl LiveState<Scope, Error> for Results {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(query) => {
                    self.list = ListState::parse(query, spec(&self.pages.search))
                        .map_err(|_| Error::Internal)?;
                }
                LiveInput::Event(value) => match value.get("type").and_then(Value::as_str) {
                    Some("filters") => self.list.apply_filters(
                        &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                            .map_err(Error::refused)?,
                    ),
                    Some("page") => self
                        .list
                        .put_page(value.get("page").and_then(Value::as_str)),
                    Some("grid") => {
                        self.columns.handle(
                            &params::string_values_checked(value, params::FIELD_LIMIT)
                                .map_err(Error::refused)?,
                        );
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            self.view(request.scope).await
        })
    }
}

// ---- /settings/search ----------------------------------------------------------

#[derive(Template)]
#[template(path = "preferences.html")]
struct PreferencesView {
    flash: Html,
    header: Html,
    current: SettingsTab,
    heading: &'static str,
    subheading: &'static str,
    body: Html,
}

/// The person's Search tab: a tick for each kind they may search, kept as
/// they change it.
async fn preferences(pages: &Pages, scope: &Scope, saved: bool) -> Result<String> {
    let body = match pages.search.preferences(scope).await {
        Err(Error::Forbidden) => render(Card {
            id: "search-preferences",
            inner_class: "px-2 py-8",
            body: render(
                EmptyState::forbidden("search records").id("search-preferences-forbidden"),
            )?,
            ..Card::default()
        })?,
        Err(error) => return Err(error),
        Ok(kinds) if kinds.is_empty() => render(Card {
            id: "search-preferences",
            inner_class: "px-2 py-8",
            body: render(
                EmptyState::missing(
                    "No records to search yet",
                    "Ask your administrator for access to the records you need.",
                )
                .id("search-preferences-empty"),
            )?,
            ..Card::default()
        })?,
        Ok(kinds) => {
            let mut fields = Html::EMPTY;
            for preference in kinds {
                fields.push(&render(
                    Field::named(format!("kinds[{}]", preference.kind.id))
                        .id(format!("search-kind-{}", preference.kind.id))
                        .kind(InputKind::Checkbox)
                        .checked(preference.shown)
                        .label(preference.kind.label),
                )?);
            }
            render(Card {
                id: "search-preferences",
                body: render(Form {
                    id: "search-preferences-form",
                    event: "keep",
                    change: true,
                    class: "",
                    body: fields,
                    actions: Html::EMPTY,
                })?,
                ..Card::default()
            })?
        }
    };
    render(PreferencesView {
        flash: render(FlashGroup {
            id: "flash-group",
            flash: FlashMap {
                success: if saved { SAVED } else { "" },
                ..FlashMap::default()
            },
        })?,
        header: render(Header {
            title: Html::text("My Settings"),
            ..Header::default()
        })?,
        current: SettingsTab::Search,
        heading: "Search",
        subheading: "Choose which kinds of records your search shows.",
        body,
    })
    .map(|html| html.to_string())
}

struct Preferences {
    pages: Pages,
    /// The "saved" flash, shown until dismissed.
    saved: bool,
}

impl LiveState<Scope, Error> for Preferences {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut notice = String::new();
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(value) => match value.get("type").and_then(Value::as_str) {
                    Some("keep") => {
                        let shown: BTreeSet<String> =
                            params::form_values_checked(value, "kinds", params::FIELD_LIMIT)
                                .map_err(Error::refused)?
                                .into_iter()
                                .filter(|(_, ticked)| ticked == "true")
                                .map(|(kind, _)| kind)
                                .collect();
                        match self.pages.search.keep(request.scope, &shown).await {
                            Ok(()) => {
                                self.saved = true;
                                notice = SAVED.into();
                            }
                            // Revoked meanwhile: the page says so below.
                            Err(Error::Forbidden) => self.saved = false,
                            Err(error) => return Err(error),
                        }
                    }
                    Some("clear-flash") => self.saved = false,
                    _ => return Err(Error::Invalid),
                },
            }
            Ok(LiveBody {
                html: preferences(&self.pages, request.scope, self.saved).await?,
                notice,
                ..LiveBody::default()
            })
        })
    }
}
