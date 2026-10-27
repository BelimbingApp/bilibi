//! `/authz/step-up` (`base/authz.pages`, operation `step-up`): the tenant's
//! two-factor requirements, in two tabs (`tab`, so a refresh keeps it). Settings
//! holds the tenant's window and cap as inline facts that save on commit, and
//! for the installation owner the installation-wide maxima. Actions holds one
//! FlexTable row per step-up operation the mounted modules declared
//! (`bilibi_step_up::declare!`), sorted by module, with its default, this
//! tenant's choice and the installation floor. No Bilimbi counterpart.
//!
//! Every change rechecks `admin.authz.step-up.manage` inside its audited
//! transaction and first passes step-up itself (`base.step_up.policy.update`,
//! on the floor): with no recent check the host sends the browser to confirm
//! and back. An action's change is confirmed first; a setting saves as it is
//! committed, and a refusal stays on its fact.
use crate::web::{render, Pages, TextCell};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_step_up::{Mode, Rule, Window, CAP_RANGE, POLICY_UPDATE, WINDOW_RANGE};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Badge, BadgeKind, BuiltinColumn, Button, Card, ColumnsOutcome, CommitStates,
    CommitStatus, ConfirmDialog, Fact, Facts, FilterToolbar, FlashGroup, FlashMap, FlexRow,
    FlexTable, Header, Html, InlineEdit, InlineField, ListColumns, ListSpec, ListState, Pagination,
    SectionHeading, SortDir, StatusKind, Tab, TableEmpty, Tabs, ToolbarControl,
};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

pub const OPERATION: &str = "step-up";
pub const MANAGE: &str = "admin.authz.step-up.manage";
const PATH: &str = "/authz/step-up";
const PAGE_SIZES: [u64; 4] = [25, 50, 100, 300];
/// The page's views; the first is the default and leaves `tab` out of the address.
const TABS: [(&str, &str); 2] = [("settings", "Settings"), ("actions", "Actions")];

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
            column("floor", "Required for all organisations", false),
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

fn tab(values: &HashMap<String, String>) -> &'static str {
    TABS.iter()
        .find(|(id, _)| values.get("tab").map(String::as_str) == Some(*id))
        .map_or(TABS[0].0, |(id, _)| *id)
}

/// One number on the Settings tab: the tenant's pair, then the installation's.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Setting {
    Minutes,
    Hours,
    MinutesLimit,
    HoursLimit,
}

impl Setting {
    const ALL: [Self; 4] = [
        Self::Minutes,
        Self::Hours,
        Self::MinutesLimit,
        Self::HoursLimit,
    ];
    fn name(self) -> &'static str {
        match self {
            Self::Minutes => "window_minutes",
            Self::Hours => "cap_hours",
            Self::MinutesLimit => "window_limit_minutes",
            Self::HoursLimit => "cap_limit_hours",
        }
    }
    fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|setting| setting.name() == name)
    }
    /// The installation's maxima, which only the installation owner changes.
    fn limit(self) -> bool {
        matches!(self, Self::MinutesLimit | Self::HoursLimit)
    }
    fn minutes(self) -> bool {
        matches!(self, Self::Minutes | Self::MinutesLimit)
    }
    fn label(self) -> &'static str {
        if self.minutes() {
            "Minutes before asking again"
        } else {
            "Hours before always asking again"
        }
    }
    fn hint(self) -> &'static str {
        match self {
            Self::Minutes => "Counts from their last action that requires 2FA: with 30, a half-hour break means a new code.",
            Self::Hours => "Counts from when they entered the code, even if they keep working.",
            _ => "",
        }
    }
    fn dom(self) -> String {
        format!("step-up-{}", self.name().replace('_', "-"))
    }
    /// The stored value, which the editor shows and carries back as its version.
    fn stored(self, window: &Window) -> i64 {
        match self {
            Self::Minutes => window.tenant_window_minutes,
            Self::Hours => window.tenant_cap_hours,
            Self::MinutesLimit => window.window_limit_minutes,
            Self::HoursLimit => window.cap_limit_hours,
        }
    }
    /// The pair to write with `value` in place of this setting. An
    /// organisation's other setting is written at the value in force, its
    /// stored value capped at the server's longest.
    fn pair(self, window: &Window, value: i64) -> (i64, i64) {
        match self {
            Self::Minutes => (value, window.cap_hours),
            Self::Hours => (
                window
                    .tenant_window_minutes
                    .min(window.window_limit_minutes),
                value,
            ),
            Self::MinutesLimit => (value, window.cap_limit_hours),
            Self::HoursLimit => (window.window_limit_minutes, value),
        }
    }
    /// Why `value` cannot be stored, in the page's own words. The facade
    /// checks the same rules again inside the write.
    fn problem(self, window: &Window, value: i64) -> Option<String> {
        let (low, high) = if self.minutes() {
            WINDOW_RANGE
        } else {
            CAP_RANGE
        };
        if !(low..=high).contains(&value) {
            return Some(format!("must be from {low} to {high}"));
        }
        let (minutes, hours) = self.pair(window, value);
        if !self.limit() {
            let limit = if self.minutes() {
                window.window_limit_minutes
            } else {
                window.cap_limit_hours
            };
            if value > limit {
                return Some(format!(
                    "cannot be more than {limit}, the longest this server allows"
                ));
            }
        }
        (hours * 60 < minutes).then(|| {
            if self.minutes() {
                format!("cannot be longer than the {hours} hours before always asking again")
            } else {
                format!("cannot be shorter than the {minutes} minutes before asking again")
            }
        })
    }
}

/// A change waiting for its confirmation.
enum Pending {
    Mode { key: String, mode: Option<Mode> },
    Floor { key: String, floor: Option<bool> },
}

pub(crate) struct StepUpPolicy {
    pages: Pages,
    tab: &'static str,
    state: ListState,
    columns: ListColumns,
    pending: Option<Pending>,
    flash: Option<(StatusKind, String)>,
    statuses: CommitStates,
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
            tab: tab(query),
            state: parse(query)?,
            columns: columns(),
            pending: None,
            flash: None,
            statuses: CommitStates::default(),
        })
    }

    fn query(&self) -> HashMap<String, String> {
        let mut query = self.state.to_params();
        if self.tab != TABS[0].0 {
            query.insert("tab".into(), self.tab.into());
        }
        query
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
                    "This action requires 2FA for all organisations and cannot be turned off here."
                        .into(),
                ));
            }
            Err(error) => return Err(error),
        }
        Ok(())
    }

    /// One committed setting. Its outcome lands on its fact; only a lost
    /// session or a step-up still owed leaves as an error.
    async fn save_setting(&mut self, scope: &Scope, value: &serde_json::Value) -> Result<()> {
        let setting = value["field"]
            .as_str()
            .and_then(Setting::parse)
            .ok_or(Error::Invalid)?;
        let submitted = value["value"].as_str().ok_or(Error::Invalid)?.trim();
        let version = value["version"].as_str().unwrap_or_default();
        let status = self.write(scope, setting, submitted, version).await?;
        self.statuses.put(setting.name(), status);
        Ok(())
    }

    async fn write(
        &self,
        scope: &Scope,
        setting: Setting,
        submitted: &str,
        version: &str,
    ) -> Result<CommitStatus> {
        let refusal = |reason: &str| CommitStatus::refusal(setting.label(), submitted, &[reason]);
        if setting.limit() && !self.owner(scope).await? {
            return Ok(CommitStatus::Error(
                "Only the server owner can change this.".into(),
            ));
        }
        let step_up = self.pages.step_up.clone();
        let window = step_up.window(scope).await?;
        if version != setting.stored(&window).to_string() {
            return Ok(CommitStatus::Error(
                "Someone else changed this setting meanwhile. Check the value shown and enter yours again."
                    .into(),
            ));
        }
        let Ok(value) = submitted.parse::<i64>() else {
            return Ok(refusal(if setting.minutes() {
                "must be a whole number of minutes, like 30"
            } else {
                "must be a whole number of hours, like 6"
            }));
        };
        if let Some(problem) = setting.problem(&window, value) {
            return Ok(refusal(&problem));
        }
        let (minutes, hours) = setting.pair(&window, value);
        let mut client = self.pages.db.get().await?;
        let mut tx = self
            .pages
            .mutations
            .begin(&mut client, scope.mutation_context(POLICY_UPDATE))
            .await?;
        match crate::authorize_key(tx.sql(), scope, MANAGE).await {
            Err(Error::Forbidden) => {
                return Ok(CommitStatus::Error(
                    "You do not have permission to change 2FA settings.".into(),
                ))
            }
            other => other?,
        }
        let target = format!(
            "{} window:{minutes}m cap:{hours}h",
            if setting.limit() {
                "installation"
            } else {
                "tenant"
            }
        );
        step_up.require(scope, POLICY_UPDATE, &target).await?;
        let written = if setting.limit() {
            step_up.set_limits(&mut tx, scope, minutes, hours).await
        } else {
            step_up.set_window(&mut tx, scope, minutes, hours).await
        };
        match written {
            Ok(()) => {
                tx.commit().await?;
                Ok(CommitStatus::Saved)
            }
            // A pair stored before the server's longest was lowered.
            Err(Error::Invalid) => Ok(refusal(
                "does not fit the other setting and the longest this server allows",
            )),
            Err(error) => Err(error),
        }
    }

    fn actions(&self, rule: &Rule<'_>, owner: bool) -> Result<Html> {
        let key = rule.declared.operation.key;
        let id = dom(key);
        let mut html = Html::default();
        if rule.declared.operation.always {
            // Declared `always`: a code every time on every server, so there
            // is nothing a tenant or the owner may change here.
            html.push(&render(TextCell {
                class: "text-xs text-ink-muted",
                text: "Cannot be changed",
            })?);
            return Ok(html);
        }
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
                body: Html::text("Use default"),
                ..Button::default()
            })?);
        }
        if owner {
            let (event, label) = if rule.floor {
                ("request_unlock", "Allow organisation choice")
            } else {
                ("request_lock", "Require for all organisations")
            };
            html.push(&render(Button {
                id: &format!("{id}-floor"),
                button_type: "button",
                event,
                name: "key",
                value: key,
                body: Html::text(label),
                ..Button::default()
            })?);
        }
        Ok(html)
    }

    fn row(&self, rule: &Rule<'_>, owner: bool) -> Result<FlexRow> {
        let operation = rule.declared.operation;
        let default = if operation.always {
            Mode::EveryTime
        } else if operation.default_on {
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
                if rule.declared.operation.always {
                    badge(BadgeKind::Warning, "Always")?
                } else if rule.floor {
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

    fn facts(&self, id: &'static str, settings: &[Setting], window: &Window) -> Result<Html> {
        let items = settings
            .iter()
            .map(|&setting| {
                let stored = setting.stored(window).to_string();
                Ok(Fact {
                    title: setting.label(),
                    id: "",
                    value: render(InlineEdit {
                        field: InlineField::new(
                            setting.dom(),
                            setting.name(),
                            setting.label(),
                            &stored,
                        )
                        .version(&stored)
                        .hint(setting.hint())
                        .status(self.statuses.get(setting.name())),
                    })?,
                })
            })
            .collect::<Result<Vec<_>>>()?;
        render(Facts { id, items })
    }

    fn section(&self, id: &str, title: &str, description: &str, facts: Html) -> Result<Html> {
        let heading = format!("{id}-heading");
        let mut body = render(SectionHeading {
            id: &heading,
            title,
            description,
            ..SectionHeading::default()
        })?;
        body.push(&facts);
        render(Card {
            id,
            class: "mb-6",
            inner_class: "p-5 sm:p-6",
            role: "region",
            labelledby: &heading,
            body,
            ..Card::default()
        })
    }

    async fn settings(&self, scope: &Scope, owner: bool) -> Result<Html> {
        let window = self.pages.step_up.window(scope).await?;
        let mut html = self.section(
            "step-up-window",
            "After someone enters a 2FA code",
            "After either limit is reached, the next protected action requires a new code.",
            self.facts(
                "step-up-window-facts",
                &[Setting::Minutes, Setting::Hours],
                &window,
            )?,
        )?;
        if owner {
            html.push(&self.section(
                "step-up-limits",
                "Longest time any organisation may choose (server owner only)",
                "Organisations cannot exceed these limits.",
                self.facts(
                    "step-up-limits-facts",
                    &[Setting::MinutesLimit, Setting::HoursLimit],
                    &window,
                )?,
            )?);
        }
        Ok(html)
    }

    async fn actions_tab(&mut self, scope: &Scope, owner: bool) -> Result<Html> {
        let rules = self.pages.step_up.rules(scope).await?;
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
        table.caption = "Actions that can require 2FA";
        if page.counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No actions can require 2FA yet."
            } else {
                "No actions match this search."
            })));
        }
        let form = self.state.filters_form();
        let mut html = render(FilterToolbar::new(
            "step-up-filters",
            vec![ToolbarControl::search(
                "step-up-search",
                "search",
                "Search actions",
                &form["search"],
                "Search by action or module...",
            )],
        ))?;
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
        Ok(html)
    }

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let owner = self.owner(scope).await?;
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
            title: Html::text("Step-up 2FA"),
            subtitle: "Choose which actions require 2FA, such as assigning a role.",
            ..Header::default()
        })?);
        // Each tab is this address with its own `tab`, so a refresh or Back
        // returns to it.
        let tab_paths: Vec<(String, String, &str, &str)> = TABS
            .iter()
            .map(|(id, label)| {
                let mut query = self.query();
                query.remove("tab");
                if *id != TABS[0].0 {
                    query.insert("tab".into(), (*id).into());
                }
                (
                    format!("step-up-tab-{id}"),
                    params::address(PATH, &query),
                    *id,
                    *label,
                )
            })
            .collect();
        html.push(&render(Tabs {
            id: "step-up-tabs",
            label: "Step-up 2FA views",
            class: "mb-6",
            tabs: tab_paths
                .iter()
                .map(|(dom, href, id, label)| Tab {
                    id: dom,
                    ..Tab::patch(href, label, self.tab == *id)
                })
                .collect(),
        })?);
        html.push(&if self.tab == "actions" {
            self.actions_tab(scope, owner).await?
        } else {
            self.settings(scope, owner).await?
        });
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
                        "{key} will {}.",
                        if *floor == Some(true) {
                            "require 2FA for every organisation"
                        } else {
                            "follow each organisation's choice"
                        }
                    ),
                    "Save requirement",
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
                LiveInput::Params(values) => {
                    self.tab = tab(values);
                    self.state = parse(values)?;
                }
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
                        Some("save_field") => self.save_setting(scope, value).await?,
                        Some("filters") => {
                            self.state.apply_filters(
                                &params::form_values_checked(
                                    value,
                                    FILTERS_FORM,
                                    params::FIELD_LIMIT,
                                )
                                .map_err(Error::refused)?,
                            );
                        }
                        Some("sort") => {
                            if !self.state.next_sort(get("sort").unwrap_or_default()) {
                                return Err(Error::Invalid);
                            }
                        }
                        Some("grid") => {
                            if let ColumnsOutcome::Sort(key) = self.columns.handle(
                                &params::string_values_checked(value, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            ) {
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
                query: self.query(),
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
