//! `/authz/step-up` (`base/authz.pages`, operation `step-up`): the tenant's
//! two-factor requirements. One FlexTable row per step-up operation the
//! mounted modules declared (`bilibi_step_up::declare!`), sorted by module,
//! with its default, this tenant's choice and the installation floor; the
//! tenant's window and cap above it; for the installation owner, the floor
//! and the installation-wide maxima. No Bilimbi counterpart.
//!
//! Every change is a confirmed intent that rechecks `admin.authz.step-up.manage`
//! inside its audited transaction and first passes step-up itself
//! (`base.step_up.policy.update`, on the floor): with no recent check the
//! host sends the browser to confirm and back.
use crate::web::{render, Pages, TextCell};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_step_up::{Mode, Rule, Window, CAP_RANGE, POLICY_UPDATE, WINDOW_RANGE};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome,
    ConfirmDialog, Field, FilterToolbar, FlashGroup, FlashMap, FlexRow, FlexTable, Header, Html,
    InputKind, ListColumns, ListSpec, ListState, Pagination, SortDir, StatusKind, TableEmpty,
    ToolbarControl,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

pub const OPERATION: &str = "step-up";
pub const MANAGE: &str = "admin.authz.step-up.manage";
const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];

pub(crate) fn columns() -> ListColumns {
    let column = |id: &str, label: &str, sortable: bool| {
        let column = BuiltinColumn::new(id, label);
        if sortable {
            column.sortable(id)
        } else {
            column
        }
    };
    ListColumns::mount(
        "step-up-operations",
        vec![
            column("module", "Module", true),
            column("operation", "Operation", true),
            column("default", "Default", false),
            column("tenant", "This tenant", false),
            column("floor", "Installation floor", false),
        ],
    )
    .expect("step-up columns")
}

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        sortable: [("module", SortDir::Asc), ("operation", SortDir::Asc)]
            .into_iter()
            .map(|(key, dir)| (key.to_owned(), dir))
            .collect(),
        default_sort: Some("module".into()),
        page_sizes: PAGE_SIZES.to_vec(),
        page_size_param: "per_page".into(),
        ..ListSpec::default()
    }
    .shared()
    .expect("step-up list spec")
});

pub(crate) fn parse(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}

/// A change waiting for its confirmation.
enum Pending {
    Mode { key: String, mode: Option<Mode> },
    Floor { key: String, floor: Option<bool> },
}

pub(crate) struct StepUpPolicy {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    pending: Option<Pending>,
    flash: Option<(StatusKind, String)>,
    window_errors: Vec<String>,
    limit_errors: Vec<String>,
}

#[derive(Template)]
#[template(
    source = r#"<div class="min-w-0"><span class="block text-sm text-ink">{{ label }}</span><span class="block font-mono text-xs text-ink-muted">{{ key }}</span></div>"#,
    ext = "html"
)]
struct OperationCell<'a> {
    label: &'a str,
    key: &'a str,
}

#[derive(Template)]
#[template(
    source = r#"<form id="{{ id }}" data-event="{{ event }}" class="flex flex-wrap items-end gap-3 p-4">{{ first }}{{ second }}{{ save }}</form>"#,
    ext = "html"
)]
struct WindowForm<'a> {
    id: &'a str,
    event: &'a str,
    first: Html,
    second: Html,
    save: Html,
}

fn badge(kind: BadgeKind, text: &str) -> Result<Html> {
    render(Badge::status(kind, text))
}
fn mode_badge(mode: Mode) -> Result<Html> {
    badge(
        match mode {
            Mode::Off => BadgeKind::Neutral,
            Mode::On => BadgeKind::Success,
            Mode::EveryTime => BadgeKind::Warning,
        },
        mode.label(),
    )
}
fn dom(key: &str) -> String {
    format!("step-up-{}", key.replace(['.', '_'], "-"))
}

impl StepUpPolicy {
    pub(crate) fn mount(pages: &Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages: pages.clone(),
            state: parse(query)?,
            columns: columns(),
            pending: None,
            flash: None,
            window_errors: Vec::new(),
            limit_errors: Vec::new(),
        })
    }

    async fn owner(&self, scope: &Scope) -> Result<bool> {
        let client = self.pages.db.get().await?;
        Ok(bilibi_tenancy::prove_owner(&**client, scope).await.is_ok())
    }

    /// One confirmed change: capability again inside the transaction, then
    /// step-up for changing the policy itself, then the audited write.
    async fn apply(&mut self, scope: &Scope, pending: Pending) -> Result<()> {
        let target = match &pending {
            Pending::Mode { key, mode } => {
                format!("{key} tenant:{}", mode.map_or("default", Mode::as_str))
            }
            Pending::Floor { key, floor } => format!("{key} floor:{floor:?}"),
        };
        if matches!(pending, Pending::Floor { .. }) && !self.owner(scope).await? {
            return Err(Error::Forbidden);
        }
        let step_up = self.pages.step_up.clone();
        let mut client = self.pages.db.get().await?;
        let mut tx = self
            .pages
            .mutations
            .begin(&mut client, scope.mutation_context(POLICY_UPDATE))
            .await?;
        crate::authorize_key(tx.sql(), scope, MANAGE).await?;
        step_up.require(scope, POLICY_UPDATE, &target).await?;
        let written = match &pending {
            Pending::Mode { key, mode } => step_up.set_mode(&mut tx, scope, key, *mode).await,
            Pending::Floor { key, floor } => step_up.set_floor(&mut tx, scope, key, *floor).await,
        };
        match written {
            Ok(()) => {
                tx.commit().await?;
                self.flash = Some((StatusKind::Success, "2FA requirement saved.".into()));
            }
            Err(Error::Conflict) => {
                self.flash = Some((
                    StatusKind::Error,
                    "The installation floor keeps this operation on; it cannot be turned off here."
                        .into(),
                ));
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    async fn save_window(
        &mut self,
        scope: &Scope,
        values: &HashMap<String, String>,
        limits: bool,
    ) -> Result<()> {
        let number = |key: &str| values.get(key).and_then(|v| v.trim().parse::<i64>().ok());
        let (first, second) = if limits {
            (number("window_limit_minutes"), number("cap_limit_hours"))
        } else {
            (number("window_minutes"), number("cap_hours"))
        };
        let errors = if limits {
            &mut self.limit_errors
        } else {
            &mut self.window_errors
        };
        errors.clear();
        let (Some(minutes), Some(hours)) = (first, second) else {
            errors.push("Enter whole numbers.".into());
            return Ok(());
        };
        if limits && !self.owner(scope).await? {
            return Err(Error::Forbidden);
        }
        let step_up = self.pages.step_up.clone();
        let mut client = self.pages.db.get().await?;
        let mut tx = self
            .pages
            .mutations
            .begin(&mut client, scope.mutation_context(POLICY_UPDATE))
            .await?;
        crate::authorize_key(tx.sql(), scope, MANAGE).await?;
        let target = format!(
            "{} window:{minutes}m cap:{hours}h",
            if limits { "installation" } else { "tenant" }
        );
        step_up.require(scope, POLICY_UPDATE, &target).await?;
        let written = if limits {
            step_up.set_limits(&mut tx, scope, minutes, hours).await
        } else {
            step_up.set_window(&mut tx, scope, minutes, hours).await
        };
        match written {
            Ok(()) => {
                tx.commit().await?;
                self.flash = Some((StatusKind::Success, "Confirmed-access window saved.".into()));
            }
            Err(Error::Invalid) => {
                let errors = if limits {
                    &mut self.limit_errors
                } else {
                    &mut self.window_errors
                };
                errors.push(format!(
                    "The window must be {}–{} minutes and the cap {}–{} hours, the cap at least the window{}.",
                    WINDOW_RANGE.0,
                    WINDOW_RANGE.1,
                    CAP_RANGE.0,
                    CAP_RANGE.1,
                    if limits { "" } else { ", both within the installation maximum" }
                ));
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn actions(&self, rule: &Rule<'_>, owner: bool) -> Result<Html> {
        let key = rule.declared.operation.key;
        let id = dom(key);
        let mut html = Html::default();
        for (mode, label) in [
            (Mode::Off, "Off"),
            (Mode::On, "On"),
            (Mode::EveryTime, "Ask every time"),
        ] {
            if rule.effective == mode || (mode == Mode::Off && rule.floor) {
                continue;
            }
            html.push(&render(Button {
                id: &format!("{id}-{}", mode.as_str()),
                button_type: "button",
                event: &format!("request_{}", mode.as_str()),
                name: "key",
                value: key,
                class: "px-2 py-0.5 text-xs",
                body: Html::text(label),
                ..Button::default()
            })?);
        }
        if rule.choice.is_some() {
            html.push(&render(Button {
                id: &format!("{id}-default"),
                button_type: "button",
                event: "request_default",
                name: "key",
                value: key,
                class: "px-2 py-0.5 text-xs",
                body: Html::text("Use default"),
                ..Button::default()
            })?);
        }
        if owner {
            let (event, label) = if rule.floor {
                ("request_unlock", "Remove from floor")
            } else {
                ("request_lock", "Add to floor")
            };
            html.push(&render(Button {
                id: &format!("{id}-floor"),
                button_type: "button",
                event,
                name: "key",
                value: key,
                class: "px-2 py-0.5 text-xs",
                body: Html::text(label),
                ..Button::default()
            })?);
        }
        Ok(html)
    }

    fn row(&self, rule: &Rule<'_>, owner: bool) -> Result<FlexRow> {
        let operation = rule.declared.operation;
        let default = if operation.default_on {
            Mode::On
        } else {
            Mode::Off
        };
        let mut tenant = mode_badge(rule.effective)?;
        if rule.choice.is_none() {
            tenant.push(&render(TextCell {
                class: "ml-1 text-xs text-ink-muted",
                text: "(default)",
            })?);
        }
        let mut row = FlexRow::new(operation.key)
            .slot(
                "module",
                render(TextCell {
                    class: "font-mono text-xs text-ink-muted",
                    text: rule.declared.module,
                })?,
            )
            .slot(
                "operation",
                render(OperationCell {
                    label: operation.label,
                    key: operation.key,
                })?,
            )
            .slot("default", mode_badge(default)?)
            .slot("tenant", tenant)
            .slot(
                "floor",
                if rule.floor {
                    badge(BadgeKind::Warning, "Locked on")?
                } else {
                    render(TextCell {
                        class: "text-sm text-ink-muted",
                        text: "—",
                    })?
                },
            );
        row.dom_id = Some(dom(operation.key));
        row.actions = self.actions(rule, owner)?;
        Ok(row)
    }

    fn window_form(&self, window: &Window, limits: bool) -> Result<Html> {
        let field = |name: &str, label: &str, value: i64, errors: &[String]| {
            render(
                Field::named(name)
                    .id(format!("step-up-{}", name.replace('_', "-")))
                    .label(label)
                    .kind(InputKind::Number)
                    .value(value.to_string())
                    .errors(errors.to_vec()),
            )
        };
        let (id, event, first, second) = if limits {
            (
                "step-up-limits-form",
                "save_limits",
                field(
                    "window_limit_minutes",
                    "Longest window any tenant may set (minutes)",
                    window.window_limit_minutes,
                    &self.limit_errors,
                )?,
                field(
                    "cap_limit_hours",
                    "Longest cap any tenant may set (hours)",
                    window.cap_limit_hours,
                    &[],
                )?,
            )
        } else {
            (
                "step-up-window-form",
                "save_window",
                field(
                    "window_minutes",
                    "Window after each protected action (minutes)",
                    window.tenant_window_minutes,
                    &self.window_errors,
                )?,
                field(
                    "cap_hours",
                    "Ask again after the check, at the latest (hours)",
                    window.tenant_cap_hours,
                    &[],
                )?,
            )
        };
        render(WindowForm {
            id,
            event,
            first,
            second,
            save: render(Button {
                variant: ButtonVariant::Primary,
                id: &format!("{id}-save"),
                button_type: "submit",
                working: "Saving…",
                body: Html::text("Save"),
                ..Button::default()
            })?,
        })
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let owner = self.owner(scope).await?;
        let rules = self.pages.step_up.rules(scope).await?;
        let window = self.pages.step_up.window(scope).await?;
        let search = self.state.search.to_lowercase();
        let mut shown: Vec<&Rule<'_>> = rules
            .iter()
            .filter(|rule| {
                search.is_empty()
                    || rule
                        .declared
                        .operation
                        .label
                        .to_lowercase()
                        .contains(&search)
                    || rule.declared.operation.key.contains(&search)
                    || rule.declared.module.contains(&search)
            })
            .collect();
        if self.state.sort_by.as_deref() == Some("operation") {
            shown.sort_by_key(|rule| rule.declared.operation.label);
        }
        if self.state.sort_dir == Some(SortDir::Desc) {
            shown.reverse();
        }
        let page = self.state.paginate(&shown);
        let rows = page
            .entries
            .iter()
            .map(|rule| self.row(rule, owner))
            .collect::<Result<Vec<_>>>()?;
        let mut table = FlexTable::new("step-up-operations", self.columns.flex_columns(), rows)
            .sorted(self.state.sort_by.as_deref(), self.state.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.actions = true;
        table.caption = "2FA requirements";
        if page.counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No actions can require 2FA yet."
            } else {
                "No operation matches this search."
            })));
        }
        let (kind, message) = self
            .flash
            .as_ref()
            .map_or((StatusKind::Info, ""), |(kind, text)| {
                (*kind, text.as_str())
            });
        let mut flash = FlashMap::default();
        match kind {
            StatusKind::Success => flash.success = message,
            StatusKind::Error => flash.error = message,
            _ => flash.info = message,
        }
        let mut html = render(FlashGroup {
            id: "step-up-flash",
            flash,
        })?;
        html.push(&render(Header {
            title: Html::text("2FA requirements"),
            subtitle: "Set the actions that require 2FA.",
            ..Header::default()
        })?);
        html.push(&render(Card {
            id: "step-up-window",
            title: "Confirmed-access window",
            inner_class: "p-0",
            body: self.window_form(&window, false)?,
            ..Card::default()
        })?);
        if owner {
            html.push(&render(Card {
                id: "step-up-limits",
                title: "Installation maximum (installation owner)",
                inner_class: "p-0",
                body: self.window_form(&window, true)?,
                ..Card::default()
            })?);
        }
        let form = self.state.filters_form();
        html.push(&render(FilterToolbar::new(
            "step-up-filters",
            vec![ToolbarControl::search(
                "step-up-search",
                "search",
                "Search operations",
                &form["search"],
                "Search by operation or module...",
            )],
        ))?);
        let mut card = render(table)?;
        card.push(&render(Pagination::new(
            "step-up-pagination",
            page.counts,
            &PAGE_SIZES,
        ))?);
        html.push(&render(Card {
            id: "step-up-card",
            inner_class: "p-0",
            body: card,
            ..Card::default()
        })?);
        if let Some(pending) = &self.pending {
            let (consequence, confirm) = match pending {
                Pending::Mode { key, mode } => (
                    format!(
                        "{key} will be {} for your organisation.",
                        match mode {
                            None => "back to its default",
                            Some(Mode::Off) => "allowed without a code",
                            Some(Mode::On) => "covered by a recent check",
                            Some(Mode::EveryTime) => "asked for a code every time",
                        }
                    ),
                    "Save requirement",
                ),
                Pending::Floor { key, floor } => (
                    format!(
                        "{key} will {} for every organisation.",
                        if *floor == Some(true) {
                            "stay on whatever an organisation chooses"
                        } else {
                            "follow each organisation's choice"
                        }
                    ),
                    "Save floor",
                ),
            };
            html.push(&render(ConfirmDialog {
                id: "step-up-confirm",
                consequence: &consequence,
                detail: "You will be asked for your own 2FA code, and the change is recorded in the audit log.",
                confirm,
                working: "Saving…",
                confirm_event: "confirm",
                cancel_event: "cancel",
            })?);
        }
        Ok(render(bilibi_ui::Page {
            id: "step-up-index",
            body: html,
            ..bilibi_ui::Page::default()
        })?
        .as_str()
        .to_owned())
    }
}

impl LiveState<Scope, Error> for StepUpPolicy {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => {
                    let get = |key: &str| value.get(key).and_then(|v| v.as_str());
                    match get("type") {
                        Some(event) if event.starts_with("request_") => {
                            let name = get("key").ok_or(Error::Invalid)?;
                            self.pages.step_up.declared(name).ok_or(Error::Invalid)?;
                            let key = name.to_owned();
                            self.flash = None;
                            self.pending = Some(match &event["request_".len()..] {
                                "default" => Pending::Mode { key, mode: None },
                                "lock" => Pending::Floor {
                                    key,
                                    floor: Some(true),
                                },
                                "unlock" => Pending::Floor {
                                    key,
                                    floor: Some(false),
                                },
                                other => Pending::Mode {
                                    key,
                                    mode: Some(Mode::parse(other).ok_or(Error::Invalid)?),
                                },
                            });
                        }
                        Some("confirm") => {
                            if let Some(pending) = self.pending.take() {
                                self.apply(scope, pending).await?;
                            }
                        }
                        Some("cancel") => self.pending = None,
                        Some(event @ ("save_window" | "save_limits")) => {
                            let values = params::string_values(value);
                            self.flash = None;
                            self.save_window(scope, &values, event == "save_limits")
                                .await?;
                        }
                        Some("filters") => {
                            self.state
                                .apply_filters(&params::form_values(value, FILTERS_FORM));
                        }
                        Some("sort") => {
                            if !self.state.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("grid") => {
                            if let ColumnsOutcome::Sort(key) =
                                self.columns.handle(&params::string_values(value))
                            {
                                if !self.state.next_sort(&key) {
                                    return Err(Error::Invalid);
                                }
                            }
                        }
                        Some("page") => self.state.put_page(get("page")),
                        Some("clear-flash") => self.flash = None,
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            Ok(LiveBody {
                html: self.body(scope).await?,
                query: self.state.to_params(),
                ..LiveBody::default()
            })
        })
    }
}

impl Pages {
    pub(crate) async fn step_up_view(
        &self,
        scope: &Scope,
        query: &HashMap<String, String>,
    ) -> Result<String> {
        StepUpPolicy::mount(self, query)?.body(scope).await
    }
}
