//! The `base/authz.pages` adapter: it dispatches each discovered route's
//! operation to its screen (capabilities in `capabilities_web.rs`, roles in
//! `roles_web.rs`, field restrictions in `restrictions_web.rs`) and owns the
//! decision-log screen itself.
//!
//! `/authz/decision-logs` (`base/authz.pages`, operation `decision-logs`):
//! authorization decisions, newest first, ported from Bilimbi
//! `apps/base/authz/lib/authz/web/decision_logs_live.{ex,html.heex}`.
//!
//! The screen people reach for when someone says "it says I can't": the
//! result filter narrows to denials and every row shows its reason. The route
//! guard checks `admin.authz.decision-log.list` on every request and live event.
//! The shared FlexTable preserves Bilimbi's built-in columns and customization.
//! Bilimbi's platform-operator caution is not shown: Bilibi records every
//! decision against a tenant, so no row here is beyond the tenant's reach.
use crate::decision_logs::{self, DecisionLogSummary, Query, Sort};
use crate::restrictions_web;
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BadgeKind, BuiltinColumn, Card, ColumnType, ColumnsOutcome, FilterToolbar, FlexRow,
    FlexTable, Header, Html, IconButton, IconContext, ListColumns, ListSpec, ListState, PageCounts,
    Pagination, SortDir, TableEmpty, ToolbarControl,
};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
    time::SystemTime,
};

pub(crate) fn columns() -> ListColumns {
    let builtins = vec![
        {
            let mut column = BuiltinColumn::new("occurred_at", "When").sortable("occurred_at");
            column.sort_id = Some("logs-sort-occurred_at".into());
            column.kind = ColumnType::Datetime;
            column
        },
        {
            let mut column = BuiltinColumn::new("actor_type", "Actor").sortable("actor_type");
            column.sort_id = Some("logs-sort-actor_type".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("capability", "Capability").sortable("capability");
            column.sort_id = Some("logs-sort-capability".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("allowed", "Result").sortable("allowed");
            column.sort_id = Some("logs-sort-allowed".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("reason", "Reason").sortable("reason");
            column.sort_id = Some("logs-sort-reason".into());
            column
        },
        {
            let mut column = BuiltinColumn::new("resource", "Resource").sortable("resource");
            column.sort_id = Some("logs-sort-resource".into());
            column
        },
    ];
    ListColumns::mount("decision-logs", builtins).expect("decision-logs columns")
}

const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];

/// Bilimbi's spec: `occurred_at` opens descending, every other column
/// ascending; the page-size key is `per_page`; `result` is ``/allowed/denied.
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: BTreeMap::from([
            ("occurred_at".into(), SortDir::Desc),
            ("actor_type".into(), SortDir::Asc),
            ("capability".into(), SortDir::Asc),
            ("allowed".into(), SortDir::Asc),
            ("reason".into(), SortDir::Asc),
            ("resource".into(), SortDir::Asc),
        ]),
        default_sort: Some("occurred_at".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([(
            "result".into(),
            bilibi_ui::Filter::one_of(&["", "allowed", "denied"], ""),
        )]),
        ..ListSpec::default()
    }
    .shared()
    .expect("decision log list spec")
});

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub struct Pages {
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub grants: Arc<crate::grants::Grants>,
    pub restrictions: Arc<crate::restrictions::FieldRestrictions>,
    /// Names and proves the companies custom roles belong to.
    pub parties: Arc<bilibi_tenancy::PartyProofs>,
    pub step_up: Arc<bilibi_step_up::StepUp>,
    pub mutations: Arc<bilibi_database::MutationBoundary>,
}
impl Pages {
    async fn view(
        &self,
        scope: &Scope,
        state: &mut ListState,
        columns: &ListColumns,
    ) -> Result<String> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        let display = self.display.display(&**client, scope, None, locale).await?;
        let mut page = decision_logs::list(&**client, scope, &query(state)).await?;
        // An out-of-range page (a stale link, a shrunk filter) lands on the last
        // real page instead of an empty one with no pager.
        let pages = page.total_entries.div_ceil(state.page_size);
        if state.clamp_to_last_page(pages, true) {
            page = decision_logs::list(&**client, scope, &query(state)).await?;
        }
        render_page(state, columns, &display, &page.entries, page.total_entries)
    }
}

fn query(state: &ListState) -> Query {
    Query {
        search: state.search.clone(),
        allowed: match state.filters["result"].as_str() {
            "allowed" => Some(true),
            "denied" => Some(false),
            _ => None,
        },
        sort: state
            .sort_by
            .as_deref()
            .and_then(Sort::parse)
            .unwrap_or(Sort::OccurredAt),
        descending: state.sort_dir == Some(SortDir::Desc),
        page: state.page,
        page_size: state.page_size,
    }
}

fn parse(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}

pub(crate) fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// One instant, the Bilimbi `<.datetime>` contract: both server-decidable
/// texts travel with the element so the shell's display-mode control can swap
/// them without a re-render.
#[derive(Template)]
#[template(
    source = r#"<span class="whitespace-nowrap tabular-nums text-ink-muted"><time id="{{ id }}" datetime="{{ iso }}" data-format="datetime" data-precision="minute" data-mode="{{ mode }}" data-text-company="{{ company }}" data-text-utc="{{ utc }}" data-follow-shell="true" data-hook="DateTime" class="tabular-nums">{{ text }}</time></span>"#,
    ext = "html"
)]
pub(crate) struct WhenCell {
    pub(crate) id: String,
    pub(crate) iso: String,
    pub(crate) text: String,
    pub(crate) company: String,
    pub(crate) utc: String,
    pub(crate) mode: &'static str,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}<span class="whitespace-nowrap text-ink-muted">{% call ui::badge() %}{{ label }}{% endcall %}{% if let Some(principal) = principal %}<span class="ml-1">{{ principal }}</span>{% else %}<span class="ml-1 tabular-nums">{{ actor_id }}</span>{% endif %}{% if let Some(user) = acting_for %}<span class="ml-1 text-xs text-ink-muted tabular-nums">(as #{{ user }})</span>{% endif %}</span>"#,
    ext = "html"
)]
struct ActorCell<'a> {
    label: &'a str,
    principal: Option<&'a str>,
    actor_id: i64,
    acting_for: Option<i64>,
}

#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::badge(kind=kind) %}{{ label }}{% endcall %}"#,
    ext = "html"
)]
struct ResultCell {
    kind: BadgeKind,
    label: &'static str,
}

#[derive(Template)]
#[template(
    source = r#"<span class="{{ class }}">{{ text }}</span>"#,
    ext = "html"
)]
pub(crate) struct TextCell<'a> {
    pub(crate) class: &'a str,
    pub(crate) text: &'a str,
}

fn actor_label(actor_type: &str) -> &str {
    match actor_type {
        "agent" => "Employee",
        "user" => "User",
        "system" => "System",
        other => other,
    }
}

fn resource_label(log: &DecisionLogSummary) -> String {
    match (&log.resource_type, &log.resource_id) {
        (None, _) => "—".into(),
        (Some(kind), None) => kind.clone(),
        (Some(kind), Some(id)) => format!("{kind} #{id}"),
    }
}

fn when(display: &DisplayContext, id: i64, at: SystemTime) -> Result<Html> {
    let instant = jiff::Timestamp::try_from(at).map_err(|_| Error::Internal)?;
    let rendered = display.render(instant, Format::DateTime, Precision::Minute);
    render(WhenCell {
        id: format!("log-occurred-{id}"),
        iso: rendered.iso,
        text: rendered.text,
        company: rendered.company,
        utc: rendered.utc,
        mode: rendered.mode.as_str(),
    })
}

fn row(display: &DisplayContext, log: &DecisionLogSummary) -> Result<FlexRow> {
    let mut row = FlexRow::new(log.id.to_string());
    row.dom_id = Some(format!("logs-{}", log.id));
    let cells = vec![
        when(display, log.id, log.occurred_at)?,
        render(ActorCell {
            label: actor_label(&log.actor_type),
            principal: log.system_principal.as_deref(),
            actor_id: log.actor_id,
            acting_for: log.acting_for_user_id,
        })?,
        render(TextCell {
            class: "font-mono text-xs text-ink",
            text: &log.capability,
        })?,
        render(if log.allowed {
            ResultCell {
                kind: BadgeKind::Success,
                label: "Allowed",
            }
        } else {
            ResultCell {
                kind: BadgeKind::Danger,
                label: "Denied",
            }
        })?,
        render(TextCell {
            class: "font-mono text-xs text-ink-muted",
            text: if log.reason.is_empty() {
                "—"
            } else {
                &log.reason
            },
        })?,
        render(TextCell {
            class: "text-ink-muted",
            text: &resource_label(log),
        })?,
    ];
    for (id, cell) in [
        "occurred_at",
        "actor_type",
        "capability",
        "allowed",
        "reason",
        "resource",
    ]
    .into_iter()
    .zip(cells)
    {
        row = row.slot(id, cell);
    }
    Ok(row)
}

fn render_page(
    state: &ListState,
    columns: &ListColumns,
    display: &DisplayContext,
    entries: &[DecisionLogSummary],
    total: u64,
) -> Result<String> {
    let mut html = render(Header {
        title: Html::text("Decision Logs"),
        subtitle: "See who was allowed or refused access, newest first.",
        title_actions: render(IconButton {
            context: IconContext::Inline,
            id: "decision-logs-pin",
            nav_pin: "nav-admin-authz-decision-log",
            ..IconButton::new("bilimbi-pin", "Pin Decision Logs to sidebar")
        })?,
        ..Header::default()
    })?;
    let form = state.filters_form();
    html.push(&render(FilterToolbar::new(
        "logs-filters",
        vec![
            ToolbarControl::search(
                "logs-search",
                "search",
                "Search decisions",
                &form["search"],
                "Search capability, reason or resource",
            ),
            ToolbarControl::select(
                "logs-result",
                "result",
                "Filter by result",
                &form["result"],
                choices(&[
                    ("All results", ""),
                    ("Allowed", "allowed"),
                    ("Denied", "denied"),
                ]),
            ),
        ],
    ))?);
    let mut table = FlexTable::new(
        "decision-logs",
        columns.flex_columns(),
        entries
            .iter()
            .map(|log| row(display, log))
            .collect::<Result<Vec<_>>>()?,
    )
    .sorted(state.sort_by.as_deref(), state.sort_dir);
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.caption = "Authorization decisions";
    table.framed = false;
    if total == 0 {
        table.empty = Some(TableEmpty::Content(Html::text(
            if state.search.is_empty() && state.filters["result"].is_empty() {
                "No decisions recorded yet."
            } else {
                "No decisions match these filters."
            },
        )));
    }
    let mut card = render(table)?;
    card.push(&render(Pagination::new(
        "logs-pagination",
        PageCounts::new(state.page, state.page_size, total),
        &PAGE_SIZES,
    ))?);
    html.push(&render(Card {
        id: "decision-logs-card",
        inner_class: "p-0",
        body: card,
        ..Card::default()
    })?);
    Ok(render(bilibi_ui::Page {
        id: "decision-logs-index",
        body: html,
        ..bilibi_ui::Page::default()
    })?
    .as_str()
    .to_owned())
}

struct DecisionLogs {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
}

/// The role id of `/authz/roles/{id}`; anything else is the same not-found
/// answer as a role another tenant owns.
fn role_id(request: &PageRequest<'_, Scope>) -> Result<i64> {
    params::positive_id(request.path.get("id").map(String::as_str)).ok_or(Error::NotFound)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "decision-logs" => "Decision Logs",
            "capabilities" => "Capabilities",
            "roles" => "Roles",
            "role-create" => "Create Role",
            restrictions_web::OPERATION => "Field Restrictions",
            crate::step_up_web::OPERATION => "Step-up 2FA",
            // A role page is titled by the role's name (PageBody.title).
            _ => return None,
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut title = None;
            let html = match request.operation {
                "decision-logs" => {
                    self.view(scope, &mut parse(request.query)?, &columns())
                        .await?
                }
                "capabilities" => crate::capabilities_web::view(
                    &mut crate::capabilities_web::parse(request.query)?,
                    &crate::capabilities_web::columns(),
                )?,
                "roles" => {
                    self.roles_view(
                        scope,
                        &mut crate::roles_web::parse(request.query)?,
                        &crate::roles_web::columns(),
                    )
                    .await?
                }
                "role-create" => crate::roles_web::create_view(self, scope).await?,
                "role" => {
                    let (html, name) = self
                        .role_view(
                            scope,
                            role_id(&request)?,
                            &crate::roles_web::principal_columns(),
                        )
                        .await?;
                    title = Some(name);
                    html
                }
                restrictions_web::OPERATION => {
                    restrictions_web::Screen::mount(self, scope)
                        .await?
                        .body(scope)
                        .await?
                }
                crate::step_up_web::OPERATION => self.step_up_view(scope, request.query).await?,
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "decision-logs" => Box::new(DecisionLogs {
                    pages: self.clone(),
                    state: parse(request.query)?,
                    columns: columns(),
                }),
                "capabilities" => Box::new(crate::capabilities_web::Capabilities {
                    state: crate::capabilities_web::parse(request.query)?,
                    columns: crate::capabilities_web::columns(),
                }),
                "roles" => Box::new(crate::roles_web::Roles {
                    pages: self.clone(),
                    state: crate::roles_web::parse(request.query)?,
                    columns: crate::roles_web::columns(),
                }),
                "role-create" => {
                    let scope = request.scope.ok_or(Error::Unauthorized)?;
                    Box::new(crate::roles_web::RoleCreate::mount(self.clone(), scope).await?)
                }
                "role" => Box::new(crate::roles_web::RoleShow {
                    pages: self.clone(),
                    id: role_id(&request)?,
                    columns: crate::roles_web::principal_columns(),
                }),
                restrictions_web::OPERATION => {
                    let scope = request.scope.ok_or(Error::Unauthorized)?;
                    Box::new(restrictions_web::Screen::mount(self, scope).await?)
                }
                crate::step_up_web::OPERATION => Box::new(crate::step_up_web::StepUpPolicy::mount(
                    self,
                    request.query,
                )?),
                _ => return Err(Error::NotFound),
            };
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for DecisionLogs {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => {
                    let get = |key: &str| value.get(key).and_then(|v| v.as_str());
                    match get("type") {
                        Some("filters") => self.state.apply_filters(
                            &params::form_values(value, FILTERS_FORM).map_err(Error::refused)?,
                        ),
                        Some("sort") => {
                            if !self.state.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("grid") => {
                            let values = value
                                .as_object()
                                .into_iter()
                                .flatten()
                                .filter_map(|(key, value)| {
                                    Some((key.clone(), value.as_str()?.to_owned()))
                                })
                                .collect();
                            if let ColumnsOutcome::Sort(key) = self.columns.handle(&values) {
                                if !self.state.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                            }
                        }
                        Some("page") => self.state.put_page(get("page")),
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self
                .pages
                .view(request.scope, &mut self.state, &self.columns)
                .await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                ..LiveBody::default()
            })
        })
    }
}
