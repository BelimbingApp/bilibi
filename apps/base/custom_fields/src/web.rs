//! Tenant definition administration, mirroring Bilimbi `CustomFields`.
use crate::{CustomFields, Definition, FieldInput, FieldType, Outcome};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, BuiltinColumn, Button, ButtonVariant, Card, Choice, ColumnsOutcome, ConfirmDialog,
    Field, FilterToolbar, FlashGroup, FlexRow, FlexTable, Form, Header, Html, InputKind,
    ListColumns, ListSpec, ListState, Modal, ModalWidth, Page as UiPage, Pagination, StatusKind,
    Tab, TableEmpty, Tabs, ToolbarControl, NO_FLASH,
};
use serde_json::{json, Value};
use std::{collections::HashMap, sync::Arc};

pub const PATH: &str = "/custom-fields";
pub const PAGES: &str = "base/custom_fields.pages";
const PAGE_SIZES: [u64; 2] = [25, 50];
const MAX_OPTIONS_FORM_BYTES: usize = 48 * 1024;
const TYPES: &[(&str, &str)] = &[
    ("Text", "text"),
    ("Long text", "long_text"),
    ("Integer", "integer"),
    ("Decimal", "decimal"),
    ("Date", "date"),
    ("Date and time", "date_time"),
    ("Yes or no", "boolean"),
    ("Choice", "choice"),
    ("Multiple choice", "multi_choice"),
    ("Reference", "reference"),
];

static SPEC: std::sync::LazyLock<Arc<ListSpec>> = std::sync::LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("custom field list spec")
});

#[derive(Clone)]
pub(crate) struct Pages {
    pub fields: Arc<CustomFields>,
}

fn render<T: Template>(template: &T) -> Result<Html> {
    Html::render(template).map_err(|_| Error::Internal)
}
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}
fn type_label(field_type: FieldType) -> &'static str {
    TYPES
        .iter()
        .find(|(_, value)| *value == field_type.as_str())
        .map_or("Unknown", |(label, _)| *label)
}

#[derive(Clone, Debug, Default)]
struct Draft {
    key: String,
    label: String,
    field_type: String,
    required: bool,
    help_text: String,
    position: String,
    text_limit: String,
    format: String,
    options: String,
    reference_table: String,
}
impl Draft {
    fn from_definition(definition: &Definition) -> Self {
        Self {
            key: definition.key.clone(),
            label: definition.label.clone(),
            field_type: definition.field_type.as_str().into(),
            required: definition.required,
            help_text: definition.help_text.clone().unwrap_or_default(),
            position: definition.position.to_string(),
            text_limit: definition
                .text_limit
                .map(|value| value.to_string())
                .unwrap_or_default(),
            format: definition.format.clone().unwrap_or_default(),
            options: serde_json::to_string_pretty(&definition.options)
                .unwrap_or_else(|_| "[]".into()),
            reference_table: definition.reference_table.clone().unwrap_or_default(),
        }
    }
}

#[derive(Template)]
#[template(
    source = r#"<span class="block font-medium text-ink">{{ label }}</span><span class="font-mono text-xs text-ink-muted">cf_{{ key }}</span>"#,
    ext = "html"
)]
struct DefinitionCell<'a> {
    label: &'a str,
    key: &'a str,
}

struct Index {
    pages: Pages,
    state: ListState,
    record_type: String,
    archived: bool,
    columns: ListColumns,
    definitions: Vec<Definition>,
    rows_changed: bool,
    editing: Option<(String, i64)>,
    draft: Draft,
    confirm_archive: Option<(String, i64)>,
    flash: Option<(StatusKind, String)>,
    form_error: Option<String>,
    dialog_open: bool,
}

impl Index {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        let record_type = query
            .get("record_type")
            .cloned()
            .filter(|key| pages.fields.record_types().iter().any(|ty| ty.key == key))
            .or_else(|| {
                pages
                    .fields
                    .record_types()
                    .first()
                    .map(|ty| ty.key.to_owned())
            })
            .ok_or(Error::Invalid)?;
        let archived = query.get("archived").is_some_and(|value| value == "true");
        let columns = ListColumns::mount(
            "custom-fields-table",
            vec![
                BuiltinColumn::new("field", "Field"),
                BuiltinColumn::new("type", "Type"),
                BuiltinColumn::new("required", "Required"),
                BuiltinColumn::new("position", "Position"),
            ],
        )
        .map_err(|_| Error::Internal)?;
        Ok(Self {
            pages,
            state: parse(query)?,
            record_type,
            archived,
            columns,
            definitions: Vec::new(),
            rows_changed: true,
            editing: None,
            draft: Draft::default(),
            confirm_archive: None,
            flash: None,
            form_error: None,
            dialog_open: false,
        })
    }
    fn set_flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.definitions = self
            .pages
            .fields
            .list(scope, &self.record_type, self.archived)
            .await?;
        if self.archived {
            self.definitions.retain(|definition| definition.archived);
        }
        let needle = self.state.search.to_lowercase();
        if !needle.is_empty() {
            self.definitions.retain(|definition| {
                definition.key.to_lowercase().contains(&needle)
                    || definition.label.to_lowercase().contains(&needle)
                    || definition.field_type.as_str().contains(&needle)
            });
        }
        if self.rows_changed {
            self.columns
                .load(&self.definitions, |definition| definition.key.clone());
            self.rows_changed = false;
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let text = |key: &str| value.get(key).and_then(Value::as_str).unwrap_or_default();
        match text("type") {
            "filters" => {
                self.state
                    .apply_filters(&params::form_values(value, "filters"));
                self.rows_changed = true;
            }
            "page" => self
                .state
                .put_page(value.get("page").and_then(Value::as_str)),
            "view" => {
                self.archived = text("tab") == "archived";
                self.state.page = 1;
                self.rows_changed = true;
                self.editing = None;
                self.confirm_archive = None;
            }
            "record_type" => {
                let selected = text("tab");
                if self
                    .pages
                    .fields
                    .record_types()
                    .iter()
                    .any(|ty| ty.key == selected)
                {
                    self.record_type = selected.to_owned();
                    self.state.page = 1;
                    self.rows_changed = true;
                    self.editing = None;
                    self.confirm_archive = None;
                }
            }
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => {}
            },
            "add" => {
                self.editing = None;
                self.form_error = None;
                self.dialog_open = true;
                self.draft = Draft {
                    field_type: "text".into(),
                    position: self.definitions.len().to_string(),
                    options: "[]".into(),
                    ..Draft::default()
                };
            }
            "edit" => {
                let key = text("key");
                if let Some(definition) = self
                    .definitions
                    .iter()
                    .find(|definition| definition.key == key && !definition.archived)
                {
                    self.editing = Some((definition.key.clone(), definition.version));
                    self.draft = Draft::from_definition(definition);
                    self.form_error = None;
                    self.dialog_open = true;
                }
            }
            "close" => {
                self.editing = None;
                self.form_error = None;
                self.dialog_open = false;
            }
            "request_archive" => {
                let key = text("key");
                if let Some(definition) = self
                    .definitions
                    .iter()
                    .find(|definition| definition.key == key && !definition.archived)
                {
                    self.confirm_archive = Some((definition.key.clone(), definition.version));
                }
            }
            "cancel_archive" => self.confirm_archive = None,
            "save" => {
                let mut values = params::form_values(value, "definition");
                if let Some(options) = value.get("definition[options]").and_then(Value::as_str) {
                    if options.len() > MAX_OPTIONS_FORM_BYTES {
                        self.form_error =
                            Some("Choice options must be no larger than 48 KB.".into());
                        return Ok(());
                    }
                    values.insert("options".into(), options.to_owned());
                }
                self.save(scope, &values).await?
            }
            "archive" => self.archive(scope).await?,
            "restore" => self.restore(scope, text("key")).await?,
            "clear-flash" => {
                if self.flash.as_ref().is_some_and(|(kind, _)| {
                    Some(kind.name()) == value.get("key").and_then(Value::as_str)
                }) {
                    self.flash = None;
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn input(&self, values: &HashMap<String, String>) -> std::result::Result<FieldInput, String> {
        let field_type = if self.editing.is_some() {
            FieldType::parse(&self.draft.field_type)
        } else {
            values.get("type").and_then(|value| FieldType::parse(value))
        }
        .ok_or_else(|| "Choose a supported field type.".to_owned())?;
        let key = self
            .editing
            .as_ref()
            .map(|(key, _)| key.clone())
            .unwrap_or_else(|| values.get("key").cloned().unwrap_or_default());
        let parse_num = |name: &str, fallback: &str| -> std::result::Result<i32, String> {
            let raw = values.get(name).map(String::as_str).unwrap_or(fallback);
            if raw.trim().is_empty() {
                return Ok(0);
            }
            raw.trim().parse::<i32>().map_err(|_| {
                format!(
                    "{} must be a whole number.",
                    if name == "position" {
                        "Position"
                    } else {
                        "Text limit"
                    }
                )
            })
        };
        let position = parse_num("position", &self.draft.position)?;
        let text_limit_raw = values
            .get("text_limit")
            .map(String::as_str)
            .unwrap_or(&self.draft.text_limit);
        let text_limit = if text_limit_raw.trim().is_empty() {
            None
        } else {
            Some(parse_num("text_limit", &self.draft.text_limit)?)
        };
        let options_text = values
            .get("options")
            .map(String::as_str)
            .unwrap_or(&self.draft.options);
        let options = if matches!(field_type, FieldType::Choice | FieldType::MultiChoice) {
            serde_json::from_str(options_text).map_err(|_| {
                "Choice options must be a JSON array with stable code and label values.".to_owned()
            })?
        } else {
            json!([])
        };
        Ok(FieldInput {
            key,
            label: values.get("label").cloned().unwrap_or_default(),
            field_type,
            required: values.get("required").is_some_and(|value| value == "true"),
            help_text: values
                .get("help_text")
                .cloned()
                .filter(|value| !value.trim().is_empty()),
            position,
            options,
            reference_table: (field_type == FieldType::Reference)
                .then(|| self.draft.reference_table.clone()),
            text_limit,
            format: values
                .get("format")
                .cloned()
                .filter(|value| !value.is_empty()),
        })
    }

    async fn save(&mut self, scope: &Scope, values: &HashMap<String, String>) -> Result<()> {
        if self.editing.is_none() {
            if let Some(key) = values.get("key") {
                self.draft.key.clone_from(key);
            }
            if let Some(field_type) = values.get("type") {
                self.draft.field_type.clone_from(field_type);
            }
        }
        if let Some(label) = values.get("label") {
            self.draft.label.clone_from(label);
        }
        if let Some(required) = values.get("required") {
            self.draft.required = required == "true";
        }
        if let Some(help_text) = values.get("help_text") {
            self.draft.help_text.clone_from(help_text);
        }
        if let Some(position) = values.get("position") {
            self.draft.position.clone_from(position);
        }
        if let Some(text_limit) = values.get("text_limit") {
            self.draft.text_limit.clone_from(text_limit);
        }
        if let Some(format) = values.get("format") {
            self.draft.format.clone_from(format);
        }
        if let Some(options) = values.get("options") {
            self.draft.options.clone_from(options);
        }
        if let Some(reference_table) = values.get("reference_table") {
            self.draft.reference_table.clone_from(reference_table);
        }
        let input = match self.input(values) {
            Ok(input) => input,
            Err(message) => {
                self.form_error = Some(message);
                return Ok(());
            }
        };
        let result = if let Some((key, version)) = &self.editing {
            self.pages
                .fields
                .update(scope, &self.record_type, key, *version, &input)
                .await
        } else {
            self.pages
                .fields
                .define(scope, &self.record_type, &input)
                .await
        };
        match result {
            Ok(Outcome::Saved(_)) => { self.editing = None; self.form_error = None; self.dialog_open = false; self.rows_changed = true; self.set_flash(StatusKind::Success, "Custom field saved."); }
            Ok(Outcome::Refused(errors)) => { self.form_error = Some(errors.into_iter().map(|error| format!("{} {}", error.field, error.message)).collect::<Vec<_>>().join(". ")); }
            Ok(Outcome::Stale) => self.form_error = Some("This definition changed since it was opened. Close it and reopen the current definition before saving.".into()),
            Ok(Outcome::NotFound) => self.form_error = Some("This definition is no longer available. Close the form and reload the list.".into()),
            Err(Error::Forbidden | Error::Unauthorized) => self.form_error = Some("You no longer have permission to manage fields for this record type.".into()),
            Err(_) => self.form_error = Some("The definition could not be saved. Check the values and try again.".into()),
        }
        Ok(())
    }

    async fn archive(&mut self, scope: &Scope) -> Result<()> {
        let Some((key, version)) = self.confirm_archive.take() else {
            return Ok(());
        };
        match self
            .pages
            .fields
            .archive(scope, &self.record_type, &key, version)
            .await
        {
            Ok(Outcome::Saved(_)) => {
                self.rows_changed = true;
                self.set_flash(StatusKind::Success, format!("{key} archived."));
            }
            Ok(Outcome::Stale) => self.set_flash(
                StatusKind::Error,
                format!(
                    "{key} changed before it could be archived. Reload the list and try again."
                ),
            ),
            Ok(Outcome::NotFound) => {
                self.set_flash(StatusKind::Error, format!("{key} is no longer available."))
            }
            Ok(Outcome::Refused(errors)) => self.set_flash(
                StatusKind::Error,
                errors
                    .into_iter()
                    .map(|error| format!("{} {}", error.field, error.message))
                    .collect::<Vec<_>>()
                    .join(". "),
            ),
            Err(Error::Forbidden | Error::Unauthorized) => self.set_flash(
                StatusKind::Error,
                "You no longer have permission to manage fields for this record type.",
            ),
            Err(_) => self.set_flash(StatusKind::Error, "The field could not be archived."),
        }
        Ok(())
    }

    async fn restore(&mut self, scope: &Scope, key: &str) -> Result<()> {
        let Some(definition) = self
            .definitions
            .iter()
            .find(|definition| definition.key == key && definition.archived)
        else {
            return Ok(());
        };
        let version = definition.version;
        match self
            .pages
            .fields
            .restore(scope, &self.record_type, key, version)
            .await
        {
            Ok(Outcome::Saved(_)) => {
                self.rows_changed = true;
                self.set_flash(StatusKind::Success, format!("{key} restored."));
            }
            Ok(Outcome::Stale) => self.set_flash(
                StatusKind::Error,
                format!(
                    "{key} changed before it could be restored. Reload the list and try again."
                ),
            ),
            Ok(Outcome::NotFound) => {
                self.set_flash(StatusKind::Error, format!("{key} is no longer available."))
            }
            Ok(Outcome::Refused(errors)) => self.set_flash(
                StatusKind::Error,
                errors
                    .into_iter()
                    .map(|error| format!("{} {}", error.field, error.message))
                    .collect::<Vec<_>>()
                    .join(". "),
            ),
            Err(Error::Forbidden | Error::Unauthorized) => self.set_flash(
                StatusKind::Error,
                "You no longer have permission to manage fields for this record type.",
            ),
            Err(_) => self.set_flash(StatusKind::Error, "The field could not be restored."),
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<Html> {
        self.load(scope).await?;
        let can_manage_owner = self
            .pages
            .fields
            .can_manage_record_type(scope, &self.record_type)
            .await?;

        let header = render(&Header {
            title: Html::text("Custom fields"),
            subtitle: "Define the fields available on opted-in record types.",
            title_actions: Html::EMPTY,
            actions: if can_manage_owner {
                render(&Button {
                    variant: ButtonVariant::Primary,
                    event: "add",
                    id: "custom-fields-add",
                    body: Html::text("Add field"),
                    ..Button::default()
                })?
            } else {
                Html::EMPTY
            },
        })?;
        let mut type_tabs = Vec::new();
        for record_type in self.pages.fields.record_types() {
            type_tabs.push(Tab::button(
                "record_type",
                record_type.key,
                record_type.label,
                self.record_type == record_type.key,
            ));
        }
        let type_tabs = render(&Tabs {
            id: "custom-fields-record-types",
            label: "Record type",
            class: "mb-2",
            tabs: type_tabs,
        })?;
        let status_tabs = render(&Tabs {
            id: "custom-fields-status",
            label: "Definition status",
            class: "mb-3",
            tabs: vec![
                Tab::button("view", "active", "Active", !self.archived),
                Tab::button("view", "archived", "Archived", self.archived),
            ],
        })?;
        let mut tabs = Html::EMPTY;
        tabs.push(&type_tabs);
        tabs.push(&status_tabs);

        let toolbar = render(&FilterToolbar::new(
            "custom-fields-toolbar",
            vec![ToolbarControl::search(
                "custom-fields-search",
                "search",
                "Search fields",
                &self.state.search,
                "Search by label, key or type",
            )],
        ))?;
        let page = self.state.paginate(&self.definitions);
        let rows: Vec<FlexRow> = page
            .entries
            .iter()
            .map(|definition| {
                let mut row = FlexRow::new(&definition.key);
                let field = render(&DefinitionCell {
                    label: &definition.label,
                    key: &definition.key,
                })
                .unwrap_or(Html::EMPTY);
                row = row
                    .slot("field", field)
                    .slot("type", Html::text(type_label(definition.field_type)))
                    .slot(
                        "required",
                        Html::text(if definition.required { "Yes" } else { "No" }),
                    )
                    .slot("position", Html::text(&definition.position.to_string()));
                let mut actions = Html::EMPTY;
                if self.archived {
                    if can_manage_owner {
                        actions.push(
                            &render(&Button {
                                event: "restore",
                                name: "key",
                                value: &definition.key,
                                id: &format!("restore-{}", definition.key),
                                body: Html::text("Restore"),
                                ..Button::default()
                            })
                            .unwrap_or(Html::EMPTY),
                        );
                    }
                } else if can_manage_owner {
                    actions.push(
                        &render(&Button {
                            event: "edit",
                            name: "key",
                            value: &definition.key,
                            id: &format!("edit-{}", definition.key),
                            body: Html::text("Edit"),
                            ..Button::default()
                        })
                        .unwrap_or(Html::EMPTY),
                    );
                    actions.push(
                        &render(&Button {
                            event: "request_archive",
                            name: "key",
                            value: &definition.key,
                            variant: ButtonVariant::Danger,
                            id: &format!("archive-{}", definition.key),
                            body: Html::text("Archive"),
                            ..Button::default()
                        })
                        .unwrap_or(Html::EMPTY),
                    );
                }
                row.actions = actions;
                row
            })
            .collect();
        let mut table = FlexTable::new("custom-fields-table", self.columns.flex_columns(), rows);
        table.suggestions = self.columns.flex_suggestions();
        table.zoom = self.columns.zoom;
        table.caption = "Custom field definitions";
        table.framed = false;
        table.actions = can_manage_owner;
        if page.counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                if self.state.search.is_empty() {
                    if self.archived {
                        "No archived custom fields."
                    } else {
                        "No custom fields have been defined for this record type."
                    }
                } else {
                    "No custom fields match this search."
                },
            )));
        }
        let card_body = render(&table)?;
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        let flash = render(&FlashGroup {
            id: "custom-fields-flash",
            flash,
        })?;
        let mut card_body = card_body;
        card_body.push(&render(&Pagination::new(
            "custom-fields-pagination",
            page.counts,
            &PAGE_SIZES,
        ))?);
        let card = render(&Card {
            id: "custom-fields-card",
            inner_class: "p-0",
            body: card_body,
            ..Card::default()
        })?;
        let dialog = self.render_dialog()?;
        let confirmation = if let Some((key, _)) = &self.confirm_archive {
            render(&ConfirmDialog {
                id: "custom-field-archive-confirm",
                consequence: &format!("Archive {key}?"),
                detail: "The field will no longer be available for new values. Existing record values stay stored.",
                confirm: "Archive",
                working: "Archiving…",
                confirm_event: "archive",
                cancel_event: "cancel_archive",
            })?
        } else {
            Html::EMPTY
        };
        let mut body = Html::EMPTY;
        for fragment in [flash, header, tabs, toolbar, card, dialog, confirmation] {
            body.push(&fragment);
        }
        render(&UiPage {
            id: "custom-fields-index",
            body,
            ..UiPage::default()
        })
    }

    fn render_dialog(&self) -> Result<Html> {
        if !self.dialog_open {
            return Ok(Html::EMPTY);
        }
        let is_edit = self.editing.is_some();
        let mut body = Html::EMPTY;
        if let Some(message) = &self.form_error {
            body.push(&render(&bilibi_ui::Alert {
                kind: StatusKind::Error,
                id: "custom-field-form-error",
                class: "mb-3",
                body: Html::text(message),
            })?);
        }
        let mut key = Field::in_form("definition", "key")
            .label("Stable key")
            .value(&self.draft.key)
            .hint("Lowercase letters, digits and underscores; fixed after creation.")
            .required();
        if is_edit {
            key = key.readonly();
        }
        let label = Field::in_form("definition", "label")
            .label("Label")
            .value(&self.draft.label)
            .required();
        let mut type_field = Field::in_form("definition", "type")
            .label("Type")
            .kind(InputKind::Select)
            .value(&self.draft.field_type)
            .options(
                TYPES
                    .iter()
                    .map(|(label, value)| Choice::new(*label, *value))
                    .collect(),
            );
        if is_edit {
            type_field = type_field.disabled();
        }
        let required = Field::in_form("definition", "required")
            .label("Required")
            .kind(InputKind::Checkbox)
            .checked(self.draft.required);
        let help = Field::in_form("definition", "help_text")
            .label("Help text")
            .kind(InputKind::Textarea)
            .value(&self.draft.help_text);
        let position_attrs = bilibi_ui::forms::Attrs {
            min: Some("0".into()),
            ..bilibi_ui::forms::Attrs::default()
        };
        let position = Field::in_form("definition", "position")
            .label("Display order")
            .kind(InputKind::Number)
            .value(&self.draft.position)
            .attrs(position_attrs);
        let text_limit = Field::in_form("definition", "text_limit")
            .label("Text length limit")
            .kind(InputKind::Number)
            .value(&self.draft.text_limit)
            .hint("Optional; applies to text and long-text fields.");
        let format = Field::in_form("definition", "format")
            .label("Text format")
            .kind(InputKind::Select)
            .value(&self.draft.format)
            .prompt("No format")
            .options(vec![
                Choice::new("Email", "email"),
                Choice::new("URL", "url"),
                Choice::new("Phone", "phone"),
            ]);
        let options = Field::in_form("definition", "options").label("Choice options (JSON)").kind(InputKind::Textarea).value(&self.draft.options).hint("For Choice and Multiple choice, enter an array such as [{\"code\":\"active\",\"label\":\"Active\"}]. Preserve retired options with active:false.");
        for field in [
            render(&key)?,
            render(&label)?,
            render(&type_field)?,
            render(&required)?,
            render(&help)?,
            render(&position)?,
            render(&text_limit)?,
            render(&format)?,
            render(&options)?,
        ] {
            body.push(&field);
        }
        if self.draft.field_type == "reference" {
            let mut target = Field::in_form("definition", "reference_table")
                .label("Target record type")
                .kind(InputKind::Select)
                .value(&self.draft.reference_table)
                .prompt("Choose a record type...")
                .options(
                    self.pages
                        .fields
                        .reference_target_types()
                        .into_iter()
                        .map(|target| Choice::new(target.label, target.table))
                        .collect(),
                );
            if is_edit {
                target = target.disabled();
            }
            body.push(&render(&target)?);
        }
        let mut actions = render(&Button {
            variant: ButtonVariant::Primary,
            button_type: "submit",
            id: "custom-field-save",
            body: Html::text("Save field"),
            ..Button::default()
        })?;
        actions.push(&render(&Button {
            event: "close",
            id: "custom-field-cancel",
            body: Html::text("Cancel"),
            ..Button::default()
        })?);
        body = render(&Form {
            id: "custom-field-form",
            event: "save",
            change: false,
            class: "",
            actions,
            body,
        })?;
        render(&Modal {
            id: "custom-field-dialog",
            title: if is_edit {
                "Edit custom field"
            } else {
                "Add custom field"
            },
            cancel_event: "close",
            width: ModalWidth::Narrow,
            description: "Field definitions are scoped to this tenant and record type.",
            body,
            ..Modal::default()
        })
    }
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match request.input {
                LiveInput::Refresh => self.rows_changed = true,
                LiveInput::Params(values) => {
                    self.state = parse(values)?;
                    if let Some(record_type) = values.get("record_type").filter(|key| {
                        self.pages
                            .fields
                            .record_types()
                            .iter()
                            .any(|ty| ty.key == *key)
                    }) {
                        self.record_type = record_type.clone();
                    }
                    self.archived = values.get("archived").is_some_and(|value| value == "true");
                    self.rows_changed = true;
                }
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.render(request.scope).await?.as_str().to_owned();
            let mut query = self.state.to_params();
            query.insert("record_type".into(), self.record_type.clone());
            if self.archived {
                query.insert("archived".into(), "true".into());
            }
            Ok(LiveBody {
                html,
                query,
                rows: self.definitions.len(),
                ..LiveBody::default()
            })
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Custom fields")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut index = Index::new(self.clone(), request.query)?;
            Ok(PageBody {
                html: index.render(scope).await?.as_str().to_owned(),
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Ok(None);
            }
            Ok(Some(Box::new(Index::new(self.clone(), request.query)?)
                as Box<dyn LiveState<Scope, Error>>))
        })
    }
}
