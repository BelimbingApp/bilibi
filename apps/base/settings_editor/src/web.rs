//! `/system/settings`, the operator settings page (Bilimbi
//! `Settings.Web.GroupLive`). The screen owns no rules: `form` decides what a
//! field is, whether it is inherited, what clearing means and what a secret may
//! show; this renders that and hands submissions back.
//!
//! Every write re-asks Authz (`authorize_operator` on the route capability)
//! and rebuilds the field plan from live grants before it acts, so a field
//! whose capability was revoked since the page drew is ignored even when a
//! forged submission names it. Refusals flash and redraw; they never clear
//! the page. Restore confirms through the shared dialog and acts only on the
//! overrides the dialog counted.
//!
//! The company picker (K15) offers Global and the companies Tenancy's
//! organisation directory says this actor may manage
//! (`base.settings.company.manage`, own company or tenant-wide reach). A
//! picked company is reproved on every switch and write: a denied, missing,
//! cross-tenant or archived target is refused with Bilimbi's wording, and an
//! archived company is never offered, since a write into it would be refused.
use crate::{
    form::{self, Field, Plan, Step},
    reveal::{self, Refusal, Reveal},
    COMPANY_MANAGE, GLOBAL_MANAGE,
};
use askama::Template;
use bilibi_authz::authorize_operator;
use bilibi_database::{Db, Error, MutationBoundary, Result};
use bilibi_party_ids::OrganisationId;
use bilibi_settings::{Settings, Target, ValueType};
use bilibi_step_up::StepUp;
use bilibi_tenancy::{NamedOrganisation, PartyProofs, Scope};
use bilibi_ui::{
    choices,
    forms::{Field as Control, InputKind},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    BuiltinColumn, Button, ButtonVariant, ColumnsOutcome, CommitStatus, ConfirmDialog, EmptyState,
    Filter, FilterToolbar, FlashMap, FlexRow, FlexTable, Form, Header, Html, IconButton,
    IconContext, InlineForm, ListColumns, ListSpec, ListState, Pagination, SecretInput, SortDir,
    StatusKind, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    sync::{Arc, LazyLock},
};

const TITLE: &str = "Operator Settings";
const COLUMNS: [(&str, &str); 3] = [("setting", "Setting"), ("value", "Value"), ("area", "Area")];
static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    Arc::new(ListSpec {
        sortable: COLUMNS
            .iter()
            .map(|(key, _)| ((*key).into(), SortDir::Asc))
            .collect(),
        default_sort: Some("setting".into()),
        filters: [("company".into(), Filter::string("global"))].into(),
        omit_defaults: true,
        ..ListSpec::default()
    })
});
fn list(values: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)
}
fn displayed(field: &Field) -> String {
    if field.encrypted() {
        if field.stored_secret {
            bilibi_ui::SECRET_MASK.into()
        } else {
            "Not set".into()
        }
    } else {
        match field.value {
            Value::Bool(true) => "Yes".into(),
            Value::Bool(false) => "No".into(),
            _ => {
                let text = form::input_value(field);
                if text.is_empty() {
                    "Not set".into()
                } else {
                    text
                }
            }
        }
    }
}
const PLATFORM_DENIED: &str = "You do not have permission to manage platform settings.";
const COMPANY_DENIED: &str = "You do not have permission to manage this company's settings.";
const COMPANY_ARCHIVED: &str =
    "This company is archived and read-only, so its settings were not changed.";
const COMPANY_GONE: &str = "That company is no longer available; choose another settings scope.";
const UNAVAILABLE: &str = "Settings are unavailable; nothing was changed.";

pub struct Core {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub boundary: Arc<MutationBoundary>,
    pub parties: Arc<PartyProofs>,
    pub step_up: Arc<StepUp>,
}
pub struct Pages(Arc<Core>);
impl Pages {
    pub fn new(core: Core) -> Self {
        Self(Arc::new(core))
    }
}

struct Editor {
    core: Arc<Core>,
    flash: Option<(StatusKind, String)>,
    /// The overrides the restore dialog counted; restore acts on these only.
    pending_restore: Option<Vec<String>>,
    events: Vec<(&'static str, Value)>,
    /// The picked company scope; `None` is Global. An id, never a proof.
    company: Option<NamedOrganisation>,
    list: ListState,
    columns: ListColumns,
    editing: Option<String>,
    row_status: CommitStatus,
    reveal_pending: Option<String>,
    draft: Option<String>,
}

impl Editor {
    fn new(core: Arc<Core>, values: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            core,
            flash: None,
            pending_restore: None,
            events: Vec::new(),
            company: None,
            list: list(values)?,
            columns: ListColumns::mount(
                "settings-table",
                COLUMNS
                    .iter()
                    .map(|(key, label)| BuiltinColumn::new(key, label).sortable(key))
                    .collect(),
            )
            .map_err(|_| Error::Internal)?,
            editing: None,
            row_status: CommitStatus::None,
            reveal_pending: None,
            draft: None,
        })
    }
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    fn target(&self) -> Target {
        match &self.company {
            Some(company) => Target::Company(company.id),
            None => Target::Global,
        }
    }
    async fn plan(&self, scope: &Scope) -> Result<Plan> {
        let client = self.core.db.get().await?;
        form::plan(&**client, &self.core.settings, scope, self.target()).await
    }
    /// The route capability, asked again (and logged) before any write; a
    /// company scope asks twice more (Bilimbi `scope_writable/1`): may this
    /// actor manage that company's settings, and may the company be written
    /// at all. An archived company is read-only and the page says so rather
    /// than blaming a permission.
    async fn writable(&mut self, scope: &Scope) -> Result<bool> {
        let client = self.core.db.get().await?;
        match authorize_operator(&**client, scope, GLOBAL_MANAGE).await {
            Ok(_) => {}
            Err(Error::Forbidden) => {
                self.flash(StatusKind::Error, PLATFORM_DENIED);
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
        let Some(company) = &self.company else {
            return Ok(true);
        };
        match self
            .core
            .parties
            .organisation_target(&**client, scope, company.id, COMPANY_MANAGE)
            .await
        {
            Ok(_) => {}
            Err(Error::Forbidden) => {
                self.flash(StatusKind::Error, COMPANY_DENIED);
                return Ok(false);
            }
            Err(Error::NotFound) => {
                self.company = None;
                self.flash(StatusKind::Error, COMPANY_GONE);
                return Ok(false);
            }
            Err(error) => return Err(error),
        }
        match self
            .core
            .parties
            .organisation(&**client, scope, company.id)
            .await
        {
            Ok(_) => Ok(true),
            Err(Error::Forbidden) => {
                self.flash(StatusKind::Error, COMPANY_ARCHIVED);
                Ok(false)
            }
            Err(Error::NotFound) => {
                self.company = None;
                self.flash(StatusKind::Error, COMPANY_GONE);
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }
    /// `switch_scope`: the picked id is reproved as a manageable target now,
    /// and an archived company is refused rather than opened read-only, so
    /// the page never shows a scope it cannot write.
    async fn switch(&mut self, scope: &Scope, id: &str) -> Result<()> {
        let Some(id) = id.parse().ok().and_then(|id| OrganisationId::new(id).ok()) else {
            self.flash(StatusKind::Error, "Choose an available settings scope.");
            return Ok(());
        };
        let client = self.core.db.get().await?;
        let company = match self
            .core
            .parties
            .organisation_target(&**client, scope, id, COMPANY_MANAGE)
            .await
        {
            Ok(company) => company,
            Err(Error::Forbidden) | Err(Error::NotFound) => {
                self.flash(StatusKind::Error, COMPANY_DENIED);
                return Ok(());
            }
            Err(error) => return Err(error),
        };
        if company.archived {
            self.flash(StatusKind::Error, COMPANY_ARCHIVED);
            return Ok(());
        }
        self.list
            .filters
            .insert("company".into(), company.id.get().to_string());
        self.company = Some(company);
        self.editing = None;
        self.flash = None;
        self.pending_restore = None;
        Ok(())
    }

    async fn apply_scope(&mut self, scope: &Scope) -> Result<()> {
        let wanted = self
            .list
            .filters
            .get("company")
            .cloned()
            .unwrap_or_else(|| "global".into());
        if wanted == "global" {
            if self.company.is_some() {
                self.editing = None;
                self.flash = None;
                self.pending_restore = None;
            }
            self.company = None;
        } else if self
            .company
            .as_ref()
            .map(|c| c.id.get().to_string())
            .as_deref()
            != Some(&wanted)
        {
            self.switch(scope, &wanted).await?;
            if self.company.is_none() {
                self.list.filters.insert("company".into(), "global".into());
            }
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        let get = |key: &str| payload.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "clear-flash" => self.flash = None,
            "filters" => {
                self.list
                    .apply_filters(&bilibi_ui::params::form_values(payload, "filters"));
                self.apply_scope(scope).await?;
                self.list.filters.insert(
                    "company".into(),
                    self.company
                        .as_ref()
                        .map_or("global".into(), |company| company.id.get().to_string()),
                );
            }
            "page" => self.list.put_page(get("page")),
            "grid" => {
                if let ColumnsOutcome::Sort(key) = self
                    .columns
                    .handle(&bilibi_ui::params::string_values(payload))
                {
                    self.list.next_sort(&key);
                }
            }
            "edit_setting" => {
                self.draft = None;
                self.reveal_pending = None;
                let plan = self.plan(scope).await?;
                if let Some(field) = plan.field(get("key").unwrap_or_default()) {
                    self.editing = Some(field.key.into());
                    self.row_status = CommitStatus::None;
                }
            }
            "cancel_setting" => {
                self.draft = None;
                self.reveal_pending = None;
                self.editing = None;
                self.row_status = CommitStatus::None;
            }
            "save_settings" => {
                self.row_status = CommitStatus::None;
                let presented = scope
                    .clone()
                    .presenting_step_up(get("code").unwrap_or_default());
                self.save(&presented, payload).await?;
                if let Some((kind, message)) = &self.flash {
                    if *kind == StatusKind::Error {
                        self.row_status = CommitStatus::Error(message.clone());
                    } else {
                        self.draft = None;
                        self.editing = None;
                    }
                }
            }
            "request_restore" => {
                let plan = self.plan(scope).await?;
                let overridden: Vec<String> = plan
                    .fields
                    .iter()
                    .filter(|field| field.overridden)
                    .map(|field| field.key.to_owned())
                    .collect();
                if overridden.is_empty() {
                    self.flash(StatusKind::Info, form::restored_message(0));
                } else {
                    self.flash = None;
                    self.pending_restore = Some(overridden);
                }
            }
            "cancel_restore" => self.pending_restore = None,
            "restore_defaults" => self.restore(scope).await?,
            "switch_scope" => match get("scope[company_id]") {
                Some("global") => {
                    self.company = None;
                    self.list.filters.insert("company".into(), "global".into());
                    self.editing = None;
                    self.flash = None;
                    self.pending_restore = None;
                }
                Some(id) => self.switch(scope, id).await?,
                None => self.flash(StatusKind::Error, "Choose an available settings scope."),
            },
            "request_secret_reveal" => {
                let key = get("key").unwrap_or_default();
                let plan = self.plan(scope).await?;
                let client = self.core.db.get().await?;
                let offered = plan
                    .field(key)
                    .is_some_and(|field| field.encrypted() && field.stored_secret);
                if offered
                    && reveal::available(&**client, &plan, &self.core.settings, scope).await?
                {
                    let presented = scope
                        .clone()
                        .presenting_step_up(get("code").unwrap_or_default());
                    self.reveal(&presented, &plan, key).await?;
                } else {
                    // Even a forged request is recorded as a refused reveal.
                    reveal::refuse(&**client, scope, key).await?;
                    self.flash(StatusKind::Error, "Stored value cannot be shown.");
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    async fn save(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        if !self.writable(scope).await? {
            return Ok(());
        }
        // Planned in full before anything is written, so a bad later field
        // cannot leave earlier ones committed by a save that reported failure.
        let plan = self.plan(scope).await?;
        let mut steps = Vec::new();
        for field in &plan.fields {
            if self.editing.as_deref() != Some(field.key) {
                continue;
            }
            let Some(submitted) = payload
                .get(format!("settings[{}]", field.key))
                .and_then(Value::as_str)
            else {
                continue;
            };
            // Keep non-secret drafts when the live transport resets an answered
            // form. Never retain or render a secret submission after refusal.
            self.draft = (!field.encrypted()).then(|| submitted.to_owned());
            match form::decide(field, submitted) {
                Ok(Step::Skip) => {}
                Ok(Step::Write(_))
                    if field.encrypted() && !self.core.settings.encryption_available() =>
                {
                    self.flash(
                        StatusKind::Error,
                        format!(
                            "{}: cannot be changed until the settings encryption key is configured",
                            field.editor.label
                        ),
                    );
                    return Ok(());
                }
                Ok(step) => steps.push((field, step)),
                Err(message) => {
                    self.flash(
                        StatusKind::Error,
                        format!("{}: {message}", field.editor.label),
                    );
                    return Ok(());
                }
            }
        }
        for (field, _) in &steps {
            if let Err(error) = self.require_change_step_up(scope, field).await {
                return self.step_up_refused(error, field);
            }
        }
        let (mut written, mut cleared) = (0, 0);
        if !steps.is_empty() {
            let target = self.target();
            let action = if self.company.is_some() {
                "settings.company.save"
            } else {
                "settings.global.save"
            };
            let mut client = self.core.db.get().await?;
            let mut tx = self
                .core
                .boundary
                .begin(&mut client, scope.mutation_context(action))
                .await?;
            for (field, step) in steps {
                let result = match step {
                    Step::Write(value) => {
                        written += 1;
                        self.core
                            .settings
                            .put(&mut tx, scope, target, field.key, value)
                            .await
                    }
                    Step::Clear => {
                        cleared += 1;
                        self.core
                            .settings
                            .delete(&mut tx, scope, target, field.key)
                            .await
                    }
                    Step::Skip => Ok(()),
                };
                if let Err(error) = result {
                    // Dropping the transaction rolls every step back.
                    return self.refused(error, Some(field.editor.label));
                }
            }
            if let Err(error) = tx.commit().await {
                return self.refused(error, None);
            }
        }
        let kind = if written + cleared == 0 {
            StatusKind::Info
        } else {
            StatusKind::Success
        };
        self.flash(kind, form::saved_message(written, cleared));
        Ok(())
    }

    async fn restore(&mut self, scope: &Scope) -> Result<()> {
        let Some(held) = self.pending_restore.take() else {
            return Ok(());
        };
        if !self.writable(scope).await? {
            return Ok(());
        }
        // Asked now, not read off the page: a save since the dialog opened may
        // already have cleared one, and a revoked field is no longer shown.
        let plan = self.plan(scope).await?;
        let fields: Vec<&Field> = plan
            .fields
            .iter()
            .filter(|field| field.overridden && held.iter().any(|key| key == field.key))
            .collect();
        for field in &fields {
            if let Err(error) = self.require_change_step_up(scope, field).await {
                return match error {
                    Error::StepUpRequired { .. } | Error::Unauthorized => Err(error),
                    error => self.refused(error, Some(field.editor.label)),
                };
            }
        }
        if !fields.is_empty() {
            let target = self.target();
            let action = if self.company.is_some() {
                "settings.company.restore"
            } else {
                "settings.global.restore"
            };
            let mut client = self.core.db.get().await?;
            let mut tx = self
                .core
                .boundary
                .begin(&mut client, scope.mutation_context(action))
                .await?;
            for field in &fields {
                if let Err(error) = self
                    .core
                    .settings
                    .delete(&mut tx, scope, target, field.key)
                    .await
                {
                    return self.refused(error, Some(field.editor.label));
                }
            }
            if let Err(error) = tx.commit().await {
                return self.refused(error, None);
            }
        }
        let kind = if fields.is_empty() {
            StatusKind::Info
        } else {
            StatusKind::Success
        };
        self.flash(kind, form::restored_message(fields.len()));
        Ok(())
    }

    /// A protected field asks for the holder's code before any write. The row
    /// presents the proof on this request; restore retains the host prompt.
    async fn require_change_step_up(&self, scope: &Scope, field: &Field) -> Result<()> {
        let Some(guard) = self.core.step_up.guarding_setting(field.key) else {
            return Ok(());
        };
        self.core
            .step_up
            .require(
                scope,
                guard.operation.key,
                &format!("setting:{}", field.key),
            )
            .await
    }
    fn step_up_refused(&mut self, error: Error, field: &Field) -> Result<()> {
        match error {
            Error::Unauthorized => Err(error),
            Error::StepUpRequired { reason, .. } => {
                let message = match reason {
                    "invalid_code" => "That code was not accepted. Enter the current code from your authenticator app.",
                    "locked" => "Too many wrong codes. Wait a while, then try again with a fresh code.",
                    "not_enrolled" => "Set up 2FA before changing this setting.",
                    "impersonating" => "This setting cannot be changed while viewing as someone else.",
                    _ => "Enter the current 6-digit code from your authenticator app.",
                };
                self.flash(StatusKind::Error, message);
                Ok(())
            }
            error => self.refused(error, Some(field.editor.label)),
        }
    }

    /// A refused or failed write is a flash, never a page-clearing error; only
    /// a lost session propagates.
    fn refused(&mut self, error: Error, label: Option<&str>) -> Result<()> {
        let message = match error {
            Error::Unauthorized => return Err(Error::Unauthorized),
            Error::Forbidden if self.company.is_some() => COMPANY_ARCHIVED.to_owned(),
            Error::Forbidden => PLATFORM_DENIED.to_owned(),
            Error::NotFound if self.company.is_some() => {
                self.company = None;
                COMPANY_GONE.to_owned()
            }
            Error::Invalid => match label {
                Some(label) => format!("{label}: the value was refused; nothing was changed."),
                None => UNAVAILABLE.to_owned(),
            },
            _ => UNAVAILABLE.to_owned(),
        };
        self.flash(StatusKind::Error, message);
        Ok(())
    }

    /// Shows one stored secret after Base Step-up; a required check stays in
    /// the cell, without releasing plaintext or losing the current editor.
    async fn reveal(&mut self, scope: &Scope, plan: &Plan, key: &str) -> Result<()> {
        let client = self.core.db.get().await?;
        let reveal = Reveal {
            settings: &self.core.settings,
            step_up: &self.core.step_up,
        };
        match reveal.reveal(&**client, plan, scope, key).await {
            Ok(value) => {
                let duration = plan
                    .field(key)
                    .map_or(10_000, |field| field.editor.reveal_duration_ms);
                self.flash = None;
                self.reveal_pending = None;
                self.row_status = CommitStatus::None;
                self.events.push((
                    "secret:reveal",
                    json!({"id": format!("input-{}", form::dom(key)), "value": value, "duration_ms": duration}),
                ));
            }
            Err(Refusal::StepUp(error)) => {
                if let Some(field) = plan.field(key) {
                    self.editing = Some(key.into());
                    self.reveal_pending = Some(key.into());
                    self.step_up_refused(error, field)?;
                    if let Some((_, message)) = &self.flash {
                        self.row_status = CommitStatus::Error(message.clone());
                    }
                }
            }
            Err(Refusal::AuditUnavailable) => self.flash(
                StatusKind::Error,
                "The reveal could not be recorded, so the value was not shown. Try again later.",
            ),
            Err(Refusal::Refused) => {
                self.flash(StatusKind::Error, "This stored value cannot be shown.");
            }
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        self.list.filters.insert(
            "company".into(),
            self.company
                .as_ref()
                .map_or("global".into(), |company| company.id.get().to_string()),
        );
        let plan = self.plan(scope).await?;
        let client = self.core.db.get().await?;
        let can_reveal = reveal::available(&**client, &plan, &self.core.settings, scope).await?;
        let keyed = self.core.settings.encryption_available();
        let company = self.company.is_some();
        let enrolled = self.core.step_up.enrolled(scope).await?;
        let mut protected_override = false;
        let mut rows = Vec::new();
        let wanted = self.list.search.trim().to_lowercase();
        let mut fields: Vec<_> = plan
            .fields
            .iter()
            .filter(|field| {
                wanted.is_empty()
                    || [field.editor.label, field.key, field.editor.help]
                        .iter()
                        .any(|text| text.to_lowercase().contains(&wanted))
                    || (!field.encrypted()
                        && (form::input_value(field).to_lowercase().contains(&wanted)
                            || displayed(field).to_lowercase().contains(&wanted)))
            })
            .collect();
        let sort_key = self.list.sort_by.as_deref().unwrap_or("setting");
        let sort_value = |field: &Field| {
            match sort_key {
                "value" => displayed(field),
                "area" => field.editor.group.into(),
                _ => field.editor.label.into(),
            }
            .to_lowercase()
        };
        fields.sort_by(|left, right| {
            let order = sort_value(left)
                .cmp(&sort_value(right))
                .then_with(|| left.key.cmp(right.key));
            if self.list.sort_dir == Some(SortDir::Desc) {
                order.reverse()
            } else {
                order
            }
        });
        // Restore covers all visible settings, even while a search hides rows.
        for field in &plan.fields {
            if let Some(guard) = self.core.step_up.guarding_setting(field.key) {
                protected_override |= field.overridden
                    && !enrolled
                    && self
                        .core
                        .step_up
                        .required(scope, guard.operation.key)
                        .await?;
            }
        }
        let page = self.list.paginate(&fields);
        let counts = page.counts;
        for field in page.entries {
            let editing = self.editing.as_deref() == Some(field.key);
            let revealing = editing && self.reveal_pending.as_deref() == Some(field.key);
            let control = control(
                field,
                can_reveal,
                keyed,
                if editing { self.draft.as_deref() } else { None },
            )?;
            let save = render(Button {
                variant: ButtonVariant::Primary,
                button_type: "submit",
                body: Html::text(if revealing {
                    "Show stored value"
                } else {
                    "Save"
                }),
                ..Button::default()
            })?;
            let cancel = render(Button {
                button_type: "button",
                event: "cancel_setting",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            let protected = revealing
                || if let Some(guard) = self.core.step_up.guarding_setting(field.key) {
                    self.core
                        .step_up
                        .required(scope, guard.operation.key)
                        .await?
                } else {
                    false
                };
            let (body, actions) = if protected {
                let mut step = bilibi_ui::StepUp::new(enrolled, "/system/settings");
                step.body = control;
                step.code = render(
                    bilibi_ui::CodeInput::new(
                        format!("code-{}", field.dom()),
                        "code",
                        "6-digit code from your authenticator app",
                        6,
                    )
                    .numeric(),
                )?;
                step.actions = save;
                step.secondary_actions = cancel;
                (render(step)?, Html::EMPTY)
            } else {
                (control, [save, cancel].into_iter().collect())
            };
            let form_id = format!("form-{}", field.dom());
            let mut body = body;
            body.push(&render(
                Control::named("key")
                    .kind(InputKind::Hidden)
                    .value(field.key),
            )?);
            let editor = render(Form {
                id: &form_id,
                event: if revealing {
                    "request_secret_reveal"
                } else {
                    "save_settings"
                },
                class: "ui-compact-controls",
                body,
                actions,
                ..Form::default()
            })?;
            let cell = render(InlineForm {
                compact: self.columns.mode() == bilibi_ui::flex_table::RowMode::Compact,
                id: format!("value-{}", field.dom()),
                key: field.key.into(),
                label: field.editor.label.into(),
                value: displayed(field),
                hint: if field.overridden {
                    "Set here".into()
                } else if let Some(source) = field.source {
                    format!("Inherited from {}", source.as_str())
                } else {
                    "Inherited from the default".into()
                },
                editing,
                open_event: "edit_setting".into(),
                cancel_event: "cancel_setting".into(),
                form: editor,
                status: if editing {
                    self.row_status.clone()
                } else {
                    CommitStatus::None
                },
            })?;
            let mut row = FlexRow::new(field.key)
                .slot(
                    "setting",
                    render(bilibi_ui::InlineEdit {
                        field: bilibi_ui::InlineField::new(
                            format!("name-{}", field.dom()),
                            field.key,
                            field.editor.label,
                            field.editor.label,
                        )
                        .readonly()
                        .hint(
                            if self.columns.mode() == bilibi_ui::flex_table::RowMode::Compact {
                                ""
                            } else {
                                field.editor.help
                            },
                        ),
                    })?,
                )
                .slot("value", cell)
                .slot("area", Html::text(field.editor.group));
            row.dom_id = Some(format!("setting-{}", field.dom()));
            rows.push(row);
        }
        let mut table = FlexTable::new("settings-table", self.columns.flex_columns(), rows)
            .sorted(self.list.sort_by.as_deref(), self.list.sort_dir);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        let empty = if plan.fields.is_empty() && !plan.withheld.is_empty() {
            EmptyState::forbidden(withheld_wording(&plan.withheld))
        } else if !wanted.is_empty() {
            EmptyState::missing(
                "No settings match your search.",
                "Try another name, description or value.",
            )
        } else {
            EmptyState::missing("There are no settings to change here.", "")
        };
        table.empty = Some(bilibi_ui::TableEmpty::State(empty));
        let table = render(table)?;
        let mut controls = vec![ToolbarControl::search(
            "settings-search",
            "search",
            "Search settings",
            &self.list.search,
            "Search settings",
        )];
        let pagination = render(Pagination::new(
            "settings-pagination",
            counts,
            &SPEC.page_sizes,
        ))?;
        let header = render(Header {
            title: Html::text(TITLE),
            subtitle: "",
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "settings-pin",
                nav_pin: "nav-admin-system-settings",
                ..IconButton::new("bilimbi-pin", "Pin Operator Settings to sidebar")
            })?,
            ..Header::default()
        })?;
        let scopes = if plan.capabilities.contains(COMPANY_MANAGE) {
            let companies = self
                .core
                .parties
                .selectable_organisations(&**client, scope, COMPANY_MANAGE)
                .await?;
            let mut options = vec![("Global".to_owned(), "global".to_owned())];
            options.extend(
                companies
                    .iter()
                    .map(|company| (company.name.clone(), company.id.get().to_string())),
            );
            let options: Vec<(&str, &str)> = options
                .iter()
                .map(|(label, value)| (label.as_str(), value.as_str()))
                .collect();
            controls.push(ToolbarControl::select(
                "settings-company",
                "company",
                "Settings scope",
                &self
                    .company
                    .as_ref()
                    .map_or("global".to_owned(), |company| company.id.get().to_string()),
                choices(&options),
            ));
            if companies.is_empty() {
                render(Withheld {
                    state: EmptyState::missing(
                        "No companies are available to manage.",
                        "Ask an administrator for access to a company.",
                    )
                    .id("company-settings-no-targets"),
                })?
            } else {
                Html::default()
            }
        } else {
            render(Withheld {
                state: EmptyState::forbidden("manage company settings")
                    .id("company-settings-withheld"),
            })?
        };
        let toolbar = render(FilterToolbar::new("settings-search-form", controls))?;
        let dialog = match &self.pending_restore {
            Some(held) => {
                let scope_name = match &self.company {
                    Some(company) => format!("{} settings", company.name),
                    None => "Server settings".to_owned(),
                };
                let consequence = format!(
                    "This resets {} in {scope_name} to their defaults, including any hidden by your search.",
                    form::count(held.len(), "changed setting").unwrap_or_default()
                );
                render(ConfirmDialog {
                    id: "restore-defaults-confirm",
                    consequence: &consequence,
                    detail: "Each of those settings returns to the value it inherits. This cannot be undone.",
                    confirm: "Restore",
                    working: "Restoring…",
                    confirm_event: "restore_defaults",
                    cancel_event: "cancel_restore",
                })?
            }
            None => Html::EMPTY,
        };
        let message = |kind| match &self.flash {
            Some((k, text)) if *k == kind => text.as_str(),
            _ => "",
        };
        let view = SettingsView {
            flash: FlashMap {
                error: message(StatusKind::Error),
                warning: message(StatusKind::Warning),
                success: message(StatusKind::Success),
                info: message(StatusKind::Info),
            },
            header,
            scopes,
            table,
            toolbar,
            pagination,
            restore: if protected_override {
                render(bilibi_ui::StepUp::new(false, "/system/settings"))?
            } else {
                render(Button {
                    id: "settings-restore",
                    button_type: "button",
                    event: "request_restore",
                    body: Html::text(if company {
                        "Clear overrides"
                    } else {
                        "Restore Defaults"
                    }),
                    ..Button::default()
                })?
            },
            dialog,
        };
        view.render().map_err(|_| Error::Internal)
    }
}

/// The action `EmptyState::forbidden` completes into "You do not have
/// permission to ...", naming each capability the actor lacks.
fn withheld_wording(capabilities: &std::collections::BTreeSet<&str>) -> String {
    let names: Vec<&str> = capabilities.iter().copied().collect();
    match names.as_slice() {
        [one] => format!("see the settings in this group, each of which needs {one}"),
        [rest @ .., last] => format!(
            "see the settings in this group; each setting needs its own permission, and this group uses {} and {last}",
            rest.join(", ")
        ),
        [] => String::new(),
    }
}

fn control(field: &Field, can_reveal: bool, keyed: bool, draft: Option<&str>) -> Result<Html> {
    let id = format!("input-{}", field.dom());
    let name = format!("settings[{}]", field.key);
    if field.definition.value_type == ValueType::Boolean {
        render(
            Control::named(name)
                .id(id)
                .kind(InputKind::Select)
                .label(field.editor.label)
                .options(choices(&[("Yes", "true"), ("No", "false")]))
                .value(
                    draft
                        .map(str::to_owned)
                        .unwrap_or_else(|| form::input_value(field)),
                )
                .wrapper_class("mb-0"),
        )
    } else if field.encrypted() {
        let mut secret = SecretInput::new(id, name, field.editor.label)
            .autocomplete("off")
            .wrapper_class("mb-0");
        if field.stored_secret {
            secret = secret.stored();
            if can_reveal {
                secret = secret.stored_reveal("request_secret_reveal", field.key);
            }
        }
        if !keyed {
            secret = secret.readonly();
        }
        render(secret)
    } else {
        render(
            Control::named(name)
                .id(id)
                .label(field.editor.label)
                .value(
                    draft
                        .map(str::to_owned)
                        .unwrap_or_else(|| form::input_value(field)),
                )
                .hint("Clear this field to use the inherited value.")
                .autocomplete("off")
                .wrapper_class("mb-0"),
        )
    }
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    scopes: Html,
    table: Html,
    toolbar: Html,
    pagination: Html,
    restore: Html,
    dialog: Html,
}
#[derive(Template)]
#[template(
    source = r#"{% import "lists.html" as lists %}{% call lists::empty_state(e=state) %}{% endcall %}"#,
    ext = "html"
)]
struct Withheld {
    state: EmptyState,
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "global").then_some(TITLE)
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "global" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut editor = Editor::new(self.0.clone(), request.query)?;
            editor.apply_scope(scope).await?;
            Ok(PageBody {
                html: editor.render(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "global" {
                return Err(Error::NotFound);
            }
            let mut editor = Editor::new(self.0.clone(), request.query)?;
            editor
                .apply_scope(request.scope.ok_or(Error::Unauthorized)?)
                .await?;
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(editor);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Editor {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let before = self.flash.clone();
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.list = list(values)?;
                    self.apply_scope(request.scope).await?;
                }
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.render(request.scope).await?;
            let notice = match &self.flash {
                Some((_, message)) if self.flash != before => message.clone(),
                _ => String::new(),
            };
            Ok(LiveBody {
                html,
                query: self.list.to_params(),
                notice,
                events: std::mem::take(&mut self.events),
                ..LiveBody::default()
            })
        })
    }
}
