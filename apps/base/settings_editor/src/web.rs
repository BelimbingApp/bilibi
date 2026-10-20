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
use bilibi_settings::{ScopeKind, Settings, Target, ValueType};
use bilibi_step_up::StepUp;
use bilibi_tenancy::{NamedOrganisation, PartyProofs, Scope};
use bilibi_ui::{
    choices,
    forms::{Field as Control, InputKind},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    Button, ButtonVariant, ConfirmDialog, EmptyState, FlashMap, Header, Html, IconButton,
    IconContext, SecretInput, StatusKind,
};
use serde_json::{json, Value};
use std::sync::Arc;

const TITLE: &str = "Operator Settings";
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
}

impl Editor {
    fn new(core: Arc<Core>) -> Self {
        Self {
            core,
            flash: None,
            pending_restore: None,
            events: Vec::new(),
            company: None,
        }
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
        self.company = Some(company);
        self.flash = None;
        self.pending_restore = None;
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, payload: &Value) -> Result<()> {
        let get = |key: &str| payload.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "save_settings" => self.save(scope, payload).await?,
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
                    self.reveal(scope, &plan, key).await?;
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
            let Some(submitted) = payload
                .get(format!("settings[{}]", field.key))
                .and_then(Value::as_str)
            else {
                continue;
            };
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
                return self.step_up_refused(error, field);
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

    /// A protected field asks for the holder's code before any write; a check
    /// the session owes goes up to the host, which sends the browser to
    /// confirm and back to this page, where the person saves again.
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
            Error::StepUpRequired { .. } | Error::Unauthorized => Err(error),
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

    /// Shows one stored secret after Base Step-up. A check the session owes
    /// goes up to the host, which sends the browser to confirm and back to
    /// this page, where the person asks again.
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
                self.events.push((
                    "secret:reveal",
                    json!({"id": format!("input-{}", form::dom(key)), "value": value, "duration_ms": duration}),
                ));
            }
            Err(Refusal::StepUp(error)) => return Err(error),
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

    async fn render(&self, scope: &Scope) -> Result<String> {
        let plan = self.plan(scope).await?;
        let client = self.core.db.get().await?;
        let can_reveal = reveal::available(&**client, &plan, &self.core.settings, scope).await?;
        let keyed = self.core.settings.encryption_available();
        let company = self.company.is_some();
        let enrolled = self.core.step_up.enrolled(scope).await?;
        let mut fields = Vec::new();
        let mut protected_override = false;
        for field in &plan.fields {
            let needs_setup = if let Some(guard) = self.core.step_up.guarding_setting(field.key) {
                !enrolled
                    && self
                        .core
                        .step_up
                        .required(scope, guard.operation.key)
                        .await?
            } else {
                false
            };
            let mut view = card(field, can_reveal, keyed, company)?;
            if needs_setup {
                protected_override |= field.overridden;
                let mut step = bilibi_ui::StepUp::new(false, "/system/settings");
                step.body = view.control;
                view.control = render(step)?;
            }
            fields.push(view);
        }
        let empty = if fields.is_empty() {
            let state = if plan.withheld.is_empty() && company {
                EmptyState::missing("There are no company settings to change.", "")
                    .id("company-settings-empty")
            } else if plan.withheld.is_empty() {
                EmptyState::missing("There are no settings to change here.", "")
                    .id("settings-group-operator-empty")
            } else {
                EmptyState::forbidden(withheld_wording(&plan.withheld))
                    .id("settings-group-operator-withheld")
            };
            render(EmptyCard { state })?
        } else {
            Html::EMPTY
        };
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
            let picker = Control::named("scope[company_id]")
                .id("settings-company")
                .kind(InputKind::Select)
                .label("Settings scope")
                .options(choices(&options))
                .value(
                    self.company
                        .as_ref()
                        .map_or("global".to_owned(), |company| company.id.get().to_string()),
                );
            render(ScopePicker {
                picker,
                none: if companies.is_empty() {
                    Some(
                        EmptyState::missing(
                            "No companies are available to manage.",
                            "Ask an administrator for access to a company.",
                        )
                        .id("company-settings-no-targets"),
                    )
                } else {
                    None
                },
            })?
        } else {
            render(Withheld {
                state: EmptyState::forbidden("manage company settings")
                    .id("company-settings-withheld"),
            })?
        };
        let dialog = match &self.pending_restore {
            Some(held) => {
                let consequence = format!(
                    "{} on this page will be removed.",
                    form::count(held.len(), "override").unwrap_or_default()
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
            fields,
            empty,
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
            save: render(Button {
                variant: ButtonVariant::Primary,
                id: "settings-save",
                button_type: "submit",
                body: Html::text("Save"),
                ..Button::default()
            })?,
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

fn card(field: &Field, can_reveal: bool, keyed: bool, company: bool) -> Result<FieldCard> {
    let dom = field.dom();
    let id = format!("input-{dom}");
    let name = format!("settings[{}]", field.key);
    let control = if field.definition.value_type == ValueType::Boolean {
        render(
            Control::named(name)
                .id(id)
                .kind(InputKind::Checkbox)
                .checked(field.value == Value::Bool(true))
                .label("Enabled")
                .wrapper_class("mb-0 mt-2.5"),
        )?
    } else if field.encrypted() {
        let mut secret = SecretInput::new(id, name, field.editor.label)
            .autocomplete("off")
            .wrapper_class("mt-2.5");
        secret.class = "w-full rounded-md border border-line bg-surface px-3 py-1.5 text-sm text-ink shadow-xs transition placeholder:text-ink-faint focus:border-brand-strong focus:outline-none focus:ring-2 focus:ring-brand-strong/30".into();
        if field.stored_secret {
            secret = secret.stored();
            if can_reveal {
                secret = secret.stored_reveal("request_secret_reveal", field.key);
            }
        }
        if !keyed {
            secret = secret.readonly();
        }
        render(secret)?
    } else {
        render(PlainInput {
            kind: match field.definition.value_type {
                ValueType::Integer | ValueType::Float => "number",
                _ => "text",
            },
            float: field.definition.value_type == ValueType::Float,
            id: &id,
            name: &name,
            value: form::input_value(field),
        })?
    };
    // Bilimbi `source_note/2`: an inherited value names the scope it comes
    // from; the declared default is worded as such.
    let note = match field.source {
        _ if field.overridden => String::new(),
        Some(ScopeKind::Global) if company => "Inherited from global".to_owned(),
        Some(kind) if company => format!("Inherited from {}", kind.as_str()),
        _ => "Inherited from the default".to_owned(),
    };
    Ok(FieldCard {
        dom,
        label: field.editor.label,
        help: field.editor.help,
        control,
        overridden: field.overridden,
        badge: if company {
            "Company override"
        } else {
            "Set here"
        },
        note,
        encrypted: if !field.encrypted() {
            ""
        } else if keyed {
            "Stored encrypted; leave it unchanged to keep the current value."
        } else {
            "Stored encrypted; read-only until an encryption key is set up."
        },
    })
}

struct FieldCard {
    dom: String,
    label: &'static str,
    help: &'static str,
    control: Html,
    overridden: bool,
    badge: &'static str,
    note: String,
    encrypted: &'static str,
}

#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    scopes: Html,
    fields: Vec<FieldCard>,
    empty: Html,
    restore: Html,
    save: Html,
    dialog: Html,
}
/// Bilimbi renders a bare input under the card's own label here, not the
/// labelled `input` component; `step="any"` lets a fractional value submit.
#[derive(Template)]
#[template(
    source = r#"<input type="{{ kind }}" id="{{ id }}" name="{{ name }}" value="{{ value }}"{% if float %} step="any"{% endif %} autocomplete="off" class="mt-2.5 w-full rounded-md border border-line bg-surface px-3 py-1.5 text-sm text-ink shadow-xs transition placeholder:text-ink-faint focus:border-brand-strong focus:outline-none focus:ring-2 focus:ring-brand-strong/30" />"#,
    ext = "html"
)]
struct PlainInput<'a> {
    kind: &'a str,
    float: bool,
    id: &'a str,
    name: &'a str,
    value: String,
}
#[derive(Template)]
#[template(
    source = r#"{% import "lists.html" as lists %}<div class="rounded-xl border border-line bg-surface px-4 py-8">{% call lists::empty_state(e=state) %}{% endcall %}</div>"#,
    ext = "html"
)]
struct EmptyCard {
    state: EmptyState,
}
#[derive(Template)]
#[template(
    source = r#"{% import "forms.html" as forms %}{% import "lists.html" as lists %}<form id="settings-scope-form" data-event="switch_scope" data-change class="mt-4">{% call forms::input(field=picker) %}{% endcall %}</form>{% if let Some(e) = none %}{% call lists::empty_state(e=e) %}{% endcall %}{% endif %}"#,
    ext = "html"
)]
struct ScopePicker {
    picker: Control,
    none: Option<EmptyState>,
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
        (operation == "global").then_some("Operator Settings")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "global" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            Ok(PageBody {
                html: Editor::new(self.0.clone()).render(scope).await?,
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
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(Editor::new(self.0.clone()));
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for Editor {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let before = self.flash.clone();
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.render(request.scope).await?;
            let notice = match &self.flash {
                Some((_, message)) if self.flash != before => message.clone(),
                _ => String::new(),
            };
            Ok(LiveBody {
                html,
                notice,
                events: std::mem::take(&mut self.events),
                ..LiveBody::default()
            })
        })
    }
}
