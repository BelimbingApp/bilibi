//! `/audit/actions` (`base/audit_views.pages`, operation `actions`): the
//! operational action log, newest first, ported from Bilimbi
//! `apps/base/audit/lib/audit/web/actions_live.{ex,html.heex}`.
//!
//! The route guard checks `admin.audit.log.list` on every request and live
//! event. `admin.audit.log.manage` only decides whether the retain control is
//! drawn; the write asks again, inside the same live event, and a refusal
//! flashes instead of clearing the page. Retaining flips a bookmark beside the
//! immutable record (`bilibi_audit::toggle_retained`) and is itself audited.
//!
//! Dates are shown through the request's `DisplayContext`; the screen has no
//! period filter, as Bilimbi's has none (its filters are search, event family,
//! result, diagnostic traffic and actor type).
use crate::{
    present::{self, Presentation},
    MANAGE,
};
use askama::Template;
use bilibi_audit::{
    ActionEntry, ActionPage, ActionQuery, ActionSort, EVENT_FAMILIES, MAX_PAGE_SIZE,
};
use bilibi_authz::{authorize_key, effective_capabilities};
use bilibi_database::{Change, Db, Error, MutationBoundary, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy};
use bilibi_locale::Locales;
use bilibi_tenancy::{ClientIdentity, Scope, SelfReport};
use bilibi_ui::{
    choices,
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BadgeKind, BuiltinColumn, ColumnType, ColumnsOutcome, Filter, FilterToolbar, FlashMap,
    FlexRow, FlexTable, Html, ListColumns, ListSpec, ListState, PageCounts, Pagination, SortDir,
    TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
const ACTOR_TYPES: [&str; 8] = [
    "user",
    "agent",
    "guest",
    "console",
    "scheduler",
    "queue",
    "system",
    "unknown",
];
const FORBIDDEN: &str = "You do not have permission to manage audit logs.";
const NOT_UPDATED: &str = "Could not update retention status.";

/// Bilimbi's spec: `occurred_at` opens descending, every other column
/// ascending; the page-size key is `page_size`; diagnostics default to hidden.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    let with_any = |values: &[&'static str]| -> Vec<&'static str> {
        let mut all = vec![""];
        all.extend_from_slice(values);
        all
    };
    ListSpec {
        sortable: BTreeMap::from([
            ("occurred_at".into(), SortDir::Desc),
            ("actor_type".into(), SortDir::Asc),
            ("event".into(), SortDir::Asc),
            ("url".into(), SortDir::Asc),
            ("trace_id".into(), SortDir::Asc),
        ]),
        default_sort: Some("occurred_at".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "page_size".into(),
        filters: BTreeMap::from([
            (
                "actor_type".into(),
                Filter::one_of(&with_any(&ACTOR_TYPES), ""),
            ),
            (
                "event_family".into(),
                Filter::one_of(&with_any(&EVENT_FAMILIES), ""),
            ),
            (
                "result".into(),
                Filter::one_of(&["", "failure", "retained"], ""),
            ),
            (
                "diagnostics".into(),
                Filter::one_of(&["hide", "show"], "hide"),
            ),
        ]),
        ..ListSpec::default()
    }
    .shared()
    .expect("audit actions list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let sortable = |mut column: BuiltinColumn, key: &str| {
        column = column.sortable(key);
        column.sort_id = Some(format!("actions-sort-{key}"));
        column
    };
    let mut occurred = sortable(BuiltinColumn::new("occurred_at", "Occurred"), "occurred_at");
    occurred.kind = ColumnType::Datetime;
    vec![
        occurred,
        sortable(BuiltinColumn::new("actor_type", "Actor"), "actor_type"),
        sortable(BuiltinColumn::new("event", "Action"), "event"),
        BuiltinColumn::new("context", "Context"),
        BuiltinColumn::new("result", "Result"),
        sortable(BuiltinColumn::new("trace_id", "Trace"), "trace_id"),
        BuiltinColumn::new("agent_self_report", SelfReport::LABEL),
        BuiltinColumn::new("client_identity", ClientIdentity::LABEL),
        BuiltinColumn::new("retain", "Retain").right(),
    ]
}

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub boundary: Arc<MutationBoundary>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

/// What the last read returned, kept so a column-arrangement press redraws
/// without another query.
struct Shown {
    page: ActionPage,
    display: DisplayContext,
    can_manage: bool,
}

struct Actions {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    flash: Option<String>,
    shown: Option<Shown>,
}

fn parse(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

impl Actions {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse(query)?,
            columns: ListColumns::mount("actions-table", builtins())
                .map_err(|_| Error::Internal)?,
            flash: None,
            shown: None,
        })
    }

    fn query(&self) -> ActionQuery {
        let filter = |key: &str| self.list.filters[key].clone();
        ActionQuery {
            search: self.list.search.clone(),
            actor_type: filter("actor_type"),
            event_family: filter("event_family"),
            result: filter("result"),
            diagnostics: self.list.filters["diagnostics"] == "show",
            sort: self
                .list
                .sort_by
                .as_deref()
                .and_then(ActionSort::parse)
                .unwrap_or(ActionSort::OccurredAt),
            descending: self.list.sort_dir == Some(SortDir::Desc),
            page: self.list.page,
            page_size: self.list.page_size.min(MAX_PAGE_SIZE),
        }
    }

    /// Reads the page, the reader's clock and what the reader may manage. An
    /// out-of-range page (a stale link, a shrunk filter) lands on the last real
    /// page instead of an empty one with no pager.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        let display = self
            .pages
            .display
            .display(&**client, scope, None, locale)
            .await?;
        let can_manage = effective_capabilities(&**client, scope)
            .await?
            .contains(MANAGE);
        let mut page = bilibi_audit::list_actions(&**client, scope, &self.query()).await?;
        if self
            .list
            .clamp_to_last_page(page.total_entries.div_ceil(self.list.page_size), true)
        {
            page = bilibi_audit::list_actions(&**client, scope, &self.query()).await?;
        }
        // The rows changed: ListColumns forgets an unfinished add box.
        self.columns
            .load(&page.entries, |action| action.id.to_string());
        self.shown = Some(Shown {
            page,
            display,
            can_manage,
        });
        Ok(())
    }

    /// Flips one action's retention. `can_manage` only shows the control; this
    /// asks the capability again, so a grant removed after the page opened
    /// still refuses.
    async fn toggle_retain(&mut self, scope: &Scope, id: Option<&str>) -> Result<()> {
        let Some(id) = id
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
        else {
            self.flash = Some(NOT_UPDATED.into());
            return Ok(());
        };
        let mut client = self.pages.db.get().await?;
        match authorize_key(&**client, scope, MANAGE).await {
            Ok(()) => {}
            Err(Error::Forbidden) => {
                self.flash = Some(FORBIDDEN.into());
                return Ok(());
            }
            Err(error) => return Err(error),
        }
        let outcome: Result<bool> = async {
            let mut tx = self
                .pages
                .boundary
                .begin(
                    &mut client,
                    scope.mutation_context("audit.action.retention"),
                )
                .await?;
            match bilibi_audit::toggle_retained(tx.sql(), scope, id).await? {
                Some((before, after)) => {
                    tx.record(Change {
                        kind: "audit_action_retention".into(),
                        id,
                        old: json!({"is_retained": before}),
                        new: json!({"is_retained": after}),
                    })
                    .await?;
                    tx.commit().await?;
                    Ok(true)
                }
                None => {
                    tx.rollback().await?;
                    Ok(false)
                }
            }
        }
        .await;
        match outcome {
            Ok(true) => {}
            Ok(false) | Err(Error::Invalid | Error::NotFound | Error::Database(_)) => {
                self.flash = Some(NOT_UPDATED.into());
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn view(&mut self, scope: &Scope, requery: bool) -> Result<String> {
        if requery || self.shown.is_none() {
            self.load(scope).await?;
        }
        let shown = self.shown.as_ref().ok_or(Error::Internal)?;
        let mut flash = bilibi_ui::NO_FLASH;
        if let Some(message) = &self.flash {
            flash.error = message;
        }
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "actions-filters",
            vec![
                ToolbarControl::search(
                    "actions-search",
                    "search",
                    "Search actions",
                    &form["search"],
                    "Search event, URL, actor role, or trace...",
                ),
                ToolbarControl::select(
                    "actions-event-family",
                    "event_family",
                    "Filter by event family",
                    &form["event_family"],
                    choices(&[
                        ("All Events", ""),
                        ("HTTP", "http"),
                        ("Authentication", "auth"),
                        ("Console", "console"),
                        ("SQL console", "database"),
                        ("Queue", "queue"),
                        ("Domain", "domain"),
                    ]),
                ),
                ToolbarControl::select(
                    "actions-result",
                    "result",
                    "Filter by result",
                    &form["result"],
                    choices(&[
                        ("All Results", ""),
                        ("Failures", "failure"),
                        ("Retained", "retained"),
                    ]),
                ),
                ToolbarControl::select(
                    "actions-diagnostics",
                    "diagnostics",
                    "Diagnostic traffic",
                    &form["diagnostics"],
                    choices(&[("Hide Diagnostics", "hide"), ("Show Diagnostics", "show")]),
                ),
                ToolbarControl::select(
                    "actions-actor-type",
                    "actor_type",
                    "Filter by actor type",
                    &form["actor_type"],
                    choices(&[
                        ("All Actor Types", ""),
                        ("User", "user"),
                        ("Employee", "agent"),
                        ("Guest", "guest"),
                        ("Console", "console"),
                        ("Scheduler", "scheduler"),
                        ("Queue", "queue"),
                        ("System", "system"),
                        ("Unknown", "unknown"),
                    ]),
                ),
            ],
        ))?;
        let mut rows = Vec::new();
        for action in &shown.page.entries {
            rows.push(row(&shown.display, shown.can_manage, action)?);
        }
        let mut table = FlexTable::new("actions-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Audit actions";
        if shown.page.total_entries == 0 {
            let narrowed = !(self.list.search.is_empty()
                && self.list.filters["actor_type"].is_empty()
                && self.list.filters["event_family"].is_empty()
                && self.list.filters["result"].is_empty());
            table.empty = Some(TableEmpty::Content(Html::text(if narrowed {
                "No action logs match the current filters."
            } else {
                "No action logs match the current filters. Choose Show Diagnostics to include background traffic."
            })));
        }
        let pagination = render(Pagination::new(
            "actions-pagination",
            PageCounts::new(
                self.list.page,
                self.list.page_size,
                shown.page.total_entries,
            ),
            &PAGE_SIZES,
        ))?;
        ActionsView {
            flash,
            toolbar,
            hide_diagnostics: self.list.filters["diagnostics"] == "hide",
            table: render(table)?,
            pagination,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "actions.html")]
struct ActionsView<'a> {
    flash: FlashMap<'a>,
    toolbar: Html,
    hide_diagnostics: bool,
    table: Html,
    pagination: Html,
}

enum Cell {
    Occurred(Html),
    Actor {
        label: String,
        sub: String,
    },
    Event {
        summary: String,
        source: &'static str,
        diagnostic: bool,
        event: String,
    },
    Context(String),
    Result {
        kind: BadgeKind,
        label: String,
    },
    Trace(Option<String>),
    Retain {
        id: i64,
        retained: bool,
        can_manage: bool,
        icon: &'static str,
    },
}
#[derive(Template)]
#[template(path = "actions_cell.html")]
struct CellView {
    cell: Cell,
}

fn row(display: &DisplayContext, can_manage: bool, action: &ActionEntry) -> Result<FlexRow> {
    let Presentation {
        source,
        summary,
        context,
        result,
        variant,
        diagnostic,
    } = present::present(action);
    let cell = |cell: Cell| render(CellView { cell });
    let mut flex = FlexRow::new(action.id.to_string());
    flex.dom_id = Some(format!("actions-{}", action.id));
    for (column, html) in [
        (
            "occurred_at",
            cell(Cell::Occurred(present::time(
                display,
                &format!("action-occurred-{}", action.id),
                action.occurred_at,
                "",
            )?))?,
        ),
        (
            "actor_type",
            cell(Cell::Actor {
                label: present::actor_label(&action.into()),
                sub: present::actor_sub(&action.into()),
            })?,
        ),
        (
            "event",
            cell(Cell::Event {
                summary,
                source,
                diagnostic,
                event: action.event.clone(),
            })?,
        ),
        ("context", cell(Cell::Context(context))?),
        (
            "result",
            cell(Cell::Result {
                kind: variant,
                label: result,
            })?,
        ),
        (
            "trace_id",
            cell(Cell::Trace(
                action.trace_id.clone().filter(|trace| !trace.is_empty()),
            ))?,
        ),
        // The same muted mono text as the trace: a recorded label, not a link.
        (
            "client_identity",
            cell(Cell::Trace(action.client_identity.clone()))?,
        ),
        (
            "agent_self_report",
            cell(Cell::Trace(action.agent_self_report.clone()))?,
        ),
        (
            "retain",
            cell(Cell::Retain {
                id: action.id,
                retained: action.is_retained,
                can_manage,
                icon: if action.is_retained {
                    "retained"
                } else {
                    "retain"
                },
            })?,
        ),
    ] {
        flex.slots.insert(column.into(), html);
    }
    Ok(flex)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "actions").then_some("Audit Actions")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "actions" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = Actions::new(self.clone(), request.query)?
                .view(scope, true)
                .await?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "actions" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(Actions::new(self.clone(), request.query)?);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Actions {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut requery = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.list = parse(values)?,
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    match get("type") {
                        Some("filters") => self.list.apply_filters(
                            &params::form_values_checked(event, FILTERS_FORM, params::FIELD_LIMIT)
                                .map_err(Error::refused)?,
                        ),
                        Some("page") => self.list.put_page(get("page")),
                        Some("grid") => {
                            let values: HashMap<String, String> = event
                                .as_object()
                                .into_iter()
                                .flatten()
                                .filter_map(|(key, value)| {
                                    Some((key.clone(), value.as_str()?.to_owned()))
                                })
                                .collect();
                            match self.columns.handle(&values) {
                                ColumnsOutcome::Sort(key) => {
                                    if !self.list.next_sort(&key) {
                                        return Err(Error::Invalid);
                                    }
                                }
                                ColumnsOutcome::Update => requery = false,
                                ColumnsOutcome::Noop => {}
                            }
                        }
                        Some("toggle_retain") => {
                            self.toggle_retain(request.scope, get("id")).await?
                        }
                        Some("clear-flash") => {
                            if get("key") == Some("error") {
                                self.flash = None;
                            }
                            requery = false;
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.view(request.scope, requery).await?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
