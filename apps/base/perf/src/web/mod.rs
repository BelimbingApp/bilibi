//! Module-owned operator page and dashboard embed. The host checks the route;
//! the adapters also reauthorize before reading or returning cached history.
mod panel;
mod view;

use crate::{Filter, Kind, Outcome, Recorder, Windows, VIEW};
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::DisplayPolicy;
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Embed, EmbedContext, Method, Route, EMBEDS, ROUTES},
    Contribution, Provenance,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::{ParamNames, FILTERS_FORM},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BuiltinColumn, ColumnType, ListColumns, ListSpec, ListState,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
    time::{Duration, SystemTime},
};

const PATH: &str = "/system/performance";
/// The page's views. The first is the default and leaves `tab` out of the address.
const TABS: [(&str, &str); 2] = [("slowdowns", "Slowdowns"), ("samples", "All samples")];
const SIZES: [u64; 4] = [25, 50, 100, 300];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: SIZES.to_vec(),
        page_size_aliases: vec!["perPage".into()],
        param_names: ParamNames {
            search: "identity".into(),
            ..ParamNames::default()
        },
        filters: BTreeMap::from([
            (
                "kind".into(),
                bilibi_ui::Filter::one_of(&["", "request", "event", "job", "runtime"], ""),
            ),
            (
                "outcome".into(),
                bilibi_ui::Filter::one_of(&["", "ok", "error", "cancelled", "discarded"], ""),
            ),
        ]),
        omit_blank: vec!["search".into(), "kind".into(), "outcome".into()],
        ..ListSpec::default()
    }
    .shared()
    .expect("performance list spec")
});

#[derive(Clone)]
pub(super) struct Pages {
    pub recorder: Arc<Recorder>,
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}
impl Pages {
    async fn authorize(&self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_authz::authorize_operator(&**client, scope, VIEW).await?;
        Ok(())
    }
}

struct Index {
    pages: Pages,
    list: ListState,
    tab: &'static str,
    regressions: ListColumns,
    slowest: ListColumns,
    samples: ListColumns,
    data: Option<view::Data>,
}
fn tab_id(query: &HashMap<String, String>) -> &'static str {
    TABS.iter()
        .find(|(id, _)| query.get("tab").map(String::as_str) == Some(*id))
        .map_or(TABS[0].0, |(id, _)| *id)
}
fn builtin(id: &str, label: &str, kind: ColumnType, right: bool) -> BuiltinColumn {
    let mut column = BuiltinColumn::new(id, label);
    column.kind = kind;
    column.align_right = right;
    column
}
impl Index {
    fn new(pages: &Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            list: parse(query)?,
            tab: tab_id(query),
            regressions: ListColumns::mount(
                "performance-regression-table",
                vec![
                    BuiltinColumn::new("identity", "Page or job"),
                    builtin("baseline", "Baseline avg", ColumnType::Float, true),
                    builtin("current", "Current p95", ColumnType::Float, true),
                    builtin("change", "Change", ColumnType::Float, true),
                ],
            )
            .map_err(|_| Error::Internal)?,
            slowest: ListColumns::mount(
                "performance-slowest-table",
                vec![
                    BuiltinColumn::new("identity", "Page or job"),
                    builtin("samples", "Samples", ColumnType::Integer, true),
                    builtin("average", "Average", ColumnType::Float, true),
                    builtin("p95", "p95", ColumnType::Float, true),
                ],
            )
            .map_err(|_| Error::Internal)?,
            samples: ListColumns::mount(
                "performance-sample-table",
                vec![
                    builtin("observed_at", "Observed", ColumnType::Datetime, false),
                    BuiltinColumn::new("kind", "Kind"),
                    BuiltinColumn::new("identity", "Page or job"),
                    BuiltinColumn::new("outcome", "Outcome"),
                    builtin("duration", "Duration", ColumnType::Integer, true),
                    builtin("database", "Database", ColumnType::Integer, true),
                ],
            )
            .map_err(|_| Error::Internal)?,
            data: None,
        })
    }
    fn filter(&self) -> Filter {
        Filter {
            page: self.list.page as i64,
            page_size: self.list.page_size as i64,
            kind: Kind::parse(&self.list.filters["kind"]),
            outcome: Outcome::parse(&self.list.filters["outcome"]),
            identity: (!self.list.search.is_empty()).then(|| self.list.search.clone()),
            // Bilimbi has no time-range filter: the list is all retained history.
            from: None,
            to: None,
        }
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let now = SystemTime::now();
        let hour = Duration::from_secs(3600);
        let recorder = &self.pages.recorder;
        let (diagnostics, views) = if self.tab == TABS[1].0 {
            let filter = self.filter();
            let (diagnostics, mut samples) =
                tokio::join!(recorder.diagnostics(), recorder.list_samples(&filter));
            if let Ok(page) = &samples {
                if self
                    .list
                    .clamp_to_last_page((page.total as u64).div_ceil(self.list.page_size), true)
                {
                    samples = recorder.list_samples(&self.filter()).await;
                }
            }
            (diagnostics, view::Views::Samples(samples))
        } else {
            let windows = Windows {
                baseline_from: now - hour * 2,
                baseline_to: now - hour,
                current_from: now - hour,
                current_to: now,
                min_samples: 5,
                limit: 10,
            };
            let (diagnostics, regressions, slowest) = tokio::join!(
                recorder.diagnostics(),
                recorder.regressions(&windows),
                recorder.slowest(now - hour * 24, now, 10),
            );
            (
                diagnostics,
                view::Views::Slowdowns {
                    regressions,
                    slowest,
                },
            )
        };
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
        self.data = Some(view::Data {
            diagnostics,
            views,
            display,
        });
        Ok(())
    }
    fn query(&self) -> HashMap<String, String> {
        let mut query = self.list.to_params();
        if self.tab != TABS[0].0 {
            query.insert("tab".into(), self.tab.into());
        }
        query
    }
    fn body(&self) -> Result<String> {
        view::page(
            &self.list,
            &self.samples,
            &self.regressions,
            &self.slowest,
            self.data.as_ref().ok_or(Error::Internal)?,
            self.tab,
        )
    }
}
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    let mut state = ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?;
    // Storage bounds UTF-8 bytes; truncate at a character boundary too.
    state.search = state
        .search
        .trim()
        .chars()
        .scan(0, |bytes, c| {
            *bytes += c.len_utf8();
            (*bytes <= 255).then_some(c)
        })
        .collect();
    Ok(state)
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Performance")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            self.authorize(scope).await?;
            let mut index = Index::new(self, request.query)?;
            index.load(scope).await?;
            Ok(PageBody {
                html: index.body()?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            self.authorize(request.scope.ok_or(Error::Unauthorized)?)
                .await?;
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "index" => Box::new(Index::new(self, request.query)?),
                "health" => Box::new(panel::HealthPanel::new(self, &request)?),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}
impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.pages.authorize(request.scope).await?;
            let mut reload = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.tab = tab_id(values);
                    self.list = parse(values)?;
                }
                LiveInput::Event(value) => match value.get("type").and_then(|v| v.as_str()) {
                    Some("filters") => {
                        let mut posted =
                            params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                                .map_err(Error::refused)?;
                        if let Some(identity) = posted.remove("identity") {
                            posted.insert("search".into(), identity);
                        }
                        self.list.apply_filters(&posted);
                        self.list = parse(&self.list.to_params())?;
                    }
                    Some("page") => self
                        .list
                        .put_page(value.get("page").and_then(|v| v.as_str())),
                    Some(event @ ("regressions_grid" | "slowest_grid" | "samples_grid")) => {
                        let values = value
                            .as_object()
                            .into_iter()
                            .flatten()
                            .filter_map(|(key, v)| {
                                let text = match v {
                                    serde_json::Value::String(s) => s.clone(),
                                    serde_json::Value::Number(n) => n.to_string(),
                                    _ => return None,
                                };
                                Some((key.clone(), text))
                            })
                            .collect();
                        match event {
                            "samples_grid" => {
                                self.samples.handle(&values);
                            }
                            "slowest_grid" => {
                                self.slowest.handle(&values);
                            }
                            _ => {
                                self.regressions.handle(&values);
                            }
                        }
                        reload = false;
                    }
                    _ => return Err(Error::Invalid),
                },
            }
            if reload || self.data.is_none() {
                self.load(request.scope).await?;
            }
            Ok(LiveBody {
                html: self.body()?,
                query: self.query(),
                ..LiveBody::default()
            })
        })
    }
}

bilibi_menu::contribute!(
    "base/perf",
    bilibi_menu::MenuItem::page("admin.system.performance", "Performance", PATH)
        .icon("chart-bar")
        .under("admin.system.diagnostics")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(20)
);
bilibi_dashboard::contribute!(
    "base/perf",
    bilibi_dashboard::Widget::new(
        "base-perf-health",
        "Performance health",
        "dashboard.performance"
    )
    .order(50)
    .requires(VIEW)
    .refresh(60_000)
);
inventory::submit! { Contribution::new(ROUTES, "base/perf.index", Provenance {module:"base/perf", source:file!(), line:line!()}, &Route {
    method:Method::Get, path:PATH, service:"base/perf.pages", operation:"index", access:Access::OperatorCapability(VIEW), framed:true,
}) }
inventory::submit! { Contribution::new(EMBEDS, "dashboard.performance", Provenance {module:"base/perf", source:file!(), line:line!()}, &Embed {
    slot:"dashboard.performance", context:EmbedContext::Dashboard, service:"base/perf.pages", operation:"health", access:Access::SignedIn,
}) }
