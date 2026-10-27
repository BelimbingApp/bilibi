//! Live PageColumns walkthrough: a list page that hosts walked columns. Only
//! run against a disposable migrated PostgreSQL with
//! BILIBI_GRID_DEMO=disposable, plus DATABASE_URL, APP_ORIGIN and BIND_ADDR.
//! No test runner or mocks.
//!
//! It replaces the non-business reference fixture (`bilibi_grid::reference`)
//! and serves `/g4-page-columns`: the tenant's reference items as the page's
//! own list (search, category filter, sort and pagination through `ListState`)
//! with three built-in columns the page draws itself, hosting
//! `bilibi_grid::PageColumns`. Open the notch to add walked columns from the
//! catalog (`group.name`, `movements:count`, `movements.delta:sum`…), switch
//! their lenses, reorder, remove and change the row height; every change is
//! remembered for the signed-in account and this page in the
//! `ui.grid.page_columns` setting, and the address carries it. Reload or come
//! back without an address and the page opens as it was left; paste an address
//! into another account's browser and it shows the sender's view without
//! changing that account's memory. Reset returns the page's own columns and
//! forgets what was kept. The restriction control restricts `unit_cost` for the roles
//! the signed-in account holds, which prunes that column from the table, the
//! address and the suggestions until the restriction is lifted. Sign in as the
//! fixture manager (grid.reference.view) after the example reconciles roles.
use bilibi_base::{
    authz::grants::Grants,
    datetime::{DisplayContext, Mode},
    grid::{self, Outcome, PageColumns, PageViews, PAGE_VIEWS},
};
use bilibi_database::{Db, Error, Result};
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
    ColumnType, EmptyState, Filter, FilterToolbar, FlexRow, Header, Html, ListSpec, ListState,
    Pagination, SortDir, TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const ROOT: &str = "reference_items";
const TABLE_ID: &str = "g4-items";
const PAGE_SIZES: [u64; 2] = [10, 25];

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("quantity".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: PAGE_SIZES[0],
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([(
            "category".into(),
            Filter::one_of(&["all", "tools", "parts", "supplies"], "all"),
        )]),
        omit_blank: vec!["search".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("g4 list spec")
});

/// The columns the page draws itself. `category` is also a catalog field of
/// the root table, so the catalog never offers it a second time.
fn builtins() -> Vec<BuiltinColumn> {
    let mut quantity = BuiltinColumn::new("quantity", "Quantity")
        .sortable("quantity")
        .right();
    quantity.kind = ColumnType::Integer;
    let mut category = BuiltinColumn::new("category", "Category");
    category.kind = ColumnType::Enum;
    vec![
        BuiltinColumn::new("name", "Name").sortable("name"),
        category,
        quantity,
    ]
}

struct Item {
    id: i64,
    name: String,
    category: String,
    quantity: i64,
}

struct Pages {
    db: Db,
    views: Arc<PageViews>,
}

struct State {
    db: Db,
    list: ListState,
    columns: PageColumns,
    mode: Mode,
    /// The page of items the last load showed, by key.
    items: Vec<Item>,
    counts: bilibi_ui::PageCounts,
    restricted: bool,
}

fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

fn render(fragment: impl askama::Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

fn badge(category: &str) -> BadgeKind {
    match category {
        "tools" => BadgeKind::Warning,
        "parts" => BadgeKind::Success,
        _ => BadgeKind::Neutral,
    }
}

impl State {
    async fn new(pages: &Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            db: pages.db.clone(),
            list: parse(query)?,
            columns: PageColumns::mount(pages.views.clone(), ROOT, builtins())
                .map_err(|_| Error::Internal)?,
            mode: Mode::Company,
            items: Vec::new(),
            counts: bilibi_ui::PageCounts::new(1, PAGE_SIZES[0], 0),
            restricted: false,
        })
    }

    fn display(&self) -> DisplayContext {
        DisplayContext {
            mode: self.mode,
            timezone: "Asia/Kuala_Lumpur".into(),
            ..DisplayContext::anonymous()
        }
    }

    /// The owner's own list: the tenant's items searched, filtered, sorted
    /// and paged here. Grid only projects the walked columns for this page.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        let rows = client
            .query(
                "SELECT id, name, category, quantity FROM base_grid_ref_items WHERE tenant_id = $1",
                &[&scope.tenant()],
            )
            .await?;
        self.restricted = client
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM field_restrictions r JOIN principal_roles p ON p.tenant_id = r.tenant_id AND p.role_id = r.role_id WHERE p.tenant_id = $1 AND p.user_id = $2 AND r.table_id = 'reference_items' AND r.field_id = 'unit_cost')",
                &[&scope.tenant(), &scope.user()],
            )
            .await?
            .get(0);
        drop(client);
        let needle = self.list.search.trim().to_lowercase();
        let category = self.list.filters["category"].as_str();
        let mut items: Vec<Item> = rows
            .iter()
            .map(|row| Item {
                id: row.get(0),
                name: row.get(1),
                category: row.get(2),
                quantity: i64::from(row.get::<_, i32>(3)),
            })
            .filter(|item| category == "all" || item.category == category)
            .filter(|item| needle.is_empty() || item.name.to_lowercase().contains(&needle))
            .collect();
        items.sort_by(|a, b| {
            let order = match self.list.sort_by.as_deref() {
                Some("quantity") => a.quantity.cmp(&b.quantity),
                _ => a.name.cmp(&b.name),
            }
            .then(a.id.cmp(&b.id));
            if self.list.sort_dir == Some(SortDir::Desc) {
                order.reverse()
            } else {
                order
            }
        });
        let page = self.list.paginate(&items);
        self.counts = page.counts;
        let keys: Vec<Value> = page.entries.iter().map(|item| json!(item.id)).collect();
        let offset = self.list.offset() as usize;
        let shown = keys.len();
        self.columns.load(scope, &keys, &self.display()).await?;
        self.items = items.drain(offset..offset + shown).collect();
        Ok(())
    }

    fn body(&self) -> Result<String> {
        let mut html = render(Header {
            title: Html::text("Reference items"),
            subtitle: "Search, filter and sort items, and add columns from linked records.",
            ..Header::default()
        })?;
        html.push(&self.controls()?);
        let form = self.list.filters_form();
        html.push(&render(FilterToolbar::new(
            "g4-filters",
            vec![
                ToolbarControl::search(
                    "g4-search",
                    "search",
                    "Search items",
                    &form["search"],
                    "Search by name...",
                ),
                ToolbarControl::select(
                    "g4-category",
                    "category",
                    "Category",
                    &form["category"],
                    choices(&[
                        ("All categories", "all"),
                        ("Tools", "tools"),
                        ("Parts", "parts"),
                        ("Supplies", "supplies"),
                    ]),
                ),
            ],
        ))?);
        let mut rows = Vec::new();
        for row in &self.columns.rows {
            let Some(item) = self
                .items
                .iter()
                .find(|item| item.id.to_string() == row.key)
            else {
                continue;
            };
            let mut row: FlexRow = row.clone();
            row.dom_id = Some(format!("g4-item-{}", item.id));
            row.slots.insert(
                "name".into(),
                Html::raw(format!(
                    "<span class=\"font-medium text-ink-strong\">{}</span>",
                    Html::text(&item.name)
                )),
            );
            row.slots.insert(
                "category".into(),
                render(Badge::status(badge(&item.category), &item.category))?,
            );
            row.slots.insert(
                "quantity".into(),
                Html::raw(format!(
                    "<span class=\"tabular-nums\">{}</span>",
                    item.quantity
                )),
            );
            rows.push(row);
        }
        let mut table = self
            .columns
            .flex_table(TABLE_ID, rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.caption = "Reference items";
        table.framed = false;
        if self.items.is_empty() {
            let clear = render(Button {
                id: "g4-clear",
                button_type: "button",
                event: "clear",
                body: Html::text("Clear filters"),
                ..Button::default()
            })?;
            table.empty = Some(TableEmpty::State(
                EmptyState::missing(
                    "No items match",
                    "Clear the search and category to see every item.",
                )
                .action(clear),
            ));
        }
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "g4-pagination",
            self.counts,
            &PAGE_SIZES,
        ))?);
        html.push(&render(Card {
            id: "g4-items-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        render(bilibi_ui::Page {
            id: "g4-page-columns",
            body: Html::raw(format!("<div data-shared-ui>{html}</div>")),
            ..bilibi_ui::Page::default()
        })
        .map(|html| html.as_str().to_owned())
    }

    /// The walkthrough's own controls: the timestamp clock and the restriction
    /// of `unit_cost` for the signed-in account's roles.
    fn controls(&self) -> Result<Html> {
        let pressed = |on: bool| {
            if on {
                "border-selection-line bg-brand-surface text-brand-ink"
            } else {
                "border-line bg-surface text-ink-muted hover:bg-surface-sunken hover:text-ink"
            }
        };
        let clock = |mode: Mode, label: &str| {
            format!(
                r#"<button type="button" id="clock-{value}" data-event="clock" data-mode="{value}" aria-pressed="{on}" class="inline-flex h-6 items-center rounded-md border px-1.5 text-xs transition {class}">{label}</button>"#,
                value = mode.as_str(),
                on = self.mode == mode,
                class = pressed(self.mode == mode),
            )
        };
        let restrict = |on: bool, label: &str| {
            format!(
                r#"<button type="button" id="restrict-{value}" data-event="restrict" data-on="{value}" aria-pressed="{pressed}" class="inline-flex h-6 items-center rounded-md border px-1.5 text-xs transition {class}">{label}</button>"#,
                value = on,
                pressed = self.restricted == on,
                class = pressed(self.restricted == on),
            )
        };
        Ok(Html::raw(format!(
            r#"<div id="g4-controls" class="mb-4 flex flex-wrap items-center gap-x-6 gap-y-2 text-xs text-ink-muted"><div id="clock-switch" role="group" aria-label="Timestamp clock" class="flex items-center gap-1"><span class="mr-1">Timestamps in</span>{}{}{}</div><div id="restriction-switch" role="group" aria-label="Unit cost restriction" class="flex items-center gap-1"><span class="mr-1">Unit cost for my roles</span>{}{}</div></div>"#,
            clock(Mode::Company, "Company (Kuala Lumpur)"),
            clock(Mode::Utc, "UTC"),
            clock(Mode::Local, "Local"),
            restrict(false, "Readable"),
            restrict(true, "Restricted"),
        )))
    }

    /// Restricts or lifts `reference_items.unit_cost` for every role the
    /// signed-in account holds: what an operator does on the Field
    /// Restrictions screen, done here so the pruning can be watched live.
    async fn restrict(&self, scope: &Scope, on: bool) -> Result<()> {
        let client = self.db.get().await?;
        if on {
            client.execute(
                "INSERT INTO field_restrictions(tenant_id, role_id, table_id, field_id) SELECT tenant_id, role_id, 'reference_items', 'unit_cost' FROM principal_roles WHERE tenant_id = $1 AND user_id = $2 ON CONFLICT DO NOTHING",
                &[&scope.tenant(), &scope.user()],
            ).await?;
        } else {
            client.execute(
                "DELETE FROM field_restrictions r USING principal_roles p WHERE p.tenant_id = r.tenant_id AND p.role_id = r.role_id AND p.tenant_id = $1 AND p.user_id = $2 AND r.table_id = 'reference_items' AND r.field_id = 'unit_cost'",
                &[&scope.tenant(), &scope.user()],
            ).await?;
        }
        Ok(())
    }

    fn query(&self) -> HashMap<String, String> {
        let mut query = self.list.to_params();
        query.extend(self.columns.params());
        query
    }
}

/// The event payload's values as strings, the shape every flex op is read in.
fn values(event: &Value) -> HashMap<String, String> {
    event
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, value)| {
            let text = match value {
                Value::String(text) => text.clone(),
                Value::Number(number) => number.to_string(),
                Value::Bool(flag) => flag.to_string(),
                _ => return None,
            };
            Some((key.clone(), text))
        })
        .collect()
}

impl Page<Scope, Error> for Pages {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Forbidden)?;
            let mut state = State::new(self, request.query).await?;
            state.columns.from_params(scope, request.query).await?;
            state.load(scope).await?;
            Ok(PageBody {
                html: state.body()?,
                live: true,
                title: None,
            })
        })
    }

    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Forbidden)?;
            let mut state = State::new(self, request.query).await?;
            state.columns.from_params(scope, request.query).await?;
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(state);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope;
            let mut reload = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list = parse(values)?;
                    self.columns.from_params(scope, values).await?;
                }
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    match get("type") {
                        Some("grid") => match self.columns.handle(scope, &values(event)).await? {
                            Outcome::Sort(key) => {
                                if !self.list.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                            }
                            // Only the add box changed: the rows stay.
                            Outcome::Update => reload = false,
                            Outcome::Patch | Outcome::Noop => {}
                        },
                        Some("filters") => self.list.apply_filters(
                            &params::form_values(event, FILTERS_FORM).map_err(Error::refused)?,
                        ),
                        Some("page") => self.list.put_page(get("page")),
                        Some("clear") => self.list.apply_filters(&HashMap::from([
                            ("search".into(), String::new()),
                            ("category".into(), "all".into()),
                        ])),
                        Some("clock") => {
                            if let Some(mode) = get("mode").and_then(Mode::parse) {
                                self.mode = mode;
                            }
                        }
                        Some("restrict") => self.restrict(scope, get("on") == Some("true")).await?,
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            if reload {
                self.load(scope).await?;
            }
            Ok(LiveBody {
                html: self.body()?,
                query: self.query(),
                rows: self.items.len(),
                ..LiveBody::default()
            })
        })
    }
}

inventory::submit! {
    Module {id: "extensions/g4_page_columns", layer: Layer::Extensions,
        dependencies: &["base/ui", "base/grid", "base/database"], migrations: &[], services: &[Service {
            id: "extensions/g4_page_columns.pages",
            factory: |services| Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                views: services.get::<PageViews>(PAGE_VIEWS)?,
            })))),
        }],
    }
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/g4_page_columns.page", Provenance {module:"extensions/g4_page_columns", source:file!(), line:line!()}, &Route {
        method:Method::Get, path:"/g4-page-columns", service:"extensions/g4_page_columns.pages", operation:"page", access:Access::SignedIn, framed:true,
    })
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    if std::env::var("BILIBI_GRID_DEMO").as_deref() != Ok("disposable") {
        return Err("set BILIBI_GRID_DEMO=disposable on a disposable migrated database".into());
    }
    let db = bilibi_database::pool(&std::env::var("DATABASE_URL")?)?;
    bilibi_walkthrough_fixture::install(&db).await?;
    let origin = std::env::var("APP_ORIGIN")?;
    let bind = std::env::var("BIND_ADDR")?;
    let app = bilibi_web::App::new(db.clone(), &origin)?;
    app.services
        .get::<Grants>("base/authz.grants")?
        .reconcile()
        .await?;
    db.get()
        .await?
        .batch_execute(grid::reference::FIXTURE)
        .await?;
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Page columns at {origin}/g4-page-columns");
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
