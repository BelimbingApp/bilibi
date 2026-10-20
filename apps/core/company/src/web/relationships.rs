//! K6: the relationship screens over K5's catalog and links (Bilimbi
//! `Company.Web.RelationshipsLive` and the Relationships section of
//! `Company.Web.ShowLive`, generalized to the party model).
//!
//! - `/companies/relationship-types` (operation `roles`, `admin.company.list`):
//!   the tenant's relationship-role catalog: create, rename and retire, with
//!   retired roles shown on request. A role describes a business relationship
//!   only; no access grant follows it.
//! - `/companies/{id}/relationships` (`company`, `admin.company.view`): one
//!   company's directed links to organisations and persons with their
//!   direction, dates and status; add, edit dates, end and unlink, and a history
//!   switch that brings ended and unlinked links back.
//! - Embed `company.relationships` (slot `record.relationships`, operation
//!   `panel`, `admin.company.view`): the Relationships card a record page
//!   mounts as a panel with path `{panel, kind: organisation|person, id}`.
//!   The company detail page hosts it for the company; Person's detail page
//!   (K8) hosts the same embed for a person.
//!
//! Every write is a K5 contract (`core.company.relationship_role.*`,
//! `core.company.relationship.*`, ADR 01), called with the sealed scope; the
//! `can_update` flag only decides which controls render. A denial, an archived company and a stale
//! version are flash messages in Bilimbi's words, never a fatal `Error`.
use crate::{
    organisation, relationship_roles::RelationshipRole, relationships::Direction,
    relationships::Entry, CompanyService, FieldError, WriteError, CREATE, LIST, UPDATE, VIEW,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_database::{Db, Error, Result};
use bilibi_menu::{MenuItem, Requirement};
use bilibi_module_registry::{
    contributions::{
        Access, Embed, EmbedContext, Method, Moved, PageWrites, Pending, Route, EMBEDS, MOVED,
        PAGE_WRITES, ROUTES,
    },
    Contribution, Provenance,
};
use bilibi_party::{
    CalendarDate, EffectiveDates, OrganisationId, PartyId, PersonId, RelationshipId,
    RelationshipRoleId,
};
use bilibi_person::PersonService;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        self, LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, PageBody,
        PageFuture, PageRequest,
    },
    ActionLink, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card,
    Choice, ConfirmDialog, CreateButton, Field, FlashGroup, FlashMap, FlexRow, FlexTable, Header,
    Html, IconButton, IconKind, InputKind, ListColumns, Modal, Page, PageVariant, RadioGroup,
    RecordLink, RowMode, SectionHeading, StatusKind, TableEmpty,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};
use tokio_postgres::GenericClient;

pub const SERVICE: &str = "core/company.relationship_pages";
const FORBIDDEN: &str = "You do not have permission to change company administration data.";
const ARCHIVED: &str = "This company is archived and read-only, so nothing was changed.";
const OTHER_ARCHIVED: &str =
    "The other company is archived and read-only, so the relationship was not changed.";
const PERSONS: i64 = 100;

macro_rules! provenance {
    () => {
        Provenance {
            module: "core/company",
            source: file!(),
            line: line!(),
        }
    };
}
// Every write these screens make is a K5 contract (ADR 01).
inventory::submit! { Contribution::new(PAGE_WRITES, SERVICE, provenance!(), &PageWrites {
    service: SERVICE,
    contracts: &[
        ("save_role", "core.company.relationship_role.create"),
        ("save_role", "core.company.relationship_role.update"),
        ("retire", "core.company.relationship_role.retire"),
        ("save_relationship", "core.company.relationship.assign"),
        ("save_relationship", "core.company.relationship.update"),
        ("save_relationship", "core.company.relationship.end"),
        ("unlink", "core.company.relationship.unlink"),
    ],
    pending: Pending::None,
}) }
inventory::submit! {
    Contribution::new(ROUTES, "core/company.relationship-types", provenance!(), &Route {
        method: Method::Get, path: "/companies/relationship-types", service: SERVICE, operation: "roles", access: Access::Capability(LIST), framed: true,
    })
}
// The catalog's address before Bilimbi named it relationship types.
inventory::submit! {
    Contribution::new(MOVED, "core/company.relationship-roles", provenance!(), &Moved {
        from: "/companies/relationship-roles", to: "/companies/relationship-types",
    })
}
inventory::submit! {
    Contribution::new(ROUTES, "core/company.relationships", provenance!(), &Route {
        method: Method::Get, path: "/companies/{id}/relationships", service: SERVICE, operation: "company", access: Access::Capability(VIEW), framed: true,
    })
}
// The Relationships card of a record page. The host names the party in the
// panel path; the embed reads under Company's view capability whichever
// module's page mounts it. The company detail page mounts the first by
// name; Person's detail page discovers the second by its record kind and
// mounts it with its own path (`person_id`, `kinds: person`).
inventory::submit! {
    Contribution::new(EMBEDS, "company.relationships", provenance!(), &Embed {
        slot: "record.relationships", context: EmbedContext::Record { kind: "*" },
        service: SERVICE, operation: "panel", access: Access::Capability(VIEW),
    })
}
inventory::submit! {
    Contribution::new(EMBEDS, "company.person-relationships", provenance!(), &Embed {
        slot: "record.relationships", context: EmbedContext::Record { kind: "core/person" },
        service: SERVICE, operation: "panel", access: Access::Capability(VIEW),
    })
}
bilibi_menu::contribute!(
    "core/company",
    MenuItem::page(
        "admin.company.relationship-type",
        "Relationship Roles",
        "/companies/relationship-types"
    )
    .under("admin.company")
    .requires(Requirement::Capability(LIST))
    .order(30)
);

/// The page service (`core/company.relationship_pages`).
pub struct RelationshipPages {
    pub db: Db,
    pub company: Arc<CompanyService>,
    pub persons: Arc<PersonService>,
    pub contracts: Arc<PageContracts>,
}
struct Shared {
    db: Db,
    company: Arc<CompanyService>,
    persons: Arc<PersonService>,
    contracts: Arc<PageContracts>,
}
impl Shared {
    /// One write of these screens through the contract its event declared.
    /// A session that must sign in again or step up is the host's to answer;
    /// any other refusal comes back in the facade's shape for the flash.
    async fn write(
        &self,
        scope: &Scope,
        credential: &str,
        event: &'static str,
        contract: &'static str,
        input: Value,
    ) -> Result<std::result::Result<(), WriteError>> {
        let call = Call {
            scope,
            credential,
            service: SERVICE,
            event,
            contract,
        };
        match self.contracts.submit(&call, input).await {
            Ok(_) => Ok(Ok(())),
            Err(ContractError::Backend(
                error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
            )) => Err(error),
            Err(error) => Ok(Err(refusal(error))),
        }
    }
}
/// The K5 contracts word the owner's `WriteError` as fields
/// (`relationship_operations`); this reads them back.
fn refusal(error: ContractError) -> WriteError {
    match error {
        ContractError::NotFound | ContractError::Backend(Error::NotFound) => WriteError::NotFound,
        ContractError::Forbidden => WriteError::Access(Error::Forbidden),
        ContractError::InvalidInput(fields) => {
            if fields.contains_key("version") {
                WriteError::Stale
            } else if fields.contains_key("source_organisation_id") {
                WriteError::Archived
            } else if fields.contains_key("id") {
                WriteError::InUse
            } else {
                WriteError::Invalid(
                    fields
                        .iter()
                        .flat_map(|(field, messages)| {
                            let field = [
                                "code",
                                "name",
                                "inverse_name",
                                "role_id",
                                "target",
                                "target_kind",
                                "target_id",
                                "effective_from",
                                "effective_to",
                                "retired_on",
                            ]
                            .into_iter()
                            .find(|known| known == field)
                            .unwrap_or("base");
                            messages
                                .iter()
                                .map(move |message| FieldError::new(field, message.clone()))
                        })
                        .collect(),
                )
            }
        }
        ContractError::Backend(error) => WriteError::Access(error),
        _ => WriteError::Access(Error::Internal),
    }
}
/// A form's optional calendar days, as the contracts take them.
fn days(input: &mut Value, dates: &EffectiveDates) {
    if let Some(from) = dates.from() {
        input["effective_from"] = json!(from.as_str());
    }
    if let Some(to) = dates.to() {
        input["effective_to"] = json!(to.as_str());
    }
}
impl RelationshipPages {
    fn shared(&self) -> Arc<Shared> {
        Arc::new(Shared {
            db: self.db.clone(),
            company: self.company.clone(),
            persons: self.persons.clone(),
            contracts: self.contracts.clone(),
        })
    }
    async fn state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        request.scope.ok_or(Error::Unauthorized)?;
        let id = |key: &str| {
            request
                .path
                .get(key)
                .and_then(|v| v.parse().ok())
                .filter(|id: &i64| *id > 0)
                .ok_or(Error::Invalid)
        };
        Ok(match request.operation {
            "roles" => Box::new(RolesPage {
                shared: self.shared(),
                columns: role_columns()?,
                include_retired: false,
                modal: None,
                pending_retire: None,
                flash: Flash::default(),
            }),
            "company" => Box::new(LinksPage {
                shared: self.shared(),
                company: OrganisationId::new(id("id")?).map_err(|_| Error::Invalid)?,
                columns: link_columns("company-relationships", true)?,
                history: false,
                modal: None,
                pending_unlink: None,
                flash: Flash::default(),
            }),
            "panel" => {
                let panel = request
                    .path
                    .get("panel")
                    .filter(|id| !id.is_empty() && id.len() <= 128)
                    .ok_or(Error::Invalid)?
                    .clone();
                // `kind` + `id` is this module's own host; `kinds: person` +
                // `person_id` is how Person's detail page names its record.
                let kind = request.path.get("kind").map(String::as_str);
                let kinds = request.path.get("kinds").map(String::as_str);
                let party = match (kind, kinds) {
                    (Some("organisation"), None) => PartyId::Organisation(
                        OrganisationId::new(id("id")?).map_err(|_| Error::Invalid)?,
                    ),
                    (Some("person"), None) => {
                        PartyId::Person(PersonId::new(id("id")?).map_err(|_| Error::Invalid)?)
                    }
                    (None, Some("person")) => PartyId::Person(
                        PersonId::new(id("person_id")?).map_err(|_| Error::Invalid)?,
                    ),
                    _ => return Err(Error::Invalid),
                };
                if request.path.len() > 4 || !request.query.is_empty() {
                    return Err(Error::Invalid);
                }
                Box::new(Panel {
                    shared: self.shared(),
                    columns: link_columns(&format!("{panel}-table"), false)?,
                    id: panel,
                    party,
                })
            }
            _ => return Err(Error::NotFound),
        })
    }
}
impl page::Page<Scope, Error> for RelationshipPages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "roles" => Some("Relationship Roles"),
            "company" => Some("Relationships"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation == "panel" {
                return Err(Error::NotFound);
            }
            let mut state = self.state(&request).await?;
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let body = state
                .update(LiveRequest {
                    scope,
                    credential: "",
                    input: LiveInput::Refresh,
                })
                .await?;
            // Bilimbi titles the relationships page after its company.
            let title = match request.operation {
                "company" => {
                    let id: i64 = request
                        .path
                        .get("id")
                        .and_then(|v| v.parse().ok())
                        .ok_or(Error::Invalid)?;
                    let client = self.db.get().await?;
                    organisation::identity(&**client, scope, id)
                        .await?
                        .map(|company| format!("{} — Relationships", company.display_name))
                }
                _ => None,
            };
            Ok(PageBody {
                html: body.html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move { Ok(Some(self.state(&request).await?)) })
    }
}

fn render(value: &impl Template) -> Result<Html> {
    Html::render(value).map_err(|_| Error::Internal)
}
async fn allowed(client: &impl GenericClient, scope: &Scope, key: &str) -> Result<bool> {
    match bilibi_authz::authorize_key(client, scope, key).await {
        Ok(()) => Ok(true),
        Err(Error::Forbidden) => Ok(false),
        Err(e) => Err(e),
    }
}
/// Whether this actor may change `company` now: reach over it, and the
/// company unarchived. The same two checks guard every write's opening.
async fn writable(client: &impl GenericClient, scope: &Scope, company: i64) -> Result<Writable> {
    match organisation::authorize_target(client, scope, company, UPDATE).await {
        Ok(_) => {}
        Err(Error::Forbidden) => return Ok(Writable::Forbidden),
        Err(e) => return Err(e),
    }
    match organisation::require_writable(client, scope, company, false).await {
        Ok(_) => Ok(Writable::Yes),
        Err(WriteError::Archived) => Ok(Writable::Archived),
        Err(WriteError::NotFound) => Err(Error::NotFound),
        Err(WriteError::Access(e)) => Err(e),
        Err(_) => Err(Error::Internal),
    }
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Writable {
    Yes,
    Forbidden,
    Archived,
}

/// The page's flash: one success or one error at a time, cleared by the next
/// intent, rendered through the shared flash group and inside an open dialog.
#[derive(Default)]
struct Flash {
    success: String,
    error: String,
}
impl Flash {
    fn map(&self) -> FlashMap<'_> {
        FlashMap {
            success: &self.success,
            error: &self.error,
            ..FlashMap::default()
        }
    }
    fn render(&self) -> Result<Html> {
        render(&FlashGroup {
            id: "flash-group",
            flash: self.map(),
        })
    }
    fn error(&mut self, message: impl Into<String>) {
        self.success.clear();
        self.error = message.into();
    }
    fn success(&mut self, message: impl Into<String>) {
        self.error.clear();
        self.success = message.into();
    }
    fn clear(&mut self) {
        self.success.clear();
        self.error.clear();
    }
}
/// Access errors other than denial propagate; `Unauthorized` ends the page.
fn access(error: Error, flash: &mut Flash) -> Result<()> {
    match error {
        Error::Forbidden => {
            flash.error(FORBIDDEN);
            Ok(())
        }
        other => Err(other),
    }
}
/// A form's or button's values, without the event name.
fn values_of(value: &Value) -> HashMap<String, String> {
    let mut values = bilibi_ui::params::string_values(value);
    values.remove("type");
    values
}
fn get<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}
fn errors_for(errors: &[FieldError], field: &str) -> Vec<String> {
    errors
        .iter()
        .filter(|e| e.field == field)
        .map(|e| e.message.clone())
        .collect()
}
fn page(id: &str, variant: PageVariant, body: Html) -> Result<String> {
    Ok(render(&Page {
        id,
        variant,
        body,
        ..Page::default()
    })?
    .as_str()
    .to_owned())
}

#[derive(Template)]
#[template(
    source = r#"<code class="text-xs font-medium">{{ text }}</code>"#,
    ext = "html"
)]
struct CodeCell<'a> {
    text: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<span class="{{ class }}">{{ text }}</span>"#,
    ext = "html"
)]
struct TextCell<'a> {
    class: &'a str,
    text: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<div class="flex items-center gap-2">{{ body }}</div>"#,
    ext = "html"
)]
struct Actions {
    body: Html,
}
#[derive(Template)]
#[template(source = r#"<div id="{{ id }}">{{ body }}</div>"#, ext = "html")]
struct PanelRoot<'a> {
    id: &'a str,
    body: Html,
}
#[derive(Template)]
#[template(source = r#"<div class="{{ class }}">{{ body }}</div>"#, ext = "html")]
struct Wrap<'a> {
    class: &'a str,
    body: Html,
}
#[derive(Template)]
#[template(
    source = r#"<button type="button" id="{{ id }}" data-event="{{ event }}" data-id="{{ record }}" class="text-xs text-ink-subtle hover:text-ink hover:underline">{{ label }}</button>"#,
    ext = "html"
)]
struct TextAction<'a> {
    id: String,
    event: &'a str,
    record: i64,
    label: &'a str,
}
#[derive(Template)]
#[template(
    source = r#"<form id="{{ id }}" data-event="{{ event }}"{% if !submit_event.is_empty() %} data-change data-submit-event="{{ submit_event }}"{% endif %} class="mt-4 space-y-4">{% for field in fields %}{{ field }}{% endfor %}<div class="mt-6 flex justify-end gap-2">{{ cancel }}{{ save }}</div></form>"#,
    ext = "html"
)]
/// `submit_event` names a validating form's submission (Bilimbi's
/// `phx-submit` beside `phx-change`). Neither event is `save`, which the
/// transport reserves for the inline-edit shape.
struct Form<'a> {
    id: &'a str,
    event: &'a str,
    submit_event: &'a str,
    fields: Vec<Html>,
    cancel: Html,
    save: Html,
}
fn form_buttons(save_disabled: bool) -> Result<(Html, Html)> {
    Ok((
        render(&Button {
            id: "relationship-form-cancel",
            event: "close_modal",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?,
        render(&Button {
            id: "relationship-form-save",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            working: "Saving…",
            disabled: save_disabled,
            body: Html::text("Save"),
            ..Button::default()
        })?,
    ))
}
fn status(kind: BadgeKind, text: &str) -> Result<Html> {
    render(&Badge::status(kind, text))
}

// ---------------------------------------------------------------------------
// The relationship-role catalog.

fn role_columns() -> Result<ListColumns> {
    ListColumns::mount(
        "relationship-types",
        vec![
            BuiltinColumn::new("code", "Code"),
            BuiltinColumn::new("name", "Name"),
            BuiltinColumn::new("inverse_name", "Inverse Name"),
            BuiltinColumn::new("status", "Status"),
        ],
    )
    .map_err(|_| Error::Internal)
}
struct RoleModal {
    editing: Option<RelationshipRole>,
    values: HashMap<String, String>,
    errors: Vec<FieldError>,
}
struct RolesPage {
    shared: Arc<Shared>,
    columns: ListColumns,
    include_retired: bool,
    modal: Option<RoleModal>,
    pending_retire: Option<RelationshipRole>,
    flash: Flash,
}
#[derive(Template)]
#[template(
    source = r#"{{ badge }}{% if let Some(since) = since %}<span class="block text-xs text-ink-subtle">since {{ since }}</span>{% endif %}"#,
    ext = "html"
)]
struct RoleStatus<'a> {
    badge: Html,
    since: Option<&'a str>,
}
fn role_row(role: &RelationshipRole, mode: RowMode, can_update: bool) -> Result<FlexRow> {
    let id = role.id.get();
    let mut row = FlexRow::new(id.to_string());
    row.dom_id = Some(format!("relationship-role-{id}"));
    row = row
        .slot("code", render(&CodeCell { text: &role.code })?)
        .slot(
            "name",
            render(&TextCell {
                class: "font-medium text-ink-strong",
                text: &role.name,
            })?,
        )
        .slot(
            "inverse_name",
            render(&TextCell {
                class: "text-sm text-ink",
                text: &role.inverse_name,
            })?,
        )
        .slot(
            "status",
            render(&RoleStatus {
                badge: if role.retired() {
                    status(BadgeKind::Neutral, "retired")?
                } else {
                    status(BadgeKind::Success, "active")?
                },
                since: role
                    .retired_on
                    .as_ref()
                    .filter(|_| mode == RowMode::Normal)
                    .map(CalendarDate::as_str),
            })?,
        );
    if can_update {
        let mut actions = Html::EMPTY;
        if !role.retired() {
            actions.push(&render(&TextAction {
                id: format!("retire-relationship-role-{id}"),
                event: "request_retire",
                record: id,
                label: "Retire",
            })?);
        }
        let label = format!("Edit {}", role.name);
        let dom = format!("edit-relationship-role-{id}");
        let value = id.to_string();
        let mut edit = IconButton::new("edit", &label);
        edit.id = &dom;
        edit.event = "edit";
        edit.name = "id";
        edit.value = &value;
        actions.push(&render(&edit)?);
        row.actions = render(&Actions { body: actions })?;
    }
    Ok(row)
}
impl RolesPage {
    async fn draw(&mut self, scope: &Scope) -> Result<LiveBody> {
        let client = self.shared.db.get().await?;
        let can_create = allowed(&**client, scope, CREATE).await?;
        let can_update = allowed(&**client, scope, UPDATE).await?;
        let roles = self
            .shared
            .company
            .relationship_roles(scope, self.include_retired)
            .await?;
        let mut html = self.flash.render()?;
        let mut actions = render(&BackLink {
            id: "relationship-types-back",
            href: "/companies",
            title: "Back to companies",
            ..BackLink::default()
        })?;
        if can_create {
            actions.push(&render(&CreateButton {
                noun: "Relationship Type",
                id: "new-relationship-role-btn",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        html.push(&render(&Header {
            title: Html::text("Relationship Roles"),
            subtitle: "Set how a company can relate to other companies and people, such as vendor or customer.",
            actions: render(&Wrap {
                class: "flex flex-wrap items-center gap-2 sm:gap-3",
                body: actions,
            })?,
            ..Header::default()
        })?);
        html.push(&render(&Wrap {
            class: "mb-4 flex flex-wrap items-center gap-2",
            body: render(&Button {
                id: "relationship-types-retired",
                event: "toggle_retired",
                button_type: "button",
                variant: if self.include_retired {
                    ButtonVariant::Primary
                } else {
                    ButtonVariant::Secondary
                },
                body: Html::text("Show retired"),
                ..Button::default()
            })?,
        })?);
        let rows = roles
            .iter()
            .map(|role| role_row(role, self.columns.mode(), can_update))
            .collect::<Result<Vec<_>>>()?;
        let mut table = FlexTable::new("relationship-types", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Relationship Roles";
        table.framed = false;
        table.actions = can_update;
        if roles.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No relationship roles defined yet.",
            )));
        }
        html.push(&render(&Card {
            id: "relationship-types-card",
            inner_class: "p-0",
            body: render(&table)?,
            ..Card::default()
        })?);
        if let Some(modal) = &self.modal {
            html.push(&self.role_modal(modal)?);
        } else if let Some(role) = &self.pending_retire {
            html.push(&render(&ConfirmDialog {
                id: "retire-relationship-role-confirm",
                consequence: &format!("Relationship role “{}” will be retired.", role.name),
                detail: "No new relationship can use it. Every existing relationship keeps it and stays visible as history.",
                confirm: "Retire",
                working: "Retiring…",
                confirm_event: "retire",
                cancel_event: "cancel_retire",
            })?);
        }
        Ok(LiveBody {
            html: page("relationship-types-index", PageVariant::List, html)?,
            rows: 1,
            ..LiveBody::default()
        })
    }
    fn role_modal(&self, modal: &RoleModal) -> Result<Html> {
        let value = |key: &str| modal.values.get(key).cloned().unwrap_or_default();
        let mut fields = Vec::new();
        let mut code = Field::named("code")
            .id("relationship-role-code")
            .label("Code")
            .placeholder("e.g. vendor, reseller")
            .hint("Lowercase letters, digits, '_', '-' and '.'; fixed once created.")
            .errors(errors_for(&modal.errors, "code"))
            .required();
        code = match &modal.editing {
            Some(role) => code.value(&role.code).disabled(),
            None => code.value(value("code")),
        };
        fields.push(render(&code)?);
        for (key, label, placeholder, hint) in [
            (
                "name",
                "Name",
                "e.g. Vendor",
                "What the related party is to this company.",
            ),
            (
                "inverse_name",
                "Inverse Name",
                "e.g. Customer",
                "What this company is to the related party.",
            ),
        ] {
            fields.push(render(
                &Field::named(key)
                    .id(format!("relationship-role-{}", key.replace('_', "-")))
                    .label(label)
                    .placeholder(placeholder)
                    .hint(hint)
                    .value(match &modal.editing {
                        Some(role) if !modal.values.contains_key(key) => {
                            if key == "name" {
                                role.name.clone()
                            } else {
                                role.inverse_name.clone()
                            }
                        }
                        _ => value(key),
                    })
                    .attr("maxlength", "160")
                    .errors(errors_for(&modal.errors, key))
                    .required(),
            )?);
        }
        let (cancel, save) = form_buttons(false)?;
        let body = render(&Form {
            id: "relationship-role-form",
            event: "save_role",
            submit_event: "",
            fields,
            cancel,
            save,
        })?;
        let (title, description) = match &modal.editing {
            Some(_) => (
                "Edit Relationship Type",
                "Renaming keeps the code and existing relationships.",
            ),
            None => (
                "New Relationship Type",
                "Add a way a company can relate to another company or a person.",
            ),
        };
        render(&Modal {
            id: "relationship-role-modal",
            title,
            description,
            cancel_event: "close_modal",
            flash: Some(self.flash.map()),
            body,
            ..Modal::default()
        })
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let client = self.shared.db.get().await?;
        let kind = get(value, "type");
        if kind != "grid" {
            self.flash.clear();
        }
        match kind {
            "grid" => {
                self.columns.handle(&values_of(value));
            }
            "toggle_retired" => self.include_retired = !self.include_retired,
            "new" => {
                if !allowed(&**client, scope, CREATE).await? {
                    return access(Error::Forbidden, &mut self.flash);
                }
                self.modal = Some(RoleModal {
                    editing: None,
                    values: HashMap::new(),
                    errors: Vec::new(),
                });
            }
            "edit" => {
                if !allowed(&**client, scope, UPDATE).await? {
                    return access(Error::Forbidden, &mut self.flash);
                }
                match self
                    .shared
                    .company
                    .relationship_role(scope, role_id(value)?)
                    .await
                {
                    Ok(role) => {
                        self.modal = Some(RoleModal {
                            editing: Some(role),
                            values: HashMap::new(),
                            errors: Vec::new(),
                        })
                    }
                    Err(Error::NotFound) => self.flash.error("Relationship role not found."),
                    Err(e) => return access(e, &mut self.flash),
                }
            }
            "close_modal" => self.modal = None,
            "save_role" => {
                let Some(modal) = self.modal.as_mut() else {
                    return Ok(());
                };
                modal.values = values_of(value);
                let text = |key: &str| modal.values.get(key).cloned().unwrap_or_default();
                match &modal.editing {
                    Some(role) => {
                        let input = json!({"role_id": role.id.get(), "version": role.version, "name": text("name"), "inverse_name": text("inverse_name")});
                        match self
                            .shared
                            .write(scope, credential, "save_role", "core.company.relationship_role.update", input)
                            .await?
                        {
                            Ok(()) => {
                                self.modal = None;
                                self.flash.success("Relationship role updated successfully.");
                            }
                            Err(WriteError::Stale) => self.flash.error(
                                "The role changed since it was opened. Close the dialog and try again.",
                            ),
                            Err(WriteError::NotFound) => {
                                self.modal = None;
                                self.flash.error("Relationship role not found.");
                            }
                            Err(WriteError::Invalid(errors)) => modal.errors = errors,
                            Err(WriteError::Access(e)) => return access(e, &mut self.flash),
                            Err(_) => self.flash.error("Could not update the relationship role."),
                        }
                    }
                    None => {
                        let input = json!({"code": text("code"), "name": text("name"), "inverse_name": text("inverse_name")});
                        match self
                            .shared
                            .write(
                                scope,
                                credential,
                                "save_role",
                                "core.company.relationship_role.create",
                                input,
                            )
                            .await?
                        {
                            Ok(()) => {
                                self.modal = None;
                                self.flash
                                    .success("Relationship role created successfully.");
                            }
                            Err(WriteError::Invalid(errors)) => modal.errors = errors,
                            Err(WriteError::Access(e)) => return access(e, &mut self.flash),
                            Err(_) => self.flash.error("Could not create the relationship role."),
                        }
                    }
                }
            }
            "request_retire" => {
                if !allowed(&**client, scope, UPDATE).await? {
                    return access(Error::Forbidden, &mut self.flash);
                }
                match self
                    .shared
                    .company
                    .relationship_role(scope, role_id(value)?)
                    .await
                {
                    Ok(role) if role.retired() => self
                        .flash
                        .error("That relationship role is already retired."),
                    Ok(role) => self.pending_retire = Some(role),
                    Err(Error::NotFound) => self.flash.error("Relationship role not found."),
                    Err(e) => return access(e, &mut self.flash),
                }
            }
            "cancel_retire" => self.pending_retire = None,
            "retire" => {
                let Some(role) = self.pending_retire.take() else {
                    return Ok(());
                };
                let input = json!({"role_id": role.id.get(), "version": role.version});
                match self
                    .shared
                    .write(
                        scope,
                        credential,
                        "retire",
                        "core.company.relationship_role.retire",
                        input,
                    )
                    .await?
                {
                    Ok(()) => self.flash.success(
                        "Relationship role retired. Existing relationships keep it as history.",
                    ),
                    Err(WriteError::Stale) => self
                        .flash
                        .error("The role changed since it was opened. Reload and try again."),
                    Err(WriteError::NotFound) => self.flash.error("Relationship role not found."),
                    Err(WriteError::Invalid(_)) => self
                        .flash
                        .error("That relationship role is already retired."),
                    Err(WriteError::Access(e)) => return access(e, &mut self.flash),
                    Err(_) => self.flash.error("Could not retire the relationship role."),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
}
fn role_id(value: &Value) -> Result<RelationshipRoleId> {
    RelationshipRoleId::new(get(value, "id").parse().map_err(|_| Error::Invalid)?)
        .map_err(|_| Error::Invalid)
}
fn link_id(value: &Value) -> Result<RelationshipId> {
    RelationshipId::new(get(value, "id").parse().map_err(|_| Error::Invalid)?)
        .map_err(|_| Error::Invalid)
}
impl LiveState<Scope, Error> for RolesPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                self.event(request.scope, request.credential, value).await?;
            }
            self.draw(request.scope).await
        })
    }
}

// ---------------------------------------------------------------------------
// A company's relationships, and the Relationships card of a record page.

// Bilimbi RelationshipsLive's date editor also supplies the panel's columns.
bilibi_ui::record_fields! { mod link_fields {
    EFFECTIVE_FROM: bilibi_ui::RecordField::new("effective_from", "Effective From"),
    EFFECTIVE_TO: bilibi_ui::RecordField::new("effective_to", "Effective To"),
} }

/// Bilimbi's built-ins; the first names the related party, which the party
/// model lets be a person.
fn link_columns(id: &str, page: bool) -> Result<ListColumns> {
    let columns = if page {
        vec![
            BuiltinColumn::new("other", "Related Party"),
            BuiltinColumn::new("direction", "Direction"),
            BuiltinColumn::new("role", "Relationship Role"),
            BuiltinColumn::new("effective", "Effective Period"),
            BuiltinColumn::new("status", "Status"),
        ]
    } else {
        vec![
            BuiltinColumn::new("other", "Party"),
            BuiltinColumn::new("role", "Relationship Role"),
            BuiltinColumn::new("direction", "Direction"),
            BuiltinColumn::new("effective", "Effective"),
            BuiltinColumn::new("status", "Status"),
        ]
    };
    ListColumns::mount(id, columns)
        .map(|columns| columns.with_record_fields(link_fields::ALL.iter().copied()))
        .map_err(|_| Error::Internal)
}
/// The page header whose subtitle is the company's code, as the source sets
/// it in monospace.
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::header(subtitle_html=code, actions=header_actions) %}{{ title }}{% endcall %}"#,
    ext = "html"
)]
struct CodeHeader {
    title: Html,
    code: Html,
    header_actions: Html,
}
#[derive(Template)]
#[template(
    source = r#"{{ name }}{% if let Some(code) = code %}<code class="block text-xs text-ink-subtle">{{ code }}</code>{% elif person %}<span class="block text-xs text-ink-subtle">Person</span>{% endif %}"#,
    ext = "html"
)]
struct PartyCell<'a> {
    name: Html,
    code: Option<&'a str>,
    person: bool,
}
#[derive(Template)]
#[template(
    source = r#"<span class="font-medium">{{ label }}</span>{% if retired %} <span class="text-xs text-ink-subtle">(role retired)</span>{% endif %}"#,
    ext = "html"
)]
struct RoleCell<'a> {
    label: &'a str,
    retired: bool,
}
#[derive(Template)]
#[template(
    source = r#"<span class="text-xs tabular-nums text-ink-subtle">{{ from }} → {{ to }}</span>"#,
    ext = "html"
)]
struct PeriodCell<'a> {
    from: &'a str,
    to: &'a str,
}
fn period(entry: &Entry) -> Result<Html> {
    render(&PeriodCell {
        from: entry
            .relationship
            .dates
            .from()
            .map_or("Always", CalendarDate::as_str),
        to: entry
            .relationship
            .dates
            .to()
            .map_or("Present", CalendarDate::as_str),
    })
}
fn date_cell(date: Option<&CalendarDate>) -> Result<bilibi_ui::Cell> {
    let Some(date) = date else {
        return Ok(bilibi_ui::Cell::text("—"));
    };
    let iso = date.as_str();
    let date = iso.parse().map_err(|_| Error::Internal)?;
    Ok(bilibi_ui::Cell::date(
        iso,
        &bilibi_datetime::DisplayContext::anonymous().render_date(date),
    ))
}

/// Bilimbi's active/inactive judgment, with the history states the party
/// model keeps: an unlinked link and one whose dates are over or not yet.
fn link_status(entry: &Entry) -> Result<Html> {
    let link = &entry.relationship;
    if link.unlinked() {
        status(BadgeKind::Neutral, "unlinked")
    } else if link.current {
        status(BadgeKind::Success, "active")
    } else {
        status(BadgeKind::Neutral, "inactive")
    }
}
/// One link as a row; `link` names the other company (a record page's card),
/// `actions` adds Bilimbi's edit and unlink controls (the administration page).
fn link_row(
    entry: &Entry,
    mode: RowMode,
    link: bool,
    workspace: bool,
    actions: bool,
) -> Result<FlexRow> {
    let id = entry.relationship.id.get();
    let mut row = FlexRow::new(id.to_string());
    row.dom_id = Some(format!("company-relationship-{id}"));
    let name = match (link, entry.other) {
        (true, PartyId::Organisation(other)) => {
            let href = format!("/companies/{}", other.get());
            let record_id = other.get().to_string();
            render(&RecordLink {
                workspace,
                kind: "core/company",
                record_id: &record_id,
                href: &href,
                class: "font-medium text-action hover:underline",
                body: Html::text(&entry.other_name),
            })?
        }
        _ => render(&TextCell {
            class: "font-medium text-ink-strong",
            text: &entry.other_name,
        })?,
    };
    row = row
        .slot(
            "other",
            render(&PartyCell {
                name,
                code: entry
                    .other_code
                    .as_deref()
                    .filter(|_| mode == RowMode::Normal),
                person: matches!(entry.other, PartyId::Person(_)) && mode == RowMode::Normal,
            })?,
        )
        .slot(
            "direction",
            match entry.direction {
                Direction::Outgoing => status(BadgeKind::Plain, "Outgoing")?,
                Direction::Incoming => status(BadgeKind::Neutral, "Incoming")?,
            },
        )
        .slot(
            "role",
            render(&RoleCell {
                label: &entry.label,
                retired: entry.role.retired(),
            })?,
        )
        .slot("effective", period(entry)?)
        .cell(
            "effective_from",
            date_cell(entry.relationship.dates.from())?,
        )
        .cell("effective_to", date_cell(entry.relationship.dates.to())?)
        .slot("status", link_status(entry)?);
    if actions && !entry.relationship.unlinked() {
        let value = id.to_string();
        let edit_id = format!("edit-rel-{id}");
        let mut edit = IconButton::new("edit", "Edit relationship dates");
        edit.id = &edit_id;
        edit.event = "edit";
        edit.name = "id";
        edit.value = &value;
        let unlink_id = format!("unlink-rel-{id}");
        let mut unlink = IconButton::new("unlink", "Unlink relationship");
        unlink.id = &unlink_id;
        unlink.kind = IconKind::Danger;
        unlink.event = "request_unlink";
        unlink.name = "id";
        unlink.value = &value;
        let mut body = render(&edit)?;
        let end_id = format!("end-rel-{id}");
        let mut end = IconButton::new("stop", "End relationship");
        end.id = &end_id;
        end.event = "end";
        end.name = "id";
        end.value = &value;
        body.push(&render(&end)?);
        body.push(&render(&unlink)?);
        row.actions = render(&Actions { body })?;
    }
    Ok(row)
}
fn link_table<'a>(
    columns: &'a ListColumns,
    rows: Vec<FlexRow>,
    caption: &'a str,
    actions: bool,
    empty: &str,
) -> FlexTable<'a> {
    let mut table = FlexTable::new(&columns.id, columns.flex_columns(), rows);
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.caption = caption;
    table.framed = false;
    table.actions = actions;
    if table.rows.is_empty() {
        table.empty = Some(TableEmpty::Content(Html::text(empty)));
    }
    table
}

struct Options {
    companies: Vec<Choice>,
    persons: Vec<Choice>,
    roles: Vec<Choice>,
    /// Whether the reader may list persons at all (Person's own capability).
    persons_readable: bool,
}
struct LinkModal {
    editing: Option<Entry>,
    ending: bool,
    values: HashMap<String, String>,
    errors: Vec<FieldError>,
    options: Options,
}
struct LinksPage {
    shared: Arc<Shared>,
    company: OrganisationId,
    columns: ListColumns,
    history: bool,
    modal: Option<LinkModal>,
    pending_unlink: Option<Entry>,
    flash: Flash,
}
impl LinksPage {
    async fn options(&self, client: &impl GenericClient, scope: &Scope) -> Result<Options> {
        let companies = client
            .query(
                "SELECT id,name,code FROM companies WHERE tenant_id=$1 AND deleted_at IS NULL AND id<>$2 AND status<>'archived' ORDER BY lower(name),id",
                &[&scope.tenant(), &self.company.get()],
            )
            .await?
            .into_iter()
            .map(|row| {
                Choice::new(
                    format!("{} ({})", row.get::<_, String>(1), row.get::<_, String>(2)),
                    row.get::<_, i64>(0).to_string(),
                )
            })
            .collect();
        let (persons, persons_readable) = match self
            .shared
            .persons
            .search(
                scope,
                &bilibi_person::Search {
                    page_size: PERSONS,
                    ..bilibi_person::Search::default()
                },
            )
            .await
        {
            Ok(page) => (
                page.entries
                    .into_iter()
                    .filter(|person| person.system_key.is_none())
                    .map(|person| Choice::new(person.name, person.id.get().to_string()))
                    .collect(),
                true,
            ),
            Err(Error::Forbidden) => (Vec::new(), false),
            Err(e) => return Err(e),
        };
        let roles = self
            .shared
            .company
            .relationship_roles(scope, false)
            .await?
            .into_iter()
            .map(|role| Choice::new(role.name, role.id.get().to_string()))
            .collect();
        Ok(Options {
            companies,
            persons,
            roles,
            persons_readable,
        })
    }
    async fn draw(&mut self, scope: &Scope, workspace: bool) -> Result<LiveBody> {
        let client = self.shared.db.get().await?;
        let company = organisation::identity(&**client, scope, self.company.get())
            .await?
            .ok_or(Error::NotFound)?;
        let writable = writable(&**client, scope, self.company.get()).await?;
        let can_update = writable == Writable::Yes;
        let entries = self
            .shared
            .company
            .company_relationships(scope, self.company, self.history)
            .await?;
        let title = format!("{} — Relationships", company.display_name);
        let back_title = format!("Back to {}", company.display_name);
        let mut html = self.flash.render()?;
        html.push(&render(&CodeHeader {
            title: Html::text(&title),
            code: render(&CodeCell {
                text: &company.code,
            })?,
            header_actions: render(&BackLink {
                id: "relationships-back",
                href: &format!("/companies/{}", company.id.get()),
                title: &back_title,
                ..BackLink::default()
            })?,
        })?);
        if writable == Writable::Archived {
            html.push(&render(&Alert {
                kind: StatusKind::Warning,
                id: "company-archived",
                class: "mt-4",
                body: Html::text("This company is archived and read-only."),
            })?);
        }
        let mut actions = render(&Button {
            id: "relationships-history",
            event: "toggle_history",
            button_type: "button",
            variant: if self.history {
                ButtonVariant::Primary
            } else {
                ButtonVariant::Secondary
            },
            body: Html::text("Show history"),
            ..Button::default()
        })?;
        if can_update {
            actions.push(&render(&Button {
                id: "add-rel-btn",
                event: "new",
                button_type: "button",
                variant: ButtonVariant::Primary,
                body: Html::text("Add Relationship"),
                ..Button::default()
            })?);
        }
        let mut body = render(&Wrap {
            class: "p-5 pb-2 sm:p-6 sm:pb-2",
            body: render(&SectionHeading {
                id: "company-relationships-heading",
                title: "Relationships",
                count: Some(entries.len()),
                actions: render(&Wrap {
                    class: "flex items-center gap-2",
                    body: actions,
                })?,
                ..SectionHeading::default()
            })?,
        })?;
        let rows = entries
            .iter()
            .map(|entry| link_row(entry, self.columns.mode(), false, workspace, can_update))
            .collect::<Result<Vec<_>>>()?;
        body.push(&render(&link_table(
            &self.columns,
            rows,
            "Company Relationships",
            can_update,
            if self.history {
                "No relationships recorded for this company, current or past."
            } else {
                "No company relationships configured yet."
            },
        ))?);
        html.push(&render(&Card {
            id: "company-relationships-card",
            class: "mt-6",
            inner_class: "p-0",
            role: "region",
            labelledby: "company-relationships-heading",
            body,
            ..Card::default()
        })?);
        if let Some(modal) = &self.modal {
            html.push(&self.link_modal(&company.display_name, modal)?);
        } else if let Some(entry) = &self.pending_unlink {
            html.push(&render(&ConfirmDialog {
                id: "unlink-rel-confirm",
                consequence: &format!(
                    "The {} relationship with {} will be unlinked.",
                    entry.label, entry.other_name
                ),
                detail: "Both parties are kept. The relationship stays in the history with its dates and can no longer be edited.",
                confirm: "Unlink",
                working: "Unlinking…",
                confirm_event: "unlink",
                cancel_event: "cancel_unlink",
            })?);
        }
        Ok(LiveBody {
            html: page("company-relationships-page", PageVariant::Detail, html)?,
            rows: 1,
            ..LiveBody::default()
        })
    }
    fn link_modal(&self, company: &str, modal: &LinkModal) -> Result<Html> {
        let value = |key: &str| modal.values.get(key).cloned().unwrap_or_default();
        let mut fields = Vec::new();
        let mut save_disabled = false;
        if modal.editing.is_none() {
            let kind = if value("target_kind") == "person" {
                "person"
            } else {
                "organisation"
            };
            fields.push(render(
                &RadioGroup::new(
                    "relationship-target-kind",
                    "target_kind",
                    vec![
                        Choice::new("Company", "organisation"),
                        Choice::new("Person", "person"),
                    ],
                )
                .label("Related To")
                .value(kind),
            )?);
            let target_errors = errors_for(&modal.errors, "target");
            if kind == "person" {
                let mut field = Field::named("target_person_id")
                    .id("relationship-related-person-id")
                    .kind(InputKind::Select)
                    .label("Related Person")
                    .prompt("Select a person")
                    .options(modal.options.persons.clone())
                    .value(value("target_person_id"))
                    .errors(target_errors)
                    .required();
                if !modal.options.persons_readable {
                    field = field.hint("You do not have permission to list persons.");
                } else if modal.options.persons.is_empty() {
                    field = field.hint("No people have been added yet.");
                }
                save_disabled = modal.options.persons.is_empty();
                fields.push(render(&field)?);
            } else {
                save_disabled = modal.options.companies.is_empty();
                fields.push(render(
                    &Field::named("target_organisation_id")
                        .id("relationship-related-company-id")
                        .kind(InputKind::Select)
                        .label("Related Company")
                        .prompt("Select a company")
                        .options(modal.options.companies.clone())
                        .value(value("target_organisation_id"))
                        .errors(target_errors)
                        .required(),
                )?);
            }
            save_disabled |= modal.options.roles.is_empty();
            fields.push(render(
                &Field::named("role_id")
                    .id("relationship-role-id")
                    .kind(InputKind::Select)
                    .label("Relationship Role")
                    .prompt("Select relationship role")
                    .hint("What the related party is to this company.")
                    .options(modal.options.roles.clone())
                    .value(value("role_id"))
                    .errors(errors_for(&modal.errors, "role_id"))
                    .required(),
            )?);
        }
        for key in [link_fields::EFFECTIVE_FROM.id, link_fields::EFFECTIVE_TO.id] {
            let label = link_fields::get(key).label;
            if modal.ending && key == "effective_from" {
                continue;
            }
            fields.push(render(
                &Field::named(key)
                    .id(format!("relationship-{}", key.replace('_', "-")))
                    .kind(InputKind::Date)
                    .label(label)
                    .value(value(key))
                    .errors(errors_for(&modal.errors, key)),
            )?);
        }
        let (cancel, save) = form_buttons(save_disabled)?;
        let body = render(&Form {
            id: "relationship-form",
            event: "validate",
            submit_event: "save_relationship",
            fields,
            cancel,
            save,
        })?;
        let (title, description) = match &modal.editing {
            Some(entry) if modal.ending => (
                "End Relationship".to_owned(),
                format!("The relationship with {} ends on the chosen date, inclusive; its history is kept.", entry.other_name),
            ),
            Some(entry) => (
                "Edit Relationship Dates".to_owned(),
                format!(
                    "Change the dates of the relationship with {}.",
                    entry.other_name
                ),
            ),
            None => (
                "Add Relationship".to_owned(),
                format!(
                    "Link {company} to another company or a person."
                ),
            ),
        };
        render(&Modal {
            id: "relationship-modal",
            title: &title,
            description: &description,
            cancel_event: "close_modal",
            flash: Some(self.flash.map()),
            body,
            ..Modal::default()
        })
    }
    /// The two checks every write's opening makes again (Bilimbi `write/2`):
    /// reach over the company, and the company unarchived.
    async fn guard(&mut self, client: &impl GenericClient, scope: &Scope) -> Result<bool> {
        Ok(match writable(client, scope, self.company.get()).await? {
            Writable::Yes => true,
            Writable::Forbidden => {
                self.pending_unlink = None;
                self.flash.error(FORBIDDEN);
                false
            }
            Writable::Archived => {
                self.pending_unlink = None;
                self.flash.error(ARCHIVED);
                false
            }
        })
    }
    async fn find(&self, scope: &Scope, id: RelationshipId) -> Result<Option<Entry>> {
        Ok(self
            .shared
            .company
            .company_relationships(scope, self.company, true)
            .await?
            .into_iter()
            .find(|entry| entry.relationship.id == id && !entry.relationship.unlinked()))
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let client = self.shared.db.get().await?;
        let kind = get(value, "type");
        if !matches!(kind, "grid" | "validate") {
            self.flash.clear();
        }
        match kind {
            "grid" => {
                self.columns.handle(&values_of(value));
            }
            "toggle_history" => self.history = !self.history,
            "new" => {
                if !self.guard(&**client, scope).await? {
                    return Ok(());
                }
                let options = self.options(&**client, scope).await?;
                // Bilimbi opens the form with today as the start.
                let today: String = client
                    .query_one("SELECT CURRENT_DATE::text", &[])
                    .await?
                    .get(0);
                self.modal = Some(LinkModal {
                    editing: None,
                    ending: false,
                    values: HashMap::from([
                        ("target_kind".to_owned(), "organisation".to_owned()),
                        ("effective_from".to_owned(), today),
                    ]),
                    errors: Vec::new(),
                    options,
                });
            }
            "edit" | "end" => {
                if !self.guard(&**client, scope).await? {
                    return Ok(());
                }
                match self.find(scope, link_id(value)?).await? {
                    Some(entry) => {
                        let dates = &entry.relationship.dates;
                        let mut values = HashMap::from([
                            (
                                "effective_from".to_owned(),
                                dates
                                    .from()
                                    .map_or(String::new(), |d| d.as_str().to_owned()),
                            ),
                            (
                                "effective_to".to_owned(),
                                dates.to().map_or(String::new(), |d| d.as_str().to_owned()),
                            ),
                        ]);
                        if kind == "end" {
                            let today: String = client
                                .query_one("SELECT CURRENT_DATE::text", &[])
                                .await?
                                .get(0);
                            values.insert("effective_to".into(), today);
                        }
                        self.modal = Some(LinkModal {
                            editing: Some(entry),
                            ending: kind == "end",
                            values,
                            errors: Vec::new(),
                            options: Options {
                                companies: Vec::new(),
                                persons: Vec::new(),
                                roles: Vec::new(),
                                persons_readable: true,
                            },
                        });
                    }
                    None => self.flash.error("Relationship not found."),
                }
            }
            "close_modal" => self.modal = None,
            "validate" => {
                if let Some(modal) = self.modal.as_mut() {
                    let was_person =
                        modal.values.get("target_kind").map(String::as_str) == Some("person");
                    modal.values = values_of(value);
                    // The picker the radio hid keeps nothing it never showed.
                    if was_person
                        != (modal.values.get("target_kind").map(String::as_str) == Some("person"))
                    {
                        modal.errors.retain(|e| e.field != "target");
                    }
                }
            }
            "save_relationship" => {
                if !self.guard(&**client, scope).await? {
                    return Ok(());
                }
                let Some(modal) = self.modal.as_mut() else {
                    return Ok(());
                };
                modal.values = values_of(value);
                if modal.ending {
                    let Some(entry) = modal.editing.as_ref() else {
                        return Err(Error::Internal);
                    };
                    let date = modal
                        .values
                        .get("effective_to")
                        .cloned()
                        .unwrap_or_default();
                    let Ok(date) = CalendarDate::try_from(date) else {
                        modal.errors =
                            vec![FieldError::new("effective_to", "choose a valid end date")];
                        return Ok(());
                    };
                    let input = json!({"relationship_id": entry.relationship.id.get(), "version": entry.relationship.version, "effective_to": date.as_str()});
                    match self
                        .shared
                        .write(
                            scope,
                            credential,
                            "save_relationship",
                            "core.company.relationship.end",
                            input,
                        )
                        .await?
                    {
                        Ok(()) => {
                            self.modal = None;
                            self.flash
                                .success("Relationship ended. Its history is kept.");
                        }
                        Err(error) => self.write_error(error, true),
                    }
                    return Ok(());
                }
                let mut errors = Vec::new();
                let dates = parse_dates(&modal.values, &mut errors);
                match modal.editing.clone() {
                    Some(entry) => {
                        let Some(dates) = dates else {
                            modal.errors = errors;
                            return Ok(());
                        };
                        let mut input = json!({"relationship_id": entry.relationship.id.get(), "version": entry.relationship.version});
                        days(&mut input, &dates);
                        match self
                            .shared
                            .write(
                                scope,
                                credential,
                                "save_relationship",
                                "core.company.relationship.update",
                                input,
                            )
                            .await?
                        {
                            Ok(()) => {
                                self.modal = None;
                                self.flash.success("Relationship updated successfully.");
                            }
                            Err(error) => self.write_error(error, true),
                        }
                    }
                    None => {
                        let text = |key: &str| modal.values.get(key).cloned().unwrap_or_default();
                        let target = if text("target_kind") == "person" {
                            text("target_person_id")
                                .parse()
                                .ok()
                                .and_then(|id| PersonId::new(id).ok())
                                .map(PartyId::Person)
                        } else {
                            text("target_organisation_id")
                                .parse()
                                .ok()
                                .and_then(|id| OrganisationId::new(id).ok())
                                .map(PartyId::Organisation)
                        };
                        if target.is_none() {
                            errors.push(FieldError::new("target", "can't be blank"));
                        }
                        let role_id = text("role_id")
                            .parse()
                            .ok()
                            .and_then(|id| RelationshipRoleId::new(id).ok());
                        if role_id.is_none() {
                            errors.push(FieldError::new("role_id", "can't be blank"));
                        }
                        let (Some(target), Some(role_id), Some(dates)) = (target, role_id, dates)
                        else {
                            modal.errors = errors;
                            return Ok(());
                        };
                        let (target_kind, target_id) = match target {
                            PartyId::Organisation(id) => ("organisation", id.get()),
                            PartyId::Person(id) => ("person", id.get()),
                        };
                        let mut input = json!({"source_organisation_id": self.company.get(), "target_kind": target_kind, "target_id": target_id, "role_id": role_id.get()});
                        days(&mut input, &dates);
                        match self
                            .shared
                            .write(
                                scope,
                                credential,
                                "save_relationship",
                                "core.company.relationship.assign",
                                input,
                            )
                            .await?
                        {
                            Ok(()) => {
                                self.modal = None;
                                self.flash.success("Relationship established successfully.");
                            }
                            Err(error) => self.write_error(error, false),
                        }
                    }
                }
            }
            "request_unlink" => {
                if !self.guard(&**client, scope).await? {
                    return Ok(());
                }
                match self.find(scope, link_id(value)?).await? {
                    Some(entry) => self.pending_unlink = Some(entry),
                    None => self.flash.error("Relationship not found."),
                }
            }
            "cancel_unlink" => self.pending_unlink = None,
            "unlink" => {
                let Some(entry) = self.pending_unlink.take() else {
                    return Ok(());
                };
                if !self.guard(&**client, scope).await? {
                    return Ok(());
                }
                let input = json!({"relationship_id": entry.relationship.id.get(), "version": entry.relationship.version});
                match self
                    .shared
                    .write(
                        scope,
                        credential,
                        "unlink",
                        "core.company.relationship.unlink",
                        input,
                    )
                    .await?
                {
                    Ok(()) => self
                        .flash
                        .success("Relationship unlinked. It stays in the history."),
                    Err(error) => self.write_error(error, true),
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    /// A refused or failed write in Bilimbi's words. `existing` is a write to
    /// a link that already exists, whose `Invalid` can only name the other
    /// company; a new link's field errors go back to its form.
    fn write_error(&mut self, error: WriteError, existing: bool) {
        match error {
            WriteError::Access(Error::Forbidden) => self.flash.error(FORBIDDEN),
            WriteError::Access(_) => self.flash.error("The relationship was not saved. Reload the page and try again."),
            WriteError::Archived => self.flash.error(ARCHIVED),
            WriteError::Stale => self.flash.error(
                "The relationship changed since it was opened. Your draft is kept; reload before saving.",
            ),
            WriteError::NotFound => {
                self.modal = None;
                self.flash
                    .error("That relationship had already been unlinked.");
            }
            WriteError::InUse => self.flash.error("Could not change the relationship."),
            WriteError::Invalid(errors) => {
                if let Some(modal) = self.modal.as_mut().filter(|_| !existing || errors.iter().any(|e| e.field != "target")) {
                    modal.errors = errors;
                } else if errors.iter().any(|e| e.field == "target") {
                    self.flash.error(OTHER_ARCHIVED);
                } else {
                    self.flash.error("Could not change the relationship.");
                }
            }
        }
    }
}
/// The form's two optional calendar days; a bad day or a reversed pair is a
/// field error in the source's words.
fn parse_dates(
    values: &HashMap<String, String>,
    errors: &mut Vec<FieldError>,
) -> Option<EffectiveDates> {
    let mut date =
        |key: &'static str| match values.get(key).map(|v| v.trim()).filter(|v| !v.is_empty()) {
            None => Ok(None),
            Some(text) => match CalendarDate::try_from(text.to_owned()) {
                Ok(date) => Ok(Some(date)),
                Err(_) => {
                    errors.push(FieldError::new(key, "is not a valid date"));
                    Err(())
                }
            },
        };
    let from = date("effective_from");
    let to = date("effective_to");
    let (Ok(from), Ok(to)) = (from, to) else {
        return None;
    };
    match EffectiveDates::new(from, to) {
        Ok(dates) => Some(dates),
        Err(_) => {
            errors.push(FieldError::new(
                "effective_to",
                "must be on or after effective from date",
            ));
            None
        }
    }
}
impl LiveState<Scope, Error> for LinksPage {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                self.event(request.scope, request.credential, value).await?;
            }
            self.draw(request.scope, false).await
        })
    }
}

/// The Relationships card of a record page (Bilimbi `ShowLive` section 6):
/// the party's current links, read-only, with Manage leading a company's
/// readers to its administration page.
struct Panel {
    shared: Arc<Shared>,
    id: String,
    party: PartyId,
    columns: ListColumns,
}
impl Panel {
    async fn draw(&self, scope: &Scope) -> Result<LiveBody> {
        let entries = match self.party {
            PartyId::Organisation(company) => {
                self.shared
                    .company
                    .company_relationships(scope, company, false)
                    .await?
            }
            PartyId::Person(person) => {
                self.shared
                    .company
                    .person_relationships(scope, person, false)
                    .await?
            }
        };
        let heading = format!("{}-heading", self.id);
        let card = format!("{}-card", self.id);
        let manage_id = format!("{}-manage", self.id);
        let actions = match self.party {
            PartyId::Organisation(company) => render(&ActionLink {
                id: &manage_id,
                icon: "manage",
                title: "Manage relationships",
                href: &format!("/companies/{}/relationships", company.get()),
                body: Html::text("Manage"),
                ..ActionLink::default()
            })?,
            PartyId::Person(_) => Html::EMPTY,
        };
        let mut body = render(&Wrap {
            class: "px-5 pb-4 pt-5 sm:px-6",
            body: render(&SectionHeading {
                id: &heading,
                title: "Relationships",
                count: Some(entries.len()),
                actions,
                ..SectionHeading::default()
            })?,
        })?;
        let rows = entries
            .iter()
            .map(|entry| link_row(entry, self.columns.mode(), true, false, false))
            .collect::<Result<Vec<_>>>()?;
        let mut table = link_table(
            &self.columns,
            rows,
            "Relationships",
            false,
            "No relationships defined.",
        );
        table.target = &self.id;
        body.push(&render(&table)?);
        let card = render(&Card {
            id: &card,
            class: "mt-6",
            inner_class: "p-0",
            role: "region",
            labelledby: &heading,
            body,
            ..Card::default()
        })?;
        // The fragment's root carries the panel's own id: the host patches the
        // panel by that id, so a root named otherwise would rename the anchor
        // and every later patch would miss it.
        let html = render(&PanelRoot {
            id: &self.id,
            body: card,
        })?;
        Ok(LiveBody {
            html: html.as_str().to_owned(),
            rows: 1,
            ..LiveBody::default()
        })
    }
}
impl LiveState<Scope, Error> for Panel {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            if let LiveInput::Event(value) = request.input {
                if get(value, "type") != "grid" {
                    return Err(Error::Invalid);
                }
                self.columns.handle(&values_of(value));
            }
            self.draw(request.scope).await
        })
    }
}
