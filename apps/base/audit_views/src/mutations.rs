//! `/audit/mutations` (`base/audit_views.pages`, operation `mutations`): the
//! tenant's data changes, newest first, ported from Bilimbi
//! `apps/base/audit/lib/audit/web/mutations_live.{ex,html.heex}` and the
//! `MutationDetails` component.
//!
//! The route guard checks `admin.audit.log.list` on every request and live
//! event. The values of a field the record's owner withholds from this reader
//! leave the entry before it is drawn (`bilibi_authz::fields::withheld_by_record_type`
//! over the field catalog's record types), so the list never shows a value the
//! record's own page would not.
//!
//! The table streams up to 300 rows. Rendering every old and new value inline
//! made each row about 15 KB in the source, so a row's details cell paints the
//! field summary and builds the diff only while the row is open; the values
//! stay on the server either way. The toggle is a disclosure button, not a
//! data-changing button: `aria-expanded` follows the open state, a second
//! activation closes it, and click-away is not a close, so a reader selecting
//! text in the diff keeps it.
use crate::{diff, present};
use askama::Template;
use bilibi_audit::{MutationEntry, MutationPage, MutationQuery, MutationSort, MAX_PAGE_SIZE};
use bilibi_database::{Db, Error, Result};
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
    params, BadgeKind, BuiltinColumn, ColumnType, ColumnsOutcome, Filter, FilterToolbar, FlexRow,
    FlexTable, Html, ListColumns, ListSpec, ListState, PageCounts, Pagination, Restricted, SortDir,
    TableEmpty, ToolbarControl,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    sync::{Arc, LazyLock},
};

const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];

/// Bilimbi's spec: `occurred_at` opens descending, every other column
/// ascending; the page-size key is `page_size`.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("occurred_at".into(), SortDir::Desc),
            ("actor_type".into(), SortDir::Asc),
            ("event".into(), SortDir::Asc),
            ("auditable_type".into(), SortDir::Asc),
            ("trace_id".into(), SortDir::Asc),
        ]),
        default_sort: Some("occurred_at".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "page_size".into(),
        filters: BTreeMap::from([(
            "event".into(),
            Filter::one_of(&["", "created", "updated", "deleted"], ""),
        )]),
        ..ListSpec::default()
    }
    .shared()
    .expect("audit mutations list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let sortable = |mut column: BuiltinColumn, key: &str| {
        column = column.sortable(key);
        column.sort_id = Some(format!("mutations-sort-{key}"));
        column
    };
    let mut occurred = sortable(BuiltinColumn::new("occurred_at", "Occurred"), "occurred_at");
    occurred.kind = ColumnType::Datetime;
    vec![
        occurred,
        sortable(BuiltinColumn::new("actor_type", "Actor"), "actor_type"),
        sortable(BuiltinColumn::new("event", "Event"), "event"),
        sortable(
            BuiltinColumn::new("auditable_type", "Subject"),
            "auditable_type",
        ),
        BuiltinColumn::new("details", "Details"),
        sortable(BuiltinColumn::new("trace_id", "Trace"), "trace_id"),
        BuiltinColumn::new("agent_self_report", SelfReport::LABEL),
        BuiltinColumn::new("client_identity", ClientIdentity::LABEL),
    ]
}

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

/// What the last read returned, kept so a column-arrangement press or a
/// details toggle redraws without another query.
struct Shown {
    page: MutationPage,
    display: DisplayContext,
}

pub(crate) struct Mutations {
    pages: Pages,
    list: ListState,
    columns: ListColumns,
    /// The rows whose diff is open.
    open: HashSet<i64>,
    shown: Option<Shown>,
}

fn parse(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// Reads the reader's withheld fields for the kinds on this page and takes
/// their values out of each entry, the one place a stored mutation becomes a
/// read model for a screen.
pub(crate) async fn withhold(
    client: &impl tokio_postgres::GenericClient,
    scope: &Scope,
    entries: &mut [MutationEntry],
) -> Result<()> {
    let kinds: Vec<&str> = entries
        .iter()
        .map(|entry| entry.auditable_type.as_str())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let withheld = bilibi_authz::fields::withheld_by_record_type(client, scope, &kinds).await?;
    if withheld.is_empty() {
        return Ok(());
    }
    for entry in entries {
        if let Some(fields) = withheld.get(&entry.auditable_type) {
            entry.withhold(fields);
        }
    }
    Ok(())
}

impl Mutations {
    pub(crate) fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            list: parse(query)?,
            columns: ListColumns::mount("mutations-table", builtins())
                .map_err(|_| Error::Internal)?,
            open: HashSet::new(),
            shown: None,
        })
    }

    fn query(&self) -> MutationQuery {
        MutationQuery {
            search: self.list.search.clone(),
            event: self.list.filters["event"].clone(),
            sort: self
                .list
                .sort_by
                .as_deref()
                .and_then(MutationSort::parse)
                .unwrap_or(MutationSort::OccurredAt),
            descending: self.list.sort_dir == Some(SortDir::Desc),
            page: self.list.page,
            page_size: self.list.page_size.min(MAX_PAGE_SIZE),
        }
    }

    /// Reads the page and the reader's clock. An out-of-range page lands on
    /// the last real page instead of an empty one with no pager.
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
        let mut page = bilibi_audit::list_mutations(&**client, scope, &self.query()).await?;
        if self
            .list
            .clamp_to_last_page(page.total_entries.div_ceil(self.list.page_size), true)
        {
            page = bilibi_audit::list_mutations(&**client, scope, &self.query()).await?;
        }
        withhold(&**client, scope, &mut page.entries).await?;
        // The rows changed: ListColumns forgets an unfinished add box, and an
        // open diff belongs to a row no longer shown.
        self.columns
            .load(&page.entries, |mutation| mutation.id.to_string());
        let ids: HashSet<i64> = page.entries.iter().map(|entry| entry.id).collect();
        self.open.retain(|id| ids.contains(id));
        self.shown = Some(Shown { page, display });
        Ok(())
    }

    pub(crate) async fn view(&mut self, scope: &Scope, requery: bool) -> Result<String> {
        if requery || self.shown.is_none() {
            self.load(scope).await?;
        }
        let shown = self.shown.as_ref().ok_or(Error::Internal)?;
        let form = self.list.filters_form();
        let toolbar = render(FilterToolbar::new(
            "mutations-filters",
            vec![
                ToolbarControl::search(
                    "mutations-search",
                    "search",
                    "Search mutations",
                    &form["search"],
                    "Search actor, model, subject, event, or trace...",
                ),
                ToolbarControl::select(
                    "mutations-event",
                    "event",
                    "Filter by event",
                    &form["event"],
                    choices(&[
                        ("All Events", ""),
                        ("Created", "created"),
                        ("Updated", "updated"),
                        ("Deleted", "deleted"),
                    ]),
                ),
            ],
        ))?;
        let mut rows = Vec::new();
        for mutation in &shown.page.entries {
            rows.push(row(
                &shown.display,
                self.open.contains(&mutation.id),
                mutation,
            )?);
        }
        let mut table = FlexTable::new("mutations-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Data mutations";
        if shown.page.total_entries == 0 {
            let narrowed = !(self.list.search.is_empty() && self.list.filters["event"].is_empty());
            table.empty = Some(TableEmpty::Content(Html::text(if narrowed {
                "No mutation logs match the current filters."
            } else {
                "No mutation logs found."
            })));
        }
        let pagination = render(Pagination::new(
            "mutations-pagination",
            PageCounts::new(
                self.list.page,
                self.list.page_size,
                shown.page.total_entries,
            ),
            &PAGE_SIZES,
        ))?;
        MutationsView {
            toolbar,
            table: render(table)?,
            pagination,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

#[derive(Template)]
#[template(path = "mutations.html")]
struct MutationsView {
    toolbar: Html,
    table: Html,
    pagination: Html,
}

/// One rendered field of an open diff.
pub(crate) struct DiffLine {
    pub field: String,
    pub sensitive: bool,
    pub withheld: bool,
    pub restricted: Html,
    pub old: Html,
    pub new: Html,
}

/// The diff rows of one entry with each side rendered; `prefix` names the
/// elements (`mutation-7` on the list, `<panel>-entry-7` in a history panel).
pub(crate) fn diff_lines(
    display: &DisplayContext,
    prefix: &str,
    entry: &MutationEntry,
    old_class: &str,
    new_class: &str,
) -> Result<Vec<DiffLine>> {
    let mut lines = Vec::new();
    for row in diff::rows(entry) {
        let (old_class, new_class) = if row.sensitive {
            ("", "")
        } else {
            (old_class, new_class)
        };
        lines.push(DiffLine {
            restricted: if row.withheld {
                render(Restricted::new(format!(
                    "{prefix}-{}-restricted",
                    row.field
                )))?
            } else {
                Html::EMPTY
            },
            old: diff::render_value(
                display,
                &format!("{prefix}-{}-old", row.field),
                &row.old,
                old_class,
            )?,
            new: diff::render_value(
                display,
                &format!("{prefix}-{}-new", row.field),
                &row.new,
                new_class,
            )?,
            field: row.field,
            sensitive: row.sensitive,
            withheld: row.withheld,
        });
    }
    Ok(lines)
}

enum Cell {
    Occurred(Html),
    Actor {
        label: String,
        sub: String,
    },
    Event {
        kind: BadgeKind,
        label: String,
    },
    Subject {
        label: String,
        sub: String,
    },
    Details {
        id: String,
        mutation: i64,
        summary: String,
        open: bool,
        lines: Vec<DiffLine>,
    },
    Trace(Option<String>),
}
#[derive(Template)]
#[template(path = "mutations_cell.html")]
struct CellView {
    cell: Cell,
}

/// Bilimbi `event_badge/1` on the list: `:info` and `:default` are kinds its
/// badge never colours, so they are the plain pill here.
pub(crate) fn event_badge(event: &str) -> (BadgeKind, String) {
    match event {
        "created" => (BadgeKind::Success, "Created".into()),
        "updated" => (BadgeKind::Plain, "Updated".into()),
        "deleted" => (BadgeKind::Danger, "Deleted".into()),
        other => (BadgeKind::Plain, capitalize(other)),
    }
}

pub(crate) fn capitalize(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| first.to_uppercase().chain(chars).collect())
        .unwrap_or_default()
}

/// The last segment of a PHP or Elixir module name (`App\Models\Company`,
/// `Bilimbi.Core.Company`), or the kind as recorded.
fn short_type(kind: &str) -> &str {
    kind.rsplit(['\\', '.']).next().unwrap_or(kind)
}

fn subject_label(mutation: &MutationEntry) -> String {
    if let Some(identifier) = mutation
        .subject_identifier
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        return identifier.to_owned();
    }
    if let Some(name) = mutation
        .subject_name
        .as_deref()
        .filter(|text| !text.is_empty())
    {
        return name.to_owned();
    }
    short_type(&mutation.auditable_type).to_owned()
}

fn subject_sub(mutation: &MutationEntry) -> String {
    format!(
        "{} #{}",
        short_type(&mutation.auditable_type),
        mutation.auditable_id
    )
}

fn row(display: &DisplayContext, open: bool, mutation: &MutationEntry) -> Result<FlexRow> {
    let cell = |cell: Cell| render(CellView { cell });
    let mut flex = FlexRow::new(mutation.id.to_string());
    flex.dom_id = Some(format!("mutations-{}", mutation.id));
    let (kind, label) = event_badge(&mutation.event);
    let fields = diff::changed_fields(mutation);
    let details_id = format!("mutation-{}-details", mutation.id);
    let lines = if open && !fields.is_empty() {
        // The list colours the `<code>` wrappers (its template); the values
        // themselves carry no class, as the source's `diff_value` gets none.
        diff_lines(
            display,
            &format!("mutation-{}", mutation.id),
            mutation,
            "",
            "",
        )?
    } else {
        Vec::new()
    };
    for (column, html) in [
        (
            "occurred_at",
            cell(Cell::Occurred(present::time(
                display,
                &format!("mutation-occurred-{}", mutation.id),
                mutation.occurred_at,
                "",
            )?))?,
        ),
        (
            "actor_type",
            cell(Cell::Actor {
                label: present::actor_label(&mutation.into()),
                sub: present::actor_sub(&mutation.into()),
            })?,
        ),
        ("event", cell(Cell::Event { kind, label })?),
        (
            "auditable_type",
            cell(Cell::Subject {
                label: subject_label(mutation),
                sub: subject_sub(mutation),
            })?,
        ),
        (
            "details",
            cell(Cell::Details {
                id: details_id,
                mutation: mutation.id,
                summary: diff::summary_from_fields(&fields),
                open: open && !fields.is_empty(),
                lines,
            })?,
        ),
        (
            "trace_id",
            cell(Cell::Trace(
                mutation.trace_id.clone().filter(|trace| !trace.is_empty()),
            ))?,
        ),
        // The same muted mono text as the trace: a recorded label, not a link.
        (
            "client_identity",
            cell(Cell::Trace(mutation.client_identity.clone()))?,
        ),
        (
            "agent_self_report",
            cell(Cell::Trace(mutation.agent_self_report.clone()))?,
        ),
    ] {
        flex.slots.insert(column.into(), html);
    }
    Ok(flex)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "mutations").then_some("Data Mutations")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "mutations" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = Mutations::new(self.clone(), request.query)?
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
            if request.operation != "mutations" {
                return Err(Error::NotFound);
            }
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(Mutations::new(self.clone(), request.query)?);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Mutations {
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
                        Some("toggle_details") => {
                            let id = get("id")
                                .and_then(|id| id.parse::<i64>().ok())
                                .filter(|id| *id > 0)
                                .ok_or(Error::Invalid)?;
                            if !self.open.remove(&id) {
                                self.open.insert(id);
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
