//! Manual list specimen for the shared list surface, separate from product routes.
//! Run with an isolated, migrated DATABASE_URL, APP_ORIGIN and BIND_ADDR.
//!
//! `/list-reference` lists 137 deterministic vendors held in memory through
//! `ListState`, `FilterToolbar`, `FlexTable`, `Pagination` and `EmptyState`, with
//! the query keys Bilimbi's Companies list uses (`search`, `status_filter`,
//! `sort_by`, `sort_dir`, `page`, `per_page`). Search, filter, sort, page and
//! page size change the URL; Back and reload restore the same rows, and a forged
//! query falls back to defaults. `/list-reference/forbidden` is the permission
//! empty state.
//! Mirrors Bilimbi UI DesignLibraryLive's built-in list hosting.
use bilibi_database::{Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Contribution, Method, Route, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_security::Scope;
use bilibi_ui::page::{
    LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
    PageFuture, PageRequest, PageService,
};
use bilibi_ui::{
    choices, list_state::FILTERS_FORM, params, Badge, BadgeKind, BuiltinColumn, Button, Card,
    EmptyState, Filter, FilterToolbar, FlexRow, FlexTable, Header, Html, ListColumns, ListSpec,
    ListState, Pagination, SortDir, TableEmpty, ToolbarControl,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
const STATUSES: [&str; 4] = ["active", "suspended", "pending", "archived"];

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("status".into(), SortDir::Asc),
            ("balance".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "per_page".into(),
        invalid_page_size: bilibi_ui::list_state::InvalidPageSize::Default,
        filters: BTreeMap::from([(
            "status_filter".into(),
            Filter::one_of(
                &["all", "active", "suspended", "pending", "archived"],
                "all",
            ),
        )]),
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("list reference spec")
});

struct Vendor {
    id: u64,
    name: String,
    code: String,
    status: &'static str,
    balance: i64,
}

static VENDORS: LazyLock<Vec<Vendor>> = LazyLock::new(|| {
    let first = [
        "Amber", "Banyan", "Cobalt", "Delta", "Ember", "Fern", "Granite", "Harbor", "Iris",
        "Juniper", "Kestrel", "Lumen", "Meridian",
    ];
    let second = [
        "Freight",
        "Supplies",
        "Textiles",
        "Foods",
        "Logistics",
        "Metals",
        "Paper",
        "Tools",
        "Labs",
        "Works",
        "Trading",
    ];
    (1..=137u64)
        .map(|id| {
            let n = id as usize;
            Vendor {
                id,
                name: format!(
                    "{} {}",
                    first[(n * 7) % first.len()],
                    second[n % second.len()]
                ),
                code: format!("V-{id:04}"),
                status: STATUSES[(n * 5 + n / 3) % STATUSES.len()],
                balance: ((id * 7919) % 100_000) as i64 * 13 - 250_000,
            }
        })
        .collect()
});

struct Reference;
struct ReferenceState {
    state: ListState,
    columns: ListColumns,
}

fn columns() -> ListColumns {
    ListColumns::mount(
        "vendors",
        vec![
            BuiltinColumn::new("name", "Name").sortable("name"),
            BuiltinColumn::new("code", "Code"),
            BuiltinColumn::new("status", "Status").sortable("status"),
            BuiltinColumn::new("balance", "Balance")
                .sortable("balance")
                .right(),
        ],
    )
    .expect("vendor columns")
}

fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

fn filtered(state: &ListState) -> Vec<&'static Vendor> {
    let needle = state.search.trim().to_lowercase();
    let status = state.filters["status_filter"].as_str();
    let mut rows: Vec<&Vendor> = VENDORS
        .iter()
        .filter(|v| status == "all" || v.status == status)
        .filter(|v| {
            needle.is_empty()
                || v.name.to_lowercase().contains(&needle)
                || v.code.to_lowercase().contains(&needle)
        })
        .collect();
    rows.sort_by(|a, b| {
        let order = match state.sort_by.as_deref() {
            Some("status") => a.status.cmp(b.status),
            Some("balance") => a.balance.cmp(&b.balance),
            _ => a.name.cmp(&b.name),
        }
        .then(a.id.cmp(&b.id));
        if state.sort_dir == Some(SortDir::Desc) {
            order.reverse()
        } else {
            order
        }
    });
    rows
}

fn render(fragment: impl askama::Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

fn money(cents: i64) -> String {
    let sign = if cents < 0 { "-" } else { "" };
    let cents = cents.unsigned_abs();
    format!("{sign}{}.{:02}", cents / 100, cents % 100)
}

fn badge(status: &str) -> BadgeKind {
    match status {
        "active" => BadgeKind::Success,
        "suspended" => BadgeKind::Danger,
        "pending" => BadgeKind::Warning,
        _ => BadgeKind::Neutral,
    }
}

fn body(state: &mut ListState, columns: &ListColumns) -> Result<String> {
    let rows = filtered(state);
    let page = state.paginate(&rows);
    let header = render(Header {
        title: Html::text("Vendors"),
        subtitle: "",
        ..Header::default()
    })?;
    let form = state.filters_form();
    let toolbar = render(FilterToolbar::new(
        "vendors-filters",
        vec![
            ToolbarControl::search(
                "vendors-search",
                "search",
                "Search vendors",
                &form["search"],
                "Search by name or code...",
            ),
            ToolbarControl::select(
                "vendors-status-filter",
                "status_filter",
                "Status filter",
                &form["status_filter"],
                choices(&[
                    ("All statuses", "all"),
                    ("Active", "active"),
                    ("Suspended", "suspended"),
                    ("Pending", "pending"),
                    ("Archived", "archived"),
                ]),
            ),
        ],
    ))?;
    let mut table = FlexTable::new("vendors", columns.flex_columns(), vec![])
        .sorted(state.sort_by.as_deref(), state.sort_dir);
    table.event = "vendor-columns";
    table.sort_event = "sort";
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.caption = "Vendors";
    table.framed = false;
    for vendor in page.entries {
        table.rows.push(FlexRow {
            key: vendor.id.to_string(),
            dom_id: Some(format!("vendors-{}", vendor.id)),
            slots: ["name", "code", "status", "balance"]
                .into_iter()
                .map(str::to_owned)
                .zip(vec![
                    Html::raw(format!(
                        "<span class=\"font-medium text-ink-strong\">{}</span>",
                        Html::text(&vendor.name)
                    )),
                    Html::raw(format!(
                        "<code class=\"text-xs font-medium tabular-nums\">{}</code>",
                        Html::text(&vendor.code)
                    )),
                    render(Badge::status(badge(vendor.status), vendor.status))?,
                    Html::raw(format!(
                        "<span class=\"tabular-nums\">{}</span>",
                        money(vendor.balance)
                    )),
                ])
                .collect(),
            actions: Html::EMPTY,
            ..FlexRow::default()
        });
    }
    if page.entries.is_empty() {
        let clear = render(Button {
            id: "vendors-clear",
            button_type: "button",
            event: "clear",
            body: Html::text("Clear filters"),
            ..Button::default()
        })?;
        let title = if state.search.is_empty() {
            "No vendors match this status".to_owned()
        } else {
            format!("No vendors match “{}”", state.search)
        };
        table.empty = Some(TableEmpty::State(
            EmptyState::missing(title, "Clear the search and status to see every vendor.")
                .action(clear),
        ));
    }
    let counts = page.counts;
    let mut card = render(table)?;
    card.push(&render(Pagination::new(
        "vendors-pagination",
        counts,
        &PAGE_SIZES,
    ))?);
    let mut html = header;
    html.push(&toolbar);
    html.push(&render(Card {
        id: "vendors-card",
        inner_class: "p-0",
        body: card,
        ..Card::default()
    })?);
    page_html(html)
}

fn page_html(body: Html) -> Result<String> {
    render(bilibi_ui::Page {
        id: "list-reference",
        body,
        ..bilibi_ui::Page::default()
    })
    .map(|html| html.as_str().to_owned())
}

fn forbidden() -> Result<String> {
    let header = render(Header {
        title: Html::text("Vendors"),
        ..Header::default()
    })?;
    let mut html = header;
    html.push(&render(Card {
        id: "vendors-card",
        inner_class: "px-2 py-8",
        body: render(EmptyState::forbidden("view vendors").id("vendors-forbidden"))?,
        ..Card::default()
    })?);
    page_html(html)
}

impl Page<Scope, Error> for Reference {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let html = match request.operation {
                "forbidden" => forbidden()?,
                _ => body(&mut parse(request.query)?, &columns())?,
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
            if request.operation != "list" {
                return Ok(None);
            }
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(ReferenceState {
                state: parse(request.query)?,
                columns: columns(),
            });
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for ReferenceState {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => {
                    let get = |key: &str| value.get(key).and_then(|v| v.as_str());
                    match get("type") {
                        Some("vendor-columns") => {
                            self.columns.handle(
                                &params::string_values_checked(value, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            );
                        }
                        Some("filters") => self.state.apply_filters(
                            &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                                .map_err(Error::refused)?,
                        ),
                        Some("sort") => {
                            if !self.state.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("page") => self.state.put_page(get("page")),
                        Some("clear") => self.state.apply_filters(&HashMap::from([
                            ("search".into(), String::new()),
                            ("status_filter".into(), "all".into()),
                        ])),
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = body(&mut self.state, &self.columns)?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: 0,
                ..LiveBody::default()
            })
        })
    }
}

inventory::submit! {
    Module {id: "extensions/list_reference", layer: Layer::Extensions,
        dependencies: &["base/ui"], migrations: &[], services: &[Service {
            id: "extensions/list_reference.pages",
            factory: |_| Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Reference)))),
        }],
    }
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/list_reference.list", Provenance {module:"extensions/list_reference", source:file!(), line:line!()}, &Route {
        method:Method::Get, path:"/list-reference", service:"extensions/list_reference.pages", operation:"list", access:Access::SignedIn, framed:true,
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/list_reference.forbidden", Provenance {module:"extensions/list_reference", source:file!(), line:line!()}, &Route {
        method:Method::Get, path:"/list-reference/forbidden", service:"extensions/list_reference.pages", operation:"forbidden", access:Access::SignedIn, framed:true,
    })
}
#[tokio::main]
async fn main() -> Result<()> {
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL").map_err(|_| Error::Invalid)?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN").map_err(|_| Error::Invalid)?;
    let bind = std::env::var("BIND_ADDR").map_err(|_| Error::Invalid)?;
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|_| Error::Internal)?;
    println!("List reference listening at {origin}");
    axum::serve(
        listener,
        bilibi_web::router(bilibi_web::App::new(db, &origin)?),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
    .map_err(|_| Error::Internal)
}
