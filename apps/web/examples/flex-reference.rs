//! Manual specimen for the flexible table, separate from product routes.
//! Run with an isolated, migrated DATABASE_URL, APP_ORIGIN and BIND_ADDR.
//!
//! `/flex-reference` runs `FlexTable` live in three hosts:
//!
//! * The Design Library sample (`bilibi_ui::flex_sample`): sixty example
//!   companies with walked columns, rollups that expand in place, bar and band
//!   lenses, and the add box offering three held-back columns. Every op of the
//!   one `sample-flex` event changes what it shows; Reset restores it.
//! * The same sample's plainest shape, unframed in a `p-0` card that is its
//!   one frame (`sample-flex-plain`).
//! * A list without a Grid catalog: 24 vendors through `ListColumns` (built-in
//!   columns as the host's own fragments) and `ListState` for the sort, whose
//!   URL Back and reload restore.
//!
//! The clock switch above the sample renders its typed timestamps through the
//! real `DisplayContext` in Company (Asia/Kuala_Lumpur), UTC or Local mode; a
//! timestamp keeps its type through every lens, including the band.
use bilibi_base::datetime::{DisplayContext, Format, Mode, Precision};
use bilibi_database::{Error, Result};
use bilibi_module_registry::{
    contributions::{Access, Contribution, Method, Route, ROUTES},
    Layer, Module, Provenance, Service,
};
use bilibi_security::Scope;
use bilibi_ui::flex_sample::FlexSample;
use bilibi_ui::page::{
    LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
    PageFuture, PageRequest, PageService,
};
use bilibi_ui::{
    Badge, BadgeKind, BuiltinColumn, Card, ColumnsOutcome, FlexRow, FlexTable, Header, Html,
    Instant, ListColumns, ListSpec, ListState, SortDir,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("name".into(), SortDir::Asc),
            ("status".into(), SortDir::Asc),
            ("balance".into(), SortDir::Desc),
        ]),
        default_sort: Some("name".into()),
        page_sizes: vec![25],
        page_size_param: "per_page".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("flex reference spec")
});

const STATUSES: [&str; 3] = ["active", "pending", "suspended"];

struct Vendor {
    id: u64,
    name: String,
    code: String,
    status: &'static str,
    balance: i64,
}

static VENDORS: LazyLock<Vec<Vendor>> = LazyLock::new(|| {
    let first = [
        "Amber", "Banyan", "Cobalt", "Delta", "Ember", "Fern", "Granite",
    ];
    let second = ["Freight", "Supplies", "Textiles", "Foods", "Logistics"];
    (1..=24u64)
        .map(|id| {
            let n = id as usize;
            Vendor {
                id,
                name: format!(
                    "{} {}",
                    first[(n * 3) % first.len()],
                    second[n % second.len()]
                ),
                code: format!("V-{id:04}"),
                status: STATUSES[(n * 5 + n / 3) % STATUSES.len()],
                balance: ((id * 7919) % 100_000) as i64 * 13 - 250_000,
            }
        })
        .collect()
});

fn builtins() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("name", "Name").sortable("name"),
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("status", "Status").sortable("status"),
        BuiltinColumn::new("balance", "Balance")
            .sortable("balance")
            .right(),
    ]
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
        _ => BadgeKind::Warning,
    }
}

struct Reference;

struct ReferenceState {
    sample: FlexSample,
    plain: FlexSample,
    mode: Mode,
    list: ListState,
    columns: ListColumns,
    notice: String,
}

impl ReferenceState {
    fn new(query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            sample: FlexSample::default(),
            plain: FlexSample::default(),
            mode: Mode::Company,
            list: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("vendors", builtins()).map_err(|_| Error::Internal)?,
            notice: String::new(),
        })
    }

    fn display(&self) -> DisplayContext {
        DisplayContext {
            mode: self.mode,
            timezone: "Asia/Kuala_Lumpur".into(),
            ..DisplayContext::anonymous()
        }
    }

    fn body(&self) -> Result<String> {
        let display = self.display();
        let instant = |seconds: i64| -> Instant {
            let at = jiff::Timestamp::from_second(seconds).expect("sample instant");
            let rendered = display.render(at, Format::DateTime, Precision::Minute);
            Instant::new(
                &rendered.iso,
                &rendered.text,
                &rendered.company,
                &rendered.utc,
                rendered.mode.as_str(),
            )
        };
        let mut html = render(Header {
            title: Html::text("Flexible tables"),
            subtitle: "",
            ..Header::default()
        })?;
        html.push(&self.clock_switch());
        html.push(&render(self.sample.framed(
            "sample-flex-table",
            "sample-flex",
            &instant,
        ))?);
        html.push(&Html::raw(
            r#"<p class="mt-6 mb-3 text-xs text-ink-muted">The plainest table, inside a card.</p>"#,
        ));
        html.push(&render(Card {
            id: "sample-flex-plain-card",
            inner_class: "p-0",
            body: render(
                self.plain
                    .plain("sample-flex-plain", "sample-flex-plain", &instant),
            )?,
            ..Card::default()
        })?);
        html.push(&Html::raw(
            r#"<p class="mt-6 mb-3 text-xs text-ink-muted">A list with its own columns: reorder, remove or add back a column, and change the row height.</p>"#,
        ));
        html.push(&render(Card {
            id: "vendors-card",
            inner_class: "p-0",
            body: self.vendors()?,
            ..Card::default()
        })?);
        render(bilibi_ui::Page {
            id: "flex-reference",
            body: Html::raw(format!("<div data-shared-ui>{html}</div>")),
            ..bilibi_ui::Page::default()
        })
        .map(|html| html.as_str().to_owned())
    }

    fn clock_switch(&self) -> Html {
        let button = |mode: Mode, label: &str| {
            let pressed = self.mode == mode;
            format!(
                r#"<button type="button" id="clock-{value}" data-event="clock" data-mode="{value}" aria-pressed="{pressed}" class="inline-flex h-6 items-center rounded-md border px-1.5 text-xs transition {class}">{label}</button>"#,
                value = mode.as_str(),
                class = if pressed {
                    "border-selection-line bg-brand-surface text-brand-ink"
                } else {
                    "border-line bg-surface text-ink-muted hover:bg-surface-sunken hover:text-ink"
                },
            )
        };
        Html::raw(format!(
            r#"<div id="clock-switch" role="group" aria-label="Timestamp clock" class="mb-6 flex items-center gap-1 text-xs text-ink-muted"><span class="mr-1">Timestamps in</span>{}{}{}</div>"#,
            button(Mode::Company, "Company (Kuala Lumpur)"),
            button(Mode::Utc, "UTC"),
            button(Mode::Local, "Local"),
        ))
    }

    fn sorted(&self) -> Vec<&'static Vendor> {
        let mut rows: Vec<&Vendor> = VENDORS.iter().collect();
        rows.sort_by(|a, b| {
            let order = match self.list.sort_by.as_deref() {
                Some("status") => a.status.cmp(b.status),
                Some("balance") => a.balance.cmp(&b.balance),
                _ => a.name.cmp(&b.name),
            }
            .then(a.id.cmp(&b.id));
            if self.list.sort_dir == Some(SortDir::Desc) {
                order.reverse()
            } else {
                order
            }
        });
        rows
    }

    /// The list's rows changed: ListColumns forgets an unfinished add box.
    fn reload(&mut self) {
        let rows = self.sorted();
        self.columns.load(&rows, |vendor| vendor.id.to_string());
    }

    fn vendors(&self) -> Result<Html> {
        let mut flex_rows = Vec::new();
        for vendor in self.sorted() {
            let mut flex = FlexRow::new(vendor.id.to_string());
            flex.dom_id = Some(format!("vendors-{}", vendor.id));
            flex.slots.insert(
                "name".into(),
                Html::raw(format!(
                    "<span class=\"font-medium text-ink-strong\">{}</span>",
                    Html::text(&vendor.name)
                )),
            );
            flex.slots.insert(
                "code".into(),
                Html::raw(format!(
                    "<code class=\"text-xs font-medium tabular-nums\">{}</code>",
                    Html::text(&vendor.code)
                )),
            );
            flex.slots.insert(
                "status".into(),
                render(Badge::status(badge(vendor.status), vendor.status))?,
            );
            flex.slots.insert(
                "balance".into(),
                Html::raw(format!(
                    "<span class=\"tabular-nums\">{}</span>",
                    money(vendor.balance)
                )),
            );
            flex_rows.push(flex);
        }
        let mut table = FlexTable::new("vendors", self.columns.flex_columns(), flex_rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.caption = "Vendors";
        render(table)
    }
}

impl Page<Scope, Error> for Reference {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let html = ReferenceState::new(request.query)?.body()?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }

    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(ReferenceState::new(request.query)?);
            Ok(Some(state))
        })
    }
}

/// The event payload's values as strings, the shape every flex op is read in.
fn value(event: &serde_json::Value, key: &str) -> Option<String> {
    match event.get(key)? {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

impl LiveState<Scope, Error> for ReferenceState {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.notice.clear();
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?;
                    self.reload();
                }
                LiveInput::Event(event) => {
                    let get = |key: &str| value(event, key);
                    match get("type").as_deref() {
                        Some("sample-flex") => self.sample.apply(&get),
                        Some("sample-flex-plain") => self.plain.apply(&get),
                        Some("clock") => {
                            if let Some(mode) = get("mode").as_deref().and_then(Mode::parse) {
                                self.mode = mode;
                            }
                        }
                        Some("preview-company") => {
                            self.notice = format!(
                                "Preview of example company {}.",
                                get("id").unwrap_or_default()
                            );
                        }
                        Some("grid") => {
                            let params: HashMap<String, String> = event
                                .as_object()
                                .into_iter()
                                .flatten()
                                .filter_map(|(key, _)| Some((key.clone(), get(key)?)))
                                .collect();
                            if let ColumnsOutcome::Sort(key) = self.columns.handle(&params) {
                                if !self.list.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                                self.reload();
                            }
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.body()?;
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                notice: std::mem::take(&mut self.notice),
                ..LiveBody::default()
            })
        })
    }
}

inventory::submit! {
    Module {id: "extensions/flex_reference", layer: Layer::Extensions,
        dependencies: &["base/ui"], migrations: &[], services: &[Service {
            id: "extensions/flex_reference.pages",
            factory: |_| Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Reference)))),
        }],
    }
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/flex_reference.page", Provenance {module:"extensions/flex_reference", source:file!(), line:line!()}, &Route { company_scope: bilibi_module_registry::contributions::CompanyScope::Tenant,
        method:Method::Get, path:"/flex-reference", service:"extensions/flex_reference.pages", operation:"page", access:Access::SignedIn, framed:true,
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
    println!("Flex reference listening at {origin}");
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
