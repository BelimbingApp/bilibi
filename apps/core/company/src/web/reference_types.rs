//! `/companies/legal-entity-types` and `/companies/department-types`
//! (Bilimbi `Core.Company.Web.ReferenceTypesLive`): one modal table for both
//! global catalogs. The route picks a [`Spec`]: nouns, DOM ids and whether the
//! category filter is on. Reads are the operator's; every write goes through
//! `CompanyService`, which asks `admin.company.create`, `.update` or `.delete`
//! again at the moment of the call, so a `can_*` here only decides which
//! controls render, and a refusal is a flash, never a page-clearing error.
use super::{allowed, render, Flash};
use crate::reference_types::{
    self, check_new, check_update, DepartmentType, Direction, LegalEntityType, NewType, Outcome,
    TypeSort, TypeUpdate, CATEGORIES,
};
use crate::{FieldError, WriteError};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice,
    ConfirmDialog, CreateButton, Field, FlashMap, FlexRow, FlexTable, Header, Html, IconButton,
    IconKind, InputKind, ListColumns, Modal, StatusKind, TableEmpty,
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

const DENIED: &str = "You do not have permission to change company administration data.";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Kind {
    LegalEntityType,
    DepartmentType,
}

/// What differs between the two catalogs' screens (Bilimbi `spec/1`).
pub(super) struct Spec {
    kind: Kind,
    form_as: &'static str,
    page_title: &'static str,
    subtitle: &'static str,
    add_label: &'static str,
    caption: &'static str,
    empty: &'static str,
    new_title: &'static str,
    edit_title: &'static str,
    code_placeholder: &'static str,
    name_placeholder: &'static str,
    description_placeholder: &'static str,
    not_found: &'static str,
    created: &'static str,
    updated: &'static str,
    deleted: &'static str,
    confirm_noun: &'static str,
    confirm_detail: &'static str,
    in_use: &'static str,
    dom_page: &'static str,
    dom_back: &'static str,
    dom_new: &'static str,
    dom_card: &'static str,
    dom_table: &'static str,
    dom_toggle: &'static str,
    dom_edit: &'static str,
    dom_delete: &'static str,
    dom_modal: &'static str,
    dom_form: &'static str,
    dom_field: &'static str,
    dom_confirm: &'static str,
}
impl Spec {
    fn category(&self) -> bool {
        self.kind == Kind::DepartmentType
    }
}
pub(super) static DEPARTMENT_TYPES: Spec = Spec {
    kind: Kind::DepartmentType,
    form_as: "department_type",
    page_title: "Department Types",
    subtitle: "Set the department types a company can choose from.",
    add_label: "Department Type",
    caption: "Department Types",
    empty: "No department types found.",
    new_title: "New Department Type",
    edit_title: "Edit Department Type",
    code_placeholder: "e.g. ENG, HR, FIN",
    name_placeholder: "e.g. Engineering",
    description_placeholder: "Optional description of this department type",
    not_found: "Department type not found.",
    created: "Department type created successfully.",
    updated: "Department type updated successfully.",
    deleted: "Department type deleted.",
    confirm_noun: "Department type",
    confirm_detail: "It can no longer be chosen for a department. This cannot be undone.",
    in_use: " was not deleted: one or more company departments still use it. Change those departments' type first.",
    dom_page: "department-types-index",
    dom_back: "department-types-back",
    dom_new: "new-department-type-btn",
    dom_card: "department-types-card",
    dom_table: "department-types",
    dom_toggle: "toggle-dept-type",
    dom_edit: "edit-dept-type",
    dom_delete: "delete-dept-type",
    dom_modal: "department-type-modal",
    dom_form: "department-type-form",
    dom_field: "department-type",
    dom_confirm: "delete-dept-type-confirm",
};
pub(super) static LEGAL_ENTITY_TYPES: Spec = Spec {
    kind: Kind::LegalEntityType,
    form_as: "legal_entity_type",
    page_title: "Legal Entity Types",
    subtitle: "The legal form a company is registered as. For example, a Private Limited Company (Sdn Bhd), a Partnership, or a Branch Office of a foreign company.",
    add_label: "Legal Entity Type",
    caption: "Legal Entity Types",
    empty: "No legal entity types defined yet.",
    new_title: "New Legal Entity Type",
    edit_title: "Edit Legal Entity Type",
    code_placeholder: "e.g. LLC, CORP, PT",
    name_placeholder: "e.g. Limited Liability Company",
    description_placeholder: "Optional description of this legal entity type",
    not_found: "Legal entity type not found.",
    created: "Legal entity type created successfully.",
    updated: "Legal entity type updated successfully.",
    deleted: "Legal entity type deleted.",
    confirm_noun: "Legal entity type",
    confirm_detail: "It can no longer be chosen for a company. This cannot be undone.",
    in_use: " was not deleted: one or more companies still use it. Change those companies' legal entity type first.",
    dom_page: "legal-entity-types-index",
    dom_back: "legal-entity-types-back",
    dom_new: "new-legal-entity-type-btn",
    dom_card: "legal-entity-types-card",
    dom_table: "legal-entity-types",
    dom_toggle: "toggle-type",
    dom_edit: "edit-type",
    dom_delete: "delete-type",
    dom_modal: "legal-entity-type-modal",
    dom_form: "legal-entity-type-form",
    dom_field: "legal-entity-type",
    dom_confirm: "delete-type-confirm",
};

/// Either catalog's row as the table draws it.
#[derive(Clone, Debug)]
struct TypeRow {
    id: i64,
    code: String,
    name: String,
    category: Option<String>,
    description: Option<String>,
    is_active: bool,
    version: i64,
}
impl From<LegalEntityType> for TypeRow {
    fn from(value: LegalEntityType) -> Self {
        Self {
            id: value.id,
            code: value.code,
            name: value.name,
            category: None,
            description: value.description,
            is_active: value.is_active,
            version: value.version,
        }
    }
}
impl From<DepartmentType> for TypeRow {
    fn from(value: DepartmentType) -> Self {
        Self {
            id: value.id,
            code: value.code,
            name: value.name,
            category: Some(value.category),
            description: value.description,
            is_active: value.is_active,
            version: value.version,
        }
    }
}

enum ModalAction {
    New,
    /// The row the dialog loaded: its code is shown read-only and its
    /// version fences the save.
    Edit {
        id: i64,
        code: String,
        version: i64,
    },
}
struct Draft {
    action: ModalAction,
    values: HashMap<String, String>,
    errors: Vec<FieldError>,
}

#[derive(Template)]
#[template(path = "reference_types.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    page_id: &'a str,
    header: Html,
    categories: Vec<Html>,
    card: Html,
    modal: Html,
    confirm: Html,
}
#[derive(Template)]
#[template(path = "reference_type_form.html")]
struct FormView<'a> {
    form_id: &'a str,
    code: Field,
    name: Field,
    category: Option<Field>,
    description: Field,
    active: Option<Field>,
}

pub(super) struct ReferenceTypes {
    db: Db,
    company: Arc<crate::CompanyService>,
    spec: &'static Spec,
    columns: ListColumns,
    /// `all` or one of `CATEGORIES`; department types only.
    category: String,
    rows: Vec<TypeRow>,
    flash: Flash,
    draft: Option<Draft>,
    pending_delete: Option<TypeRow>,
}

fn builtins(spec: &Spec) -> Vec<BuiltinColumn> {
    let mut columns = vec![
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("name", "Name"),
    ];
    if spec.category() {
        columns.push(BuiltinColumn::new("category", "Category"));
    }
    columns.push(BuiltinColumn::new("description", "Description"));
    columns.push(BuiltinColumn::new("is_active", "Status"));
    columns
}
pub(super) fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
fn category_options() -> Vec<Choice> {
    CATEGORIES
        .iter()
        .map(|category| Choice::new(capitalize(category), *category))
        .collect()
}

impl ReferenceTypes {
    pub fn new(db: Db, company: Arc<crate::CompanyService>, spec: &'static Spec) -> Result<Self> {
        Ok(Self {
            db,
            company,
            spec,
            columns: ListColumns::mount(spec.dom_table, builtins(spec))
                .map_err(|_| Error::Internal)?,
            category: "all".into(),
            rows: Vec::new(),
            flash: Flash::default(),
            draft: None,
            pending_delete: None,
        })
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        bilibi_authz::authorize_operator(&**client, scope, crate::LIST).await?;
        self.rows = match self.spec.kind {
            Kind::LegalEntityType => {
                reference_types::list_legal_entity_types(&**client, TypeSort::Name, Direction::Asc)
                    .await?
                    .into_iter()
                    .map(TypeRow::from)
                    .collect()
            }
            Kind::DepartmentType => reference_types::list_department_types(
                &**client,
                Some(self.category.as_str()).filter(|category| *category != "all"),
                false,
                TypeSort::Name,
                Direction::Asc,
            )
            .await?
            .into_iter()
            .map(TypeRow::from)
            .collect(),
        };
        self.columns.load(&self.rows, |row| row.id.to_string());
        Ok(())
    }

    async fn fetch(&self, id: i64) -> Result<Option<TypeRow>> {
        let client = self.db.get().await?;
        Ok(match self.spec.kind {
            Kind::LegalEntityType => reference_types::get_legal_entity_type(&**client, id)
                .await?
                .map(TypeRow::from),
            Kind::DepartmentType => reference_types::get_department_type(&**client, id)
                .await?
                .map(TypeRow::from),
        })
    }

    fn close_modal(&mut self) {
        self.draft = None;
    }
    fn write_forbidden(&mut self) {
        self.draft = None;
        self.pending_delete = None;
        self.flash.put(StatusKind::Error, DENIED);
    }

    fn open_new(&mut self) {
        self.flash.clear();
        let mut values = HashMap::from([("is_active".into(), "true".into())]);
        if self.spec.category() {
            values.insert("category".into(), "operational".into());
        }
        self.draft = Some(Draft {
            action: ModalAction::New,
            values,
            errors: Vec::new(),
        });
    }
    async fn open_edit(&mut self, id: Option<&str>) -> Result<()> {
        let Some(id) = params::positive_id(id) else {
            return Ok(());
        };
        let Some(row) = self.fetch(id).await? else {
            self.flash.put(StatusKind::Error, self.spec.not_found);
            return Ok(());
        };
        self.flash.clear();
        let mut values = HashMap::from([
            ("name".into(), row.name.clone()),
            (
                "description".into(),
                row.description.clone().unwrap_or_default(),
            ),
        ]);
        if let Some(category) = &row.category {
            values.insert("category".into(), category.clone());
        }
        self.draft = Some(Draft {
            action: ModalAction::Edit {
                id: row.id,
                code: row.code,
                version: row.version,
            },
            values,
            errors: Vec::new(),
        });
        Ok(())
    }

    fn new_type(values: &HashMap<String, String>, category: bool) -> NewType {
        let text = |key: &str| values.get(key).cloned();
        NewType {
            code: text("code").unwrap_or_default(),
            name: text("name").unwrap_or_default(),
            category: if category {
                Some(text("category").unwrap_or_default())
            } else {
                None
            },
            description: text("description"),
            is_active: Some(values.get("is_active").map(String::as_str) == Some("true")),
        }
    }
    fn type_update(values: &HashMap<String, String>, category: bool, version: i64) -> TypeUpdate {
        let text = |key: &str| values.get(key).cloned();
        TypeUpdate {
            name: text("name").unwrap_or_default(),
            category: if category { text("category") } else { None },
            description: text("description"),
            is_active: None,
            version,
        }
    }

    /// The form changed: keep what it posted and show what a save would
    /// refuse (Bilimbi `validate`).
    fn validate(&mut self, posted: HashMap<String, String>) {
        let category = self.spec.category();
        let Some(draft) = &mut self.draft else {
            return;
        };
        draft.values.extend(posted);
        draft.errors = match &draft.action {
            ModalAction::New => check_new(&Self::new_type(&draft.values, category), category),
            ModalAction::Edit { version, .. } => check_update(
                &Self::type_update(&draft.values, category, *version),
                category,
            ),
        };
    }

    /// Returns whether the catalog must be read again.
    async fn save(&mut self, scope: &Scope, posted: HashMap<String, String>) -> Result<bool> {
        let category = self.spec.category();
        let spec = self.spec;
        let Some(draft) = &mut self.draft else {
            return Ok(false);
        };
        draft.values.extend(posted);
        match &draft.action {
            ModalAction::New => {
                let new = Self::new_type(&draft.values, category);
                let outcome = match spec.kind {
                    Kind::LegalEntityType => self
                        .company
                        .create_legal_entity_type(scope, &new)
                        .await
                        .map(|outcome| match outcome {
                            Outcome::Saved(_) => Outcome::Saved(()),
                            Outcome::Refused(errors) => Outcome::Refused(errors),
                        }),
                    Kind::DepartmentType => self
                        .company
                        .create_department_type(scope, &new)
                        .await
                        .map(|outcome| match outcome {
                            Outcome::Saved(_) => Outcome::Saved(()),
                            Outcome::Refused(errors) => Outcome::Refused(errors),
                        }),
                };
                match outcome {
                    Ok(Outcome::Saved(())) => {
                        self.draft = None;
                        self.flash.put(StatusKind::Success, spec.created);
                        Ok(true)
                    }
                    Ok(Outcome::Refused(errors)) => {
                        draft.errors = errors;
                        Ok(false)
                    }
                    Err(Error::Unauthorized) => Err(Error::Unauthorized),
                    Err(Error::Forbidden) => {
                        self.write_forbidden();
                        Ok(true)
                    }
                    Err(error) => Err(error),
                }
            }
            ModalAction::Edit { id, version, .. } => {
                let update = Self::type_update(&draft.values, category, *version);
                let id = *id;
                let outcome = match spec.kind {
                    Kind::LegalEntityType => self
                        .company
                        .update_legal_entity_type(scope, id, &update)
                        .await
                        .map(|outcome| match outcome {
                            Outcome::Saved(_) => Outcome::Saved(()),
                            Outcome::Refused(errors) => Outcome::Refused(errors),
                        }),
                    Kind::DepartmentType => self
                        .company
                        .update_department_type(scope, id, &update)
                        .await
                        .map(|outcome| match outcome {
                            Outcome::Saved(_) => Outcome::Saved(()),
                            Outcome::Refused(errors) => Outcome::Refused(errors),
                        }),
                };
                match outcome {
                    Ok(Outcome::Saved(())) => {
                        self.draft = None;
                        self.flash.put(StatusKind::Success, spec.updated);
                        Ok(true)
                    }
                    Ok(Outcome::Refused(errors)) => {
                        draft.errors = errors;
                        Ok(false)
                    }
                    Err(WriteError::NotFound) => {
                        self.draft = None;
                        self.flash.put(StatusKind::Error, spec.not_found);
                        Ok(true)
                    }
                    Err(WriteError::Stale) => {
                        self.draft = None;
                        self.flash.put(
                            StatusKind::Error,
                            "The record changed while you were editing. Review the current values and try again.",
                        );
                        Ok(true)
                    }
                    Err(WriteError::Access(Error::Unauthorized)) => Err(Error::Unauthorized),
                    Err(WriteError::Access(Error::Forbidden)) => {
                        self.write_forbidden();
                        Ok(true)
                    }
                    Err(WriteError::Access(error)) => Err(error),
                    Err(_) => {
                        self.flash
                            .put(StatusKind::Error, "Could not save the type.");
                        Ok(false)
                    }
                }
            }
        }
    }

    async fn toggle(&mut self, scope: &Scope, id: Option<&str>) -> Result<()> {
        let Some(id) = params::positive_id(id) else {
            return Ok(());
        };
        let result = match self.spec.kind {
            Kind::LegalEntityType => self
                .company
                .toggle_legal_entity_type(scope, id)
                .await
                .map(|_| ()),
            Kind::DepartmentType => self
                .company
                .toggle_department_type(scope, id)
                .await
                .map(|_| ()),
        };
        match result {
            Ok(()) => self
                .flash
                .put(StatusKind::Success, "Status updated successfully."),
            Err(WriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => self.write_forbidden(),
            Err(WriteError::Access(error)) => return Err(error),
            Err(_) => self
                .flash
                .put(StatusKind::Error, "Could not update status."),
        }
        Ok(())
    }

    /// Deleting confirms through the shared dialog: the request holds the
    /// type whose consequence the dialog states, and `delete` acts on that
    /// held type rather than on a client-supplied id.
    async fn request_delete(&mut self, id: Option<&str>) -> Result<()> {
        let Some(id) = params::positive_id(id) else {
            return Ok(());
        };
        match self.fetch(id).await? {
            Some(row) => {
                self.flash.clear();
                self.pending_delete = Some(row);
            }
            None => self.flash.put(StatusKind::Error, self.spec.not_found),
        }
        Ok(())
    }
    async fn delete(&mut self, scope: &Scope) -> Result<()> {
        let Some(row) = self.pending_delete.take() else {
            return Ok(());
        };
        let result = match self.spec.kind {
            Kind::LegalEntityType => self.company.delete_legal_entity_type(scope, row.id).await,
            Kind::DepartmentType => self.company.delete_department_type(scope, row.id).await,
        };
        match result {
            Ok(()) => self.flash.put(StatusKind::Success, self.spec.deleted),
            Err(WriteError::InUse) => self.flash.put(
                StatusKind::Error,
                format!("{}{}", row.name, self.spec.in_use),
            ),
            Err(WriteError::NotFound) => self.flash.put(StatusKind::Error, self.spec.not_found),
            Err(WriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => self.write_forbidden(),
            Err(WriteError::Access(error)) => return Err(error),
            Err(_) => self
                .flash
                .put(StatusKind::Error, "The type was not deleted."),
        }
        Ok(())
    }

    fn form(&self, draft: &Draft) -> Result<Html> {
        let spec = self.spec;
        let field = |key: &str| {
            Field::in_form(spec.form_as, key)
                .id(format!("{}-{}", spec.dom_field, key.replace('_', "-")))
                .value(draft.values.get(key).cloned().unwrap_or_default())
                .errors(
                    draft
                        .errors
                        .iter()
                        .filter(|error| error.field == key)
                        .map(|error| error.message.clone())
                        .collect(),
                )
                .attr("data-debounce", "300")
        };
        let editing = matches!(draft.action, ModalAction::Edit { .. });
        let mut code = field("code")
            .label("Code")
            .placeholder(spec.code_placeholder)
            .required();
        if let ModalAction::Edit { code: loaded, .. } = &draft.action {
            code = code.value(loaded.clone()).disabled();
        }
        let name = field("name")
            .label("Name")
            .placeholder(spec.name_placeholder)
            .required();
        let category = spec.category().then(|| {
            field("category")
                .kind(InputKind::Select)
                .label("Category")
                .options(category_options())
                .required()
        });
        let description = field("description")
            .kind(InputKind::Textarea)
            .label("Description")
            .placeholder(spec.description_placeholder);
        let active = (!editing).then(|| {
            Field::in_form(spec.form_as, "is_active")
                .id(format!("{}-active", spec.dom_field))
                .kind(InputKind::Checkbox)
                .label("Active")
                .checked(draft.values.get("is_active").map(String::as_str) == Some("true"))
        });
        render(&FormView {
            form_id: spec.dom_form,
            code,
            name,
            category,
            description,
            active,
        })
    }

    pub async fn view(&mut self, scope: &Scope, requery: bool) -> Result<String> {
        if requery {
            self.load(scope).await?;
        }
        let spec = self.spec;
        let client = self.db.get().await?;
        let can_create = allowed(&**client, scope, crate::CREATE).await?;
        let can_update = allowed(&**client, scope, crate::UPDATE).await?;
        let can_delete = allowed(&**client, scope, crate::DELETE).await?;
        drop(client);

        let mut actions = render(&BackLink {
            id: spec.dom_back,
            href: "/companies",
            title: "Back to companies",
            ..BackLink::default()
        })?;
        if can_create {
            actions.push(&render(&CreateButton {
                noun: spec.add_label,
                id: spec.dom_new,
                class: "text-xs",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        let header = render(&Header {
            title: Html::text(spec.page_title),
            subtitle: spec.subtitle,
            actions: render(&super::views::Actions {
                class: "flex items-center gap-3",
                body: actions,
            })?,
            ..Header::default()
        })?;

        let mut categories = Vec::new();
        if spec.category() {
            for (key, label) in std::iter::once(("all", "All".to_owned())).chain(
                CATEGORIES
                    .iter()
                    .map(|category| (*category, capitalize(category))),
            ) {
                categories.push(render(&Button {
                    variant: if self.category == key {
                        ButtonVariant::Primary
                    } else {
                        ButtonVariant::Secondary
                    },
                    class: "text-xs py-1 px-2.5",
                    button_type: "button",
                    event: "filter_category",
                    name: "category",
                    value: key,
                    body: Html::text(&label),
                    ..Button::default()
                })?);
            }
        }

        let mut rows = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let id = row.id.to_string();
            let mut flex = FlexRow::new(&id)
                .slot(
                    "code",
                    Html::raw(format!(
                        "<code class=\"text-xs font-medium\">{}</code>",
                        Html::text(&row.code)
                    )),
                )
                .slot(
                    "name",
                    Html::raw(format!(
                        "<span class=\"font-medium text-ink-strong\">{}</span>",
                        Html::text(&row.name)
                    )),
                )
                .slot(
                    "description",
                    Html::raw(format!(
                        "<span class=\"text-xs text-ink-subtle\">{}</span>",
                        Html::text(row.description.as_deref().unwrap_or("—"))
                    )),
                )
                .slot(
                    "is_active",
                    render(&Badge::status(
                        if row.is_active {
                            BadgeKind::Success
                        } else {
                            BadgeKind::Neutral
                        },
                        if row.is_active { "active" } else { "inactive" },
                    ))?,
                );
            if let Some(category) = &row.category {
                flex = flex.slot(
                    "category",
                    render(&Badge {
                        kind: BadgeKind::Neutral,
                        dot: false,
                        class: "",
                        body: Html::text(&capitalize(category)),
                    })?,
                );
            }
            let mut actions = Html::EMPTY;
            if can_update {
                actions.push(&Html::raw(format!(
                    "<button type=\"button\" id=\"{}-{id}\" data-event=\"toggle_active\" data-id=\"{id}\" class=\"text-xs text-ink-subtle hover:text-ink hover:underline\">{}</button>",
                    spec.dom_toggle,
                    if row.is_active { "Deactivate" } else { "Activate" }
                )));
                let button_id = format!("{}-{id}", spec.dom_edit);
                let label = format!("Edit {}", row.name);
                actions.push(&render(&IconButton {
                    id: &button_id,
                    event: "edit",
                    name: "id",
                    value: &id,
                    ..IconButton::new("edit", &label)
                })?);
            }
            if can_delete {
                let button_id = format!("{}-{id}", spec.dom_delete);
                let label = format!("Delete {}", row.name);
                actions.push(&render(&IconButton {
                    id: &button_id,
                    kind: IconKind::Danger,
                    event: "request_delete",
                    name: "id",
                    value: &id,
                    ..IconButton::new("delete", &label)
                })?);
            }
            flex.actions = Html::raw(format!(
                "<div class=\"flex items-center gap-2\">{}</div>",
                actions.as_str()
            ));
            rows.push(flex);
        }
        let mut table = FlexTable::new(spec.dom_table, self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = spec.caption;
        table.framed = false;
        table.actions = true;
        if self.rows.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(spec.empty)));
        }
        let card = render(&Card {
            id: spec.dom_card,
            inner_class: "p-0",
            body: render(&table)?,
            ..Card::default()
        })?;

        let modal = match &self.draft {
            Some(draft) => {
                let editing = matches!(draft.action, ModalAction::Edit { .. });
                render(&Modal {
                    id: spec.dom_modal,
                    title: if editing {
                        spec.edit_title
                    } else {
                        spec.new_title
                    },
                    cancel_event: "close_modal",
                    flash: Some(self.flash.map()),
                    body: self.form(draft)?,
                    ..Modal::default()
                })?
            }
            None => Html::EMPTY,
        };
        let confirm = match &self.pending_delete {
            Some(row) => render(&ConfirmDialog {
                id: spec.dom_confirm,
                consequence: &format!("{} “{}” will be deleted.", spec.confirm_noun, row.name),
                detail: spec.confirm_detail,
                confirm: "Delete",
                working: "Deleting…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?,
            None => Html::EMPTY,
        };
        View {
            flash: self.flash.map(),
            page_id: spec.dom_page,
            header,
            categories,
            card,
            modal,
            confirm,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for ReferenceTypes {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut requery = true;
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    match get("type") {
                        Some("grid") => {
                            self.columns.handle(&params::string_values(event));
                            requery = false;
                        }
                        Some("filter_category") if self.spec.category() => {
                            let category = get("category").unwrap_or("all");
                            if category == "all" || CATEGORIES.contains(&category) {
                                self.category = category.into();
                            }
                        }
                        Some("new") => {
                            self.open_new();
                            requery = false;
                        }
                        Some("edit") => {
                            self.open_edit(get("id")).await?;
                            requery = false;
                        }
                        Some("close_modal") => {
                            self.close_modal();
                            requery = false;
                        }
                        Some("validate") => {
                            self.validate(params::form_values(event, self.spec.form_as));
                            requery = false;
                        }
                        Some("save") => {
                            requery = self
                                .save(request.scope, params::form_values(event, self.spec.form_as))
                                .await?;
                        }
                        Some("toggle_active") => self.toggle(request.scope, get("id")).await?,
                        Some("request_delete") => {
                            self.request_delete(get("id")).await?;
                            requery = false;
                        }
                        Some("cancel_delete") => {
                            self.pending_delete = None;
                            requery = false;
                        }
                        Some("delete") => self.delete(request.scope).await?,
                        Some("clear-flash") => {
                            self.flash.dismiss(get("key"));
                            requery = false;
                        }
                        _ => return Err(Error::Invalid),
                    }
                }
            }
            let html = self.view(request.scope, requery).await?;
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
}
