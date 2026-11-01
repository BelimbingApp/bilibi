//! `/companies/{id}/departments`:
//! a company's departments with add, status, head and remove. The route's
//! gate is `admin.company.view`; the controls draw for an actor who may
//! update the company now, and every write asks `CompanyService` again, which
//! authorizes the company as the target and refuses an archived company in
//! these words. A department's head comes from the bound `HeadDirectory`.
use super::{allowed, render, Flash};
use crate::departments::{self, Department, HeadCandidate, Outcome, STATUSES};
use crate::reference_types::DepartmentType;
use crate::{FieldError, WriteError};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState},
    params, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice,
    ConfirmDialog, Field, FlashMap, FlexRow, FlexTable, Html, IconButton, IconKind, InputKind,
    ListColumns, Modal, SectionHeading, StatusKind, TableEmpty,
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

const DENIED: &str = "You do not have permission to change company administration data.";
const ARCHIVED: &str = "This company is archived and read-only, so nothing was changed.";
const NOT_FOUND: &str = "Department not found.";
const FORM: &str = "department";
const HEAD_FORM: &str = "department_head";

#[derive(Template)]
#[template(path = "departments.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    company_id: i64,
    title: String,
    code: Html,
    back: Html,
    archived: Html,
    card: Html,
    modal: Html,
    confirm: Html,
}
#[derive(Template)]
#[template(path = "departments_card.html")]
struct CardBody {
    heading: Html,
    table: Html,
}
#[derive(Template)]
#[template(path = "department_form.html")]
struct FormView {
    department_type: Field,
    status: Field,
    no_types: bool,
}
#[derive(Template)]
#[template(path = "department_head_form.html")]
struct HeadFormView {
    head: Field,
}

enum Dialog {
    New {
        values: HashMap<String, String>,
        errors: Vec<FieldError>,
        types: Vec<DepartmentType>,
    },
    Head {
        department: Department,
        options: Vec<HeadCandidate>,
    },
}

/// What the page read with the company: the facts the controls depend on.
struct Shown {
    name: String,
    code: String,
    archived: bool,
    can_update: bool,
    departments: Vec<Department>,
    heads: HashMap<i64, String>,
}

pub(super) struct Departments {
    db: Db,
    company: Arc<crate::CompanyService>,
    company_id: i64,
    columns: ListColumns,
    flash: Flash,
    shown: Option<Shown>,
    dialog: Option<Dialog>,
    pending_delete: Option<Department>,
}

pub(super) fn builtins() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("code", "Code"),
        BuiltinColumn::new("name", "Department Name"),
        BuiltinColumn::new("category", "Category"),
        BuiltinColumn::new("head", "Head"),
        BuiltinColumn::new("status", "Status"),
    ]
}
fn status_options() -> Vec<Choice> {
    STATUSES
        .iter()
        .map(|status| Choice::new(super::reference_types::capitalize(status), *status))
        .collect()
}

impl Departments {
    pub fn new(db: Db, company: Arc<crate::CompanyService>, company_id: i64) -> Result<Self> {
        Ok(Self {
            db,
            company,
            company_id,
            columns: ListColumns::mount("company-departments", builtins())
                .map_err(|_| Error::Internal)?,
            flash: Flash::default(),
            shown: None,
            dialog: None,
            pending_delete: None,
        })
    }

    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        let (company, _) = crate::detail(&**client, scope, self.company_id).await?;
        let can_target = allowed(&**client, scope, crate::TENANT_WIDE).await?
            || crate::organisation::reaches_without_tenant_wide(scope, self.company_id);
        let can_update =
            allowed(&**client, scope, crate::UPDATE).await? && can_target && !company.archived();
        let departments = departments::list(
            &**client,
            scope,
            self.company_id,
            departments::DepartmentSort::Name,
            crate::reference_types::Direction::Asc,
        )
        .await?;
        let heads = departments::head_names(&**client, scope, &departments).await?;
        self.columns
            .load(&departments, |department| department.id.to_string());
        self.shown = Some(Shown {
            name: company.display_name(),
            archived: company.archived(),
            code: company.code,
            can_update,
            departments,
            heads,
        });
        Ok(())
    }

    fn department(&self, id: Option<&str>) -> Option<Department> {
        let id = params::positive_id(id)?;
        self.shown
            .as_ref()?
            .departments
            .iter()
            .find(|department| department.id == id)
            .cloned()
    }

    /// One gate ahead of every persisting event: the mount-time facts hide the controls, and the
    /// write itself asks again. Returns whether the event may proceed.
    async fn may_write(&mut self, scope: &Scope) -> Result<bool> {
        let client = self.db.get().await?;
        if !allowed(&**client, scope, crate::UPDATE).await? {
            self.flash.put(StatusKind::Error, DENIED);
            self.dialog = None;
            self.pending_delete = None;
            return Ok(false);
        }
        match crate::organisation::require_writable(&**client, scope, self.company_id, false).await
        {
            Ok(_) => Ok(true),
            Err(WriteError::Archived) => {
                self.flash.put(StatusKind::Error, ARCHIVED);
                self.dialog = None;
                self.pending_delete = None;
                Ok(false)
            }
            Err(WriteError::NotFound) => Err(Error::NotFound),
            Err(WriteError::Access(error)) => Err(error),
            Err(_) => Err(Error::Internal),
        }
    }

    async fn open_new(&mut self, scope: &Scope) -> Result<()> {
        let client = self.db.get().await?;
        let types = departments::available_types(&**client, scope, self.company_id).await?;
        self.flash.clear();
        self.dialog = Some(Dialog::New {
            values: HashMap::from([("status".into(), "active".into())]),
            errors: Vec::new(),
            types,
        });
        Ok(())
    }

    fn check(values: &HashMap<String, String>, types: &[DepartmentType]) -> Vec<FieldError> {
        let mut errors = Vec::new();
        match params::positive_id(values.get("department_type_id").map(String::as_str)) {
            None => errors.push(FieldError::new("department_type_id", "can't be blank")),
            Some(id) if !types.iter().any(|department_type| department_type.id == id) => {
                errors.push(FieldError::new("department_type_id", "is invalid"))
            }
            Some(_) => {}
        }
        let status = values.get("status").map(String::as_str).unwrap_or("");
        if status.is_empty() {
            errors.push(FieldError::new("status", "can't be blank"));
        } else if !STATUSES.contains(&status) {
            errors.push(FieldError::new("status", "is invalid"));
        }
        errors
    }
    fn validate(&mut self, posted: HashMap<String, String>) {
        if let Some(Dialog::New {
            values,
            errors,
            types,
        }) = &mut self.dialog
        {
            values.extend(posted);
            *errors = Self::check(values, types);
        }
    }

    async fn save(&mut self, scope: &Scope, posted: HashMap<String, String>) -> Result<()> {
        let Some(Dialog::New {
            values,
            errors,
            types,
        }) = &mut self.dialog
        else {
            return Ok(());
        };
        values.extend(posted);
        *errors = Self::check(values, types);
        if !errors.is_empty() {
            return Ok(());
        }
        let department_type_id =
            params::positive_id(values.get("department_type_id").map(String::as_str))
                .ok_or(Error::Invalid)?;
        let status = values.get("status").cloned().unwrap_or_default();
        match self
            .company
            .create_department(scope, self.company_id, department_type_id, &status)
            .await
        {
            Ok(Outcome::Saved(_)) => {
                self.dialog = None;
                self.flash.put(StatusKind::Success, "Department added.");
            }
            Ok(Outcome::Refused(refused)) => *errors = refused,
            Err(WriteError::Archived) => {
                self.dialog = None;
                self.flash.put(StatusKind::Error, ARCHIVED);
            }
            Err(WriteError::NotFound) => return Err(Error::NotFound),
            Err(WriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => {
                self.dialog = None;
                self.flash.put(StatusKind::Error, DENIED);
            }
            Err(WriteError::Access(error)) => return Err(error),
            Err(_) => self
                .flash
                .put(StatusKind::Error, "The department was not added."),
        }
        Ok(())
    }

    async fn update_status(&mut self, scope: &Scope, id: Option<&str>, status: &str) -> Result<()> {
        let Some(department) = self.department(id) else {
            return Ok(());
        };
        match self
            .company
            .update_department_status(
                scope,
                self.company_id,
                department.id,
                status,
                department.version,
            )
            .await
        {
            Ok(Outcome::Saved(_)) => self.flash.put(
                StatusKind::Success,
                format!("Department status updated to {status}."),
            ),
            Err(WriteError::Archived) => self.flash.put(StatusKind::Error, ARCHIVED),
            Err(WriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => self.flash.put(StatusKind::Error, DENIED),
            Err(WriteError::Access(error)) => return Err(error),
            Ok(Outcome::Refused(_)) | Err(_) => self
                .flash
                .put(StatusKind::Error, "Could not update status."),
        }
        Ok(())
    }

    /// Removing confirms through the shared dialog: the request holds the
    /// department whose consequence the dialog states, and `delete` acts on
    /// that held department rather than on a client-supplied id.
    fn request_delete(&mut self, id: Option<&str>) {
        match self.department(id) {
            Some(department) => {
                self.flash.clear();
                self.pending_delete = Some(department);
            }
            None => self.flash.put(StatusKind::Error, NOT_FOUND),
        }
    }
    async fn delete(&mut self, scope: &Scope) -> Result<()> {
        let Some(department) = self.pending_delete.take() else {
            return Ok(());
        };
        match self
            .company
            .delete_department(scope, self.company_id, department.id)
            .await
        {
            Ok(()) => self.flash.put(StatusKind::Success, "Department removed."),
            Err(WriteError::NotFound) => self.flash.put(
                StatusKind::Error,
                "That department had already been removed.",
            ),
            Err(WriteError::Archived) => self.flash.put(StatusKind::Error, ARCHIVED),
            Err(WriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => self.flash.put(StatusKind::Error, DENIED),
            Err(WriteError::Access(error)) => return Err(error),
            Err(_) => self.flash.put(
                StatusKind::Error,
                "The department was not removed. Reload the page and try again.",
            ),
        }
        Ok(())
    }

    async fn edit_head(&mut self, scope: &Scope, id: Option<&str>) -> Result<()> {
        let Some(department) = self.department(id) else {
            self.flash.put(StatusKind::Error, NOT_FOUND);
            return Ok(());
        };
        let client = self.db.get().await?;
        match departments::head_candidates(&**client, scope, self.company_id).await {
            Ok(options) => {
                self.flash.clear();
                self.dialog = Some(Dialog::Head {
                    department,
                    options,
                });
            }
            Err(Error::Unauthorized) => return Err(Error::Unauthorized),
            Err(Error::NotFound) => return Err(Error::NotFound),
            Err(_) => self
                .flash
                .put(StatusKind::Error, "Could not load eligible people."),
        }
        Ok(())
    }
    async fn save_head(&mut self, scope: &Scope, posted: HashMap<String, String>) -> Result<()> {
        let Some(Dialog::Head { department, .. }) = &self.dialog else {
            self.flash.put(
                StatusKind::Error,
                "Choose a department before setting its head.",
            );
            return Ok(());
        };
        let head = match posted.get("head_id").map(String::as_str) {
            None | Some("") => None,
            Some(value) => match params::positive_id(Some(value)) {
                Some(id) => Some(id),
                None => {
                    self.flash.put(
                        StatusKind::Error,
                        "Choose an eligible person or clear the head.",
                    );
                    return Ok(());
                }
            },
        };
        match self
            .company
            .update_department_head(
                scope,
                self.company_id,
                department.id,
                head,
                department.version,
            )
            .await
        {
            Ok(Outcome::Saved(_)) => {
                self.dialog = None;
                self.flash.put(
                    StatusKind::Success,
                    if head.is_none() {
                        "Department head cleared."
                    } else {
                        "Department head updated."
                    },
                );
            }
            Ok(Outcome::Refused(_)) => self.flash.put(
                StatusKind::Error,
                "That person is not eligible to lead this department.",
            ),
            Err(WriteError::NotFound) => {
                self.dialog = None;
                self.flash.put(StatusKind::Error, NOT_FOUND);
            }
            Err(WriteError::Archived) => {
                self.dialog = None;
                self.flash.put(StatusKind::Error, ARCHIVED);
            }
            Err(WriteError::Access(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(WriteError::Access(Error::Forbidden)) => {
                self.dialog = None;
                self.flash.put(StatusKind::Error, DENIED);
            }
            Err(WriteError::Access(error)) => return Err(error),
            Err(_) => self
                .flash
                .put(StatusKind::Error, "Could not update department head."),
        }
        Ok(())
    }

    fn form(
        &self,
        values: &HashMap<String, String>,
        errors: &[FieldError],
        types: &[DepartmentType],
    ) -> Result<Html> {
        let field = |key: &str| {
            Field::in_form(FORM, key)
                .value(values.get(key).cloned().unwrap_or_default())
                .errors(
                    errors
                        .iter()
                        .filter(|error| error.field == key)
                        .map(|error| error.message.clone())
                        .collect(),
                )
        };
        render(&FormView {
            department_type: field("department_type_id")
                .id("department-type-id")
                .kind(InputKind::Select)
                .label("Department Type")
                .prompt("Select a department type")
                .options(
                    types
                        .iter()
                        .map(|department_type| {
                            Choice::new(
                                format!("{} ({})", department_type.name, department_type.code),
                                department_type.id.to_string(),
                            )
                        })
                        .collect(),
                )
                .required(),
            status: field("status")
                .id("department-status")
                .kind(InputKind::Select)
                .label("Initial Status")
                .options(status_options())
                .required(),
            no_types: types.is_empty(),
        })
    }
    fn head_form(&self, department: &Department, options: &[HeadCandidate]) -> Result<Html> {
        let mut head = Field::in_form(HEAD_FORM, "head_id")
            .id("department-head-id")
            .kind(InputKind::Select)
            .label("Department Head")
            .prompt("No department head")
            .value(
                department
                    .head_affiliation_id
                    .map(|id| id.to_string())
                    .unwrap_or_default(),
            )
            .options(
                options
                    .iter()
                    .map(|candidate| {
                        Choice::new(
                            candidate.label.clone(),
                            candidate.affiliation_id.to_string(),
                        )
                    })
                    .collect(),
            );
        if options.is_empty() {
            head = head.hint("Add people to this company before choosing a head.");
        }
        render(&HeadFormView { head })
    }

    pub async fn view(&mut self, scope: &Scope, requery: bool) -> Result<String> {
        if requery || self.shown.is_none() {
            self.load(scope).await?;
        }
        let shown = self.shown.as_ref().ok_or(Error::Internal)?;
        let can_update = shown.can_update;
        let title = format!("{} — Departments", shown.name);
        let back_title = format!("Back to {}", shown.name);
        let href = format!("/companies/{}", self.company_id);
        let back = render(&BackLink {
            id: "departments-back",
            href: &href,
            title: &back_title,
            ..BackLink::default()
        })?;
        let archived = if shown.archived {
            render(&Alert {
                kind: StatusKind::Warning,
                id: "company-archived",
                class: "mt-4",
                body: Html::text("This company is archived and read-only."),
            })?
        } else {
            Html::EMPTY
        };

        let heading = render(&SectionHeading {
            id: "company-departments-heading",
            title: "Departments",
            count: Some(shown.departments.len()),
            actions: if can_update {
                render(&Button {
                    id: "add-dept-btn",
                    variant: ButtonVariant::Primary,
                    button_type: "button",
                    event: "new",
                    body: Html::text("Add Department"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            },
            ..SectionHeading::default()
        })?;

        let mut rows = Vec::with_capacity(shown.departments.len());
        for department in &shown.departments {
            let id = department.id.to_string();
            let mut row = FlexRow::new(&id);
            row.dom_id = Some(format!("company-department-{id}"));
            let head = department
                .head_affiliation_id
                .and_then(|head| shown.heads.get(&head))
                .map(String::as_str)
                .unwrap_or("—");
            row = row
                .slot(
                    "code",
                    Html::raw(format!(
                        "<code class=\"text-xs font-medium\">{}</code>",
                        Html::text(&department.type_code)
                    )),
                )
                .slot(
                    "name",
                    Html::raw(format!(
                        "<span class=\"font-medium text-ink-strong\">{}</span>",
                        Html::text(&department.type_name)
                    )),
                )
                .slot(
                    "category",
                    render(&Badge {
                        kind: BadgeKind::Neutral,
                        dot: false,
                        class: "",
                        body: Html::text(&super::reference_types::capitalize(
                            &department.type_category,
                        )),
                    })?,
                )
                .slot("head", Html::text(head))
                .slot(
                    "status",
                    render(&Badge::status(
                        match department.status.as_str() {
                            "active" => BadgeKind::Success,
                            "suspended" => BadgeKind::Warning,
                            _ => BadgeKind::Neutral,
                        },
                        &department.status,
                    ))?,
                );
            let mut actions = Html::EMPTY;
            if can_update {
                let name = &department.type_name;
                // Each status change is its own event (one `update_status` with
                // `phx-value-status`); the row id travels
                // as the button's value.
                let buttons: [(bool, &str, &str, String, &str, IconKind); 5] = [
                    (
                        true,
                        "edit-dept-head",
                        "user-plus",
                        format!("Set head for {name}"),
                        "edit_head",
                        IconKind::Neutral,
                    ),
                    (
                        department.status != "active",
                        "activate-dept",
                        "success",
                        format!("Activate {name}"),
                        "activate",
                        IconKind::Neutral,
                    ),
                    (
                        department.status != "suspended",
                        "suspend-dept",
                        "pause",
                        format!("Suspend {name}"),
                        "suspend",
                        IconKind::Neutral,
                    ),
                    (
                        department.status != "inactive",
                        "deactivate-dept",
                        "x-circle",
                        format!("Deactivate {name}"),
                        "deactivate",
                        IconKind::Neutral,
                    ),
                    (
                        true,
                        "delete-dept",
                        "delete",
                        format!("Remove {name}"),
                        "request_delete",
                        IconKind::Danger,
                    ),
                ];
                for (shown, prefix, icon, label, event, kind) in &buttons {
                    if !shown {
                        continue;
                    }
                    let button_id = format!("{prefix}-{id}");
                    actions.push(&render(&IconButton {
                        id: &button_id,
                        event,
                        name: "id",
                        value: &id,
                        kind: *kind,
                        ..IconButton::new(icon, label)
                    })?);
                }
            }
            row.actions = Html::raw(format!(
                "<div class=\"flex items-center gap-2\">{}</div>",
                actions.as_str()
            ));
            rows.push(row);
        }
        let mut table = FlexTable::new("company-departments", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Company Departments";
        table.framed = false;
        table.actions = true;
        if shown.departments.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No departments configured for this company yet.",
            )));
        }
        let card = render(&Card {
            id: "company-departments-card",
            class: "mt-6",
            inner_class: "p-0",
            role: "region",
            labelledby: "company-departments-heading",
            body: render(&CardBody {
                heading,
                table: render(&table)?,
            })?,
            ..Card::default()
        })?;

        let modal = match &self.dialog {
            Some(Dialog::New {
                values,
                errors,
                types,
            }) => {
                let description = format!("Choose a department type to add to {}.", shown.name);
                render(&Modal {
                    id: "department-modal",
                    title: "Add Department",
                    cancel_event: "close_modal",
                    description: &description,
                    flash: Some(self.flash.map()),
                    body: self.form(values, errors, types)?,
                    ..Modal::default()
                })?
            }
            Some(Dialog::Head {
                department,
                options,
            }) => render(&Modal {
                id: "department-head-modal",
                title: "Set Department Head",
                cancel_event: "close_modal",
                description:
                    "Choose the person responsible for this department, or clear the current head.",
                flash: Some(self.flash.map()),
                body: self.head_form(department, options)?,
                ..Modal::default()
            })?,
            None => Html::EMPTY,
        };
        let confirm = match &self.pending_delete {
            Some(department) => render(&ConfirmDialog {
                id: "delete-dept-confirm",
                consequence: &format!(
                    "The {} department will be removed from this company.",
                    department.type_name
                ),
                detail: "People assigned to it keep their records but lose this department. This cannot be undone.",
                confirm: "Remove",
                working: "Removing…",
                confirm_event: "delete",
                cancel_event: "cancel_delete",
            })?,
            None => Html::EMPTY,
        };
        View {
            flash: self.flash.map(),
            company_id: self.company_id,
            title,
            code: Html::raw(format!(
                "<code class=\"text-xs font-medium\">{}</code>",
                Html::text(&shown.code)
            )),
            back,
            archived,
            card,
            modal,
            confirm,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

impl LiveState<Scope, Error> for Departments {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let mut requery = true;
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => {}
                LiveInput::Event(event) => {
                    let get = |key: &str| event.get(key).and_then(Value::as_str);
                    let kind = get("type").unwrap_or_default();
                    let write = matches!(
                        kind,
                        "new"
                            | "save"
                            | "activate"
                            | "suspend"
                            | "deactivate"
                            | "request_delete"
                            | "delete"
                            | "edit_head"
                            | "save_head"
                    );
                    if write && !self.may_write(request.scope).await? {
                        let html = self.view(request.scope, true).await?;
                        return Ok(LiveBody {
                            html,
                            ..LiveBody::default()
                        });
                    }
                    match kind {
                        "grid" => {
                            self.columns.handle(
                                &params::string_values_checked(event, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            );
                            requery = false;
                        }
                        "new" => {
                            self.open_new(request.scope).await?;
                            requery = false;
                        }
                        "validate" => {
                            self.validate(
                                params::form_values_checked(event, FORM, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            );
                            requery = false;
                        }
                        "save" => {
                            self.save(
                                request.scope,
                                params::form_values_checked(event, FORM, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            )
                            .await?
                        }
                        "activate" => {
                            self.update_status(request.scope, get("id"), "active")
                                .await?
                        }
                        "suspend" => {
                            self.update_status(request.scope, get("id"), "suspended")
                                .await?
                        }
                        "deactivate" => {
                            self.update_status(request.scope, get("id"), "inactive")
                                .await?
                        }
                        "request_delete" => {
                            self.request_delete(get("id"));
                            requery = false;
                        }
                        "cancel_delete" => {
                            self.pending_delete = None;
                            requery = false;
                        }
                        "delete" => self.delete(request.scope).await?,
                        "edit_head" => {
                            self.edit_head(request.scope, get("id")).await?;
                            requery = false;
                        }
                        "save_head" => {
                            self.save_head(
                                request.scope,
                                params::form_values_checked(event, HEAD_FORM, params::FIELD_LIMIT)
                                    .map_err(Error::refused)?,
                            )
                            .await?
                        }
                        "close_modal" => {
                            self.dialog = None;
                            requery = false;
                        }
                        "clear-flash" => {
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
