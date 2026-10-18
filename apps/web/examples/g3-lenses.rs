//! Live Grid lens walkthrough. Only run against a disposable migrated
//! PostgreSQL with BILIBI_GRID_DEMO=disposable, plus DATABASE_URL,
//! APP_ORIGIN and BIND_ADDR. No test runner or mocks.
//!
//! It replaces the non-business reference fixture with twelve tenant-1 items
//! whose movements spread over the last fourteen months (relative to today, on
//! the UTC calendar), then serves `/g3-lenses`: the items as a FlexTable whose
//! walked columns are projected by `bilibi_grid::project_lenses` and drawn
//! through `bilibi_grid::lens`. Switch a column's lens from its chip, set the
//! change-since date, and flip the timestamp clock; every reply re-projects for
//! the signed-in scope. Sign in as the fixture manager (grid.reference.view).
//!
//! With G3_DUMP=<file> it first writes every cell of every column through every
//! lens it allows, with the raw value, range and extras it was prepared from, as
//! JSON lines for comparison with Bilimbi's `Lens.cell` (evidence tooling).
use bilibi_base::{
    authz::grants::Grants,
    datetime::{DisplayContext, Mode},
    grid::{self, lens, Lens, Lenses, Stats},
    tenancy::Authentication,
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
use bilibi_ui::{flex_table, Card, FlexRow, FlexTable, Header, Html};
use jiff::civil;
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Write, sync::Arc};
bilibi_walkthrough_fixture::walkthrough_vocabulary!();
bilibi_walkthrough_fixture::walkthrough_module_roles!();

const ROOT: &str = "reference_items";

/// The walked columns the page opens with, and the lens each wears.
const COLUMNS: [(&str, Lens); 12] = [
    ("name", Lens::Value),
    ("category", Lens::Band),
    ("quantity", Lens::Bar),
    ("unit_cost", Lens::Bar),
    ("active", Lens::Band),
    ("group.name", Lens::Value),
    ("group.region", Lens::Band),
    ("movements:count", Lens::Trend),
    ("movements.delta:sum", Lens::Delta),
    ("movements.delta:avg", Lens::Band),
    ("movements.moved_on:max", Lens::Value),
    ("movements.recorded_at:max", Lens::Bar),
];

struct Lenses3;

struct LensState {
    db: Db,
    lenses: BTreeMap<String, Lens>,
    since: Option<civil::Date>,
    zoom: u16,
    mode: Mode,
}

impl LensState {
    fn new(db: Db) -> Self {
        Self {
            db,
            lenses: COLUMNS
                .iter()
                .map(|(spec, lens)| ((*spec).to_owned(), *lens))
                .collect(),
            since: None,
            zoom: flex_table::DEFAULT_ZOOM,
            mode: Mode::Company,
        }
    }

    fn display(&self) -> DisplayContext {
        DisplayContext {
            mode: self.mode,
            timezone: "Asia/Kuala_Lumpur".into(),
            ..DisplayContext::anonymous()
        }
    }

    fn specs(&self) -> Vec<&'static str> {
        COLUMNS.iter().map(|(spec, _)| *spec).collect()
    }

    fn worn(&self, lens: Lens) -> Vec<String> {
        self.lenses
            .iter()
            .filter(|(_, worn)| **worn == lens)
            .map(|(spec, _)| spec.clone())
            .collect()
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let ids = item_ids(&self.db, scope).await?;
        let specs = self.specs();
        let request = Lenses {
            trend: self.worn(Lens::Trend),
            delta: self.worn(Lens::Delta),
            since: self.since,
        };
        let projection =
            grid::project_lenses(&self.db, scope, ROOT, &ids, &specs, &request).await?;
        // Only a bar or a band scales to the column's range over the whole
        // table, so only then is that range read, and its cost with it.
        let mut scaled = self.worn(Lens::Bar);
        scaled.extend(self.worn(Lens::Band));
        let stats = if scaled.is_empty() {
            Stats::default()
        } else {
            grid::stats(&self.db, scope, ROOT, &scaled).await?
        };
        let display = self.display();
        let lens_of = |column: &grid::Column| {
            lens::normalize(
                self.lenses
                    .get(&column.spec)
                    .map_or("value", |lens| lens.as_str()),
                column,
            )
        };
        let columns: Vec<_> = projection
            .columns
            .iter()
            .map(|column| lens::flex_column(column, lens_of(column)))
            .collect();
        let rows = projection
            .rows
            .iter()
            .map(|row| {
                let mut flex = FlexRow::new(row.key.to_string());
                for column in &projection.columns {
                    let extras = lens::Extras {
                        series: row.series.get(&column.id).map(Vec::as_slice),
                        before: row.before.get(&column.id),
                    };
                    let cell = lens::cell(
                        &row.cells[&column.id],
                        column,
                        stats.ranges.get(&column.id),
                        lens_of(column),
                        extras,
                        &display,
                    );
                    flex = flex.cell(&column.id, cell);
                }
                flex
            })
            .collect();
        let since = self
            .since
            .unwrap_or_else(|| grid::query::default_since(grid::query::today()))
            .to_string();
        let mut table = FlexTable::new("g3-items", columns, rows);
        table.zoom = self.zoom;
        table.cost = lens::cost(stats.estimated_cost);
        table.since = Some(&since);
        table.framed = false;
        table.caption = "Reference items through Grid lenses";
        let mut html = render(Header {
            title: Html::text("Grid lenses"),
            subtitle: "Show each column as a value, bar, band, trend or change since a date.",
            ..Header::default()
        })?;
        html.push(&self.clock_switch());
        html.push(&render(Card {
            id: "g3-items-card",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?);
        render(bilibi_ui::Page {
            id: "g3-lenses",
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

    /// One `grid` op from the table. Lens, since and zoom are this slice's;
    /// adding, removing and reordering columns belong to PageColumns (G4).
    fn apply(&mut self, get: &dyn Fn(&str) -> Option<String>) {
        match get("op").as_deref() {
            Some("lens") => {
                if let (Some(spec), Some(lens)) =
                    (get("spec"), get("lens").as_deref().and_then(Lens::parse))
                {
                    if self.lenses.contains_key(&spec) {
                        self.lenses.insert(spec, lens);
                    }
                }
            }
            Some("since") => {
                self.since = get("since").and_then(|since| since.parse().ok());
            }
            Some("zoom") => match get("dir").as_deref() {
                Some("in") => self.zoom = flex_table::step_zoom(self.zoom, true),
                Some("out") => self.zoom = flex_table::step_zoom(self.zoom, false),
                _ => {}
            },
            Some("zoom_preset") => {
                if let Some(mode) = get("preset")
                    .as_deref()
                    .and_then(flex_table::RowMode::parse)
                {
                    self.zoom = mode.zoom();
                }
            }
            _ => {}
        }
    }
}

fn render(fragment: impl askama::Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// The owner's own list: tenant items by name, one page. Grid only projects.
async fn item_ids(db: &Db, scope: &Scope) -> Result<Vec<Value>> {
    let client = db.get().await?;
    let rows = client
        .query(
            "SELECT id FROM base_grid_ref_items WHERE tenant_id = $1 ORDER BY name, id LIMIT 25",
            &[&scope.tenant()],
        )
        .await?;
    Ok(rows.iter().map(|row| json!(row.get::<_, i64>(0))).collect())
}

/// The event payload's values as strings, the shape every flex op is read in.
fn value(event: &Value, key: &str) -> Option<String> {
    match event.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

impl Page<Scope, Error> for Lenses3 {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Forbidden)?;
            let html = LensState::new(DB.get().ok_or(Error::Internal)?.clone())
                .body(scope)
                .await?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }

    fn mount<'a>(&'a self, _request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(LensState::new(DB.get().ok_or(Error::Internal)?.clone()));
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for LensState {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(event) = request.input {
                let get = |key: &str| value(event, key);
                match get("type").as_deref() {
                    Some("grid") => self.apply(&get),
                    Some("clock") => {
                        if let Some(mode) = get("mode").as_deref().and_then(Mode::parse) {
                            self.mode = mode;
                        }
                    }
                    _ => return Err(Error::Invalid),
                }
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                ..LiveBody::default()
            })
        })
    }
}

static DB: std::sync::OnceLock<Db> = std::sync::OnceLock::new();

inventory::submit! {
    Module {id: "extensions/g3_lenses", layer: Layer::Extensions,
        dependencies: &["base/ui", "base/grid"], migrations: &[], services: &[Service {
            id: "extensions/g3_lenses.pages",
            factory: |_| Ok(Arc::new(PageService::<Scope, Error>(Arc::new(Lenses3)))),
        }],
    }
}
inventory::submit! {
    Contribution::new(ROUTES, "extensions/g3_lenses.page", Provenance {module:"extensions/g3_lenses", source:file!(), line:line!()}, &Route {
        method:Method::Get, path:"/g3-lenses", service:"extensions/g3_lenses.pages", operation:"page", access:Access::SignedIn, framed:true,
    })
}

/// Every cell of every column through every lens it allows, with what it was
/// prepared from, as JSON lines (UTC display, so texts are deterministic).
async fn dump(
    db: &Db,
    scope: &Scope,
    path: &str,
) -> std::result::Result<(), Box<dyn std::error::Error>> {
    let ids = item_ids(db, scope).await?;
    let specs: Vec<&str> = COLUMNS.iter().map(|(spec, _)| *spec).collect();
    let all: Vec<String> = specs.iter().map(|spec| (*spec).to_owned()).collect();
    let since = civil::date(2026, 7, 1);
    let request = Lenses {
        trend: all.clone(),
        delta: all.clone(),
        since: Some(since),
    };
    let projection = grid::project_lenses(db, scope, ROOT, &ids, &specs, &request).await?;
    let stats = grid::stats(db, scope, ROOT, &specs).await?;
    let display = DisplayContext {
        mode: Mode::Utc,
        timezone: "UTC".into(),
        ..DisplayContext::anonymous()
    };
    let mut out = std::io::BufWriter::new(std::fs::File::create(path)?);
    writeln!(
        out,
        "{}",
        json!({"since": since.to_string(), "months": projection.months.iter().map(ToString::to_string).collect::<Vec<_>>(), "cost": stats.estimated_cost, "estimated_projection_cost": projection.estimated_cost})
    )?;
    for row in &projection.rows {
        for column in &projection.columns {
            for lens in lens::available(column) {
                let range = stats.ranges.get(&column.id);
                let extras = lens::Extras {
                    series: row.series.get(&column.id).map(Vec::as_slice),
                    before: row.before.get(&column.id),
                };
                let cell = lens::cell(
                    &row.cells[&column.id],
                    column,
                    range,
                    *lens,
                    extras,
                    &display,
                );
                let typed = match &cell.typed {
                    flex_table::Typed::Text => Value::Null,
                    flex_table::Typed::Date { iso, text } => json!({"date": iso, "text": text}),
                    flex_table::Typed::Instant(i) => json!({"instant": i.iso, "text": i.text}),
                };
                writeln!(
                    out,
                    "{}",
                    json!({
                        "key": row.key, "spec": column.spec, "ty": format!("{:?}", column.ty),
                        "kind": format!("{:?}", column.kind), "agg": column.agg.map(|a| a.key()),
                        "field_ty": column.field.map(|f| format!("{:?}", f.ty)),
                        "dated": column.time_field.is_some(),
                        "available": lens::available(column).iter().map(|l| l.as_str()).collect::<Vec<_>>(),
                        "lens": lens.as_str(), "value": row.cells[&column.id],
                        "range": range.map(|r| json!({"min": r.min, "max": r.max})),
                        "series": row.series.get(&column.id), "before": row.before.get(&column.id),
                        "out": {"text": cell.text, "typed": typed, "n": cell.n, "band": cell.band,
                                "scale": cell.scale.map(|s| s.as_str()),
                                "series": if cell.series.is_empty() { Value::Null } else { json!(cell.series) },
                                "sparkline": cell.sparkline(), "bar": cell.bar_width()}
                    })
                )?;
            }
        }
    }
    out.flush()?;
    println!("Wrote every lens cell to {path}");
    Ok(())
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
    if let Ok(path) = std::env::var("G3_DUMP") {
        let auth = app
            .services
            .get::<Authentication>("base/tenancy.authentication")?;
        let client = db.get().await?;
        let manager = auth.authenticate(&**client, 1, 1, String::new()).await?;
        dump(&db, &manager, &path).await?;
    }
    let _ = DB.set(db);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    println!("Grid lenses at {origin}/g3-lenses");
    axum::serve(listener, bilibi_web::router(app))
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}
