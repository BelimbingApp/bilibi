//! `/authz/field-restrictions` (`base/authz.pages`, operation
//! `field-restrictions`): which fields of which records are restricted for
//! which roles, ported from Bilimbi
//! `apps/base/authz/lib/authz/web/field_restrictions_live.{ex,html.heex}`.
//!
//! Table-first, like Roles: one row per restricted field, and the header's
//! primary action opens the restrict dialog, which asks in the order an
//! operator thinks: the roles to restrict, then the tables, then the fields of
//! those tables, each shown as chips removable in place and read back in one
//! sentence before it is saved. Fields picked together commit as one
//! transaction (`FieldRestrictions::put_many`). A row's roles change in a
//! smaller dialog that also offers removal; removing, from the row or from
//! that dialog, confirms through the shared dialog. Saving with no role left
//! is not offered, so a restriction never silently becomes one for nobody.
//!
//! The route guard checks `admin.authz.field.manage` on every request and live
//! event, and the service re-checks it inside each write's transaction, so a
//! grant revoked while the page is open is refused: the host clears the page
//! on its next event or refresh, and a write that slips between the two checks
//! flashes a refusal instead of clearing anything.
//!
//! Rows the catalog no longer offers (a stale `table_id`, such as a
//! restriction on a table its module stopped contributing) stay
//! listed with their ids, can be removed, and cannot be re-restricted.
use crate::field_catalog::FieldCatalog;
use crate::restrictions::{Mode, Removed, RestrictionSummary, RoleChoice};
use crate::web::{render, Pages, TextCell, WhenCell};
use askama::Template;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::flex_table::key_id;
use bilibi_ui::forms::{Attrs, Field, InputKind};
use bilibi_ui::page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState};
use bilibi_ui::{
    params, BuiltinColumn, Button, ButtonVariant, Choice, ConfirmDialog, EmptyState, FlashMap,
    FlexRow, FlexTable, Html, IconButton, IconContext, IconKind, ListColumns, MultiSelect,
    StatusKind, TableEmpty, NO_FLASH,
};
use serde_json::Value;
use std::collections::HashMap;

/// The page operation this screen answers.
pub const OPERATION: &str = "field-restrictions";
/// The form namespace the pickers post (`restriction[role_ids][]`).
const FORM: &str = "restriction";
/// A field is picked as `table_id.field_id`, the subject spelling the audit
/// action already uses; a table id carries no dot.
const SEPARATOR: char = '.';
const DENIED: &str = "You do not have permission to manage field restrictions.";
/// At most this many values of one posted list are read.
const MAX_LIST: usize = 400;

fn builtins() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("table", "Table"),
        BuiltinColumn::new("field", "Field"),
        BuiltinColumn::new("restricted_for", "Restricted for"),
        BuiltinColumn::new("mode", "Readers see"),
        BuiltinColumn::new("since", "Since"),
    ]
}

/// What the pickers hold, as posted: ids as strings, in the order chosen.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Selection {
    role_ids: Vec<String>,
    table_ids: Vec<String>,
    field_keys: Vec<String>,
    /// `withhold` or `mask`; anything else withholds.
    mode: String,
}

enum Dialog {
    None,
    Add,
    Edit(RestrictionSummary),
}

#[derive(Template)]
#[template(path = "field_restrictions.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    title_actions: Html,
    actions: Html,
    table: Html,
    dialog: Html,
    confirm: Html,
}

#[derive(Template)]
#[template(path = "field_restrictions_add.html")]
struct AddDialog<'a> {
    flash: FlashMap<'a>,
    roles: Html,
    tables: Html,
    fields: Html,
    mode: Html,
    outcome: String,
    ready: bool,
    submit_label: String,
}

#[derive(Template)]
#[template(path = "field_restrictions_edit.html")]
struct EditDialog<'a> {
    flash: FlashMap<'a>,
    title: String,
    roles: Html,
    mode: Html,
    outcome: String,
    ready: bool,
    key: &'a str,
}

/// The roles a field is restricted for, one badge each.
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% for name in names %}<span class="mr-1 inline-flex">{% call ui::badge(dot=false) %}{{ name }}{% endcall %}</span>{% endfor %}"#,
    ext = "html"
)]
struct RoleBadges<'a> {
    names: &'a [String],
}

/// One mounted screen; also built for the plain HTTP paint.
pub(crate) struct Screen {
    pages: Pages,
    columns: ListColumns,
    catalog: &'static FieldCatalog,
    roles: Vec<RoleChoice>,
    restrictions: Vec<RestrictionSummary>,
    dialog: Dialog,
    pending_removal: Option<RestrictionSummary>,
    selection: Selection,
    flash: Option<(StatusKind, String)>,
}

impl Screen {
    pub(crate) async fn mount(pages: &Pages, scope: &Scope) -> Result<Self> {
        let mut screen = Self {
            pages: pages.clone(),
            columns: ListColumns::mount("field-restrictions", builtins())
                .map_err(|_| Error::Internal)?,
            catalog: pages.restrictions.catalog()?,
            roles: Vec::new(),
            restrictions: Vec::new(),
            dialog: Dialog::None,
            pending_removal: None,
            selection: Selection::default(),
            flash: None,
        };
        screen.load(scope).await?;
        Ok(screen)
    }

    /// Fresh rows and roles: another administrator's change shows at the next
    /// event or refresh. The route guard has already checked the capability
    /// for this round trip, so a refusal here is a race the host resolves.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        self.roles = self.pages.restrictions.roles(scope).await?;
        self.restrictions = self.pages.restrictions.list(scope).await?;
        let rows = std::mem::take(&mut self.restrictions);
        self.columns.load(&rows, key);
        self.restrictions = rows;
        Ok(())
    }

    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    fn flash_map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }

    fn find(&self, key: &str) -> Option<RestrictionSummary> {
        let (table, field) = key.split_once(SEPARATOR)?;
        self.restrictions
            .iter()
            .find(|r| r.table_id == table && r.field_id == field)
            .cloned()
    }

    fn close_dialog(&mut self) {
        self.dialog = Dialog::None;
        self.selection = Selection::default();
    }

    // --- options and labels ------------------------------------------------

    fn role_options(&self) -> Vec<Choice> {
        self.roles
            .iter()
            .map(|role| Choice::new(role.name.clone(), role.id.to_string()))
            .collect()
    }

    fn table_options(&self) -> Vec<Choice> {
        self.catalog
            .tables()
            .iter()
            .map(|table| Choice::new(table.label, table.id))
            .collect()
    }

    /// The fields of the chosen tables, in catalog order. With one table
    /// chosen the field's own label is enough; with more, each option names
    /// its table so "Email" of two tables cannot be confused.
    fn field_options(&self, table_ids: &[String]) -> Vec<Choice> {
        let chosen: Vec<_> = self
            .catalog
            .tables()
            .iter()
            .filter(|table| table_ids.iter().any(|id| id == table.id))
            .collect();
        let several = chosen.len() > 1;
        chosen
            .iter()
            .flat_map(|table| {
                table.fields.iter().map(move |field| {
                    let label = if several {
                        format!("{} › {}", table.label, field.label)
                    } else {
                        field.label.to_owned()
                    };
                    Choice::new(label, format!("{}{SEPARATOR}{}", table.id, field.id))
                })
            })
            .collect()
    }

    fn chosen_roles(&self) -> Vec<String> {
        self.roles
            .iter()
            .filter(|role| self.selection.role_ids.contains(&role.id.to_string()))
            .map(|role| role.name.clone())
            .collect()
    }

    fn chosen_fields(&self) -> Vec<String> {
        self.catalog
            .tables()
            .iter()
            .flat_map(|table| {
                table.fields.iter().filter_map(move |field| {
                    let key = format!("{}{SEPARATOR}{}", table.id, field.id);
                    self.selection
                        .field_keys
                        .contains(&key)
                        .then(|| format!("{} › {}", table.label, field.label))
                })
            })
            .collect()
    }

    /// A multi-select submits nothing at all when every box is clear, and a
    /// field of a table no longer chosen is not a choice.
    fn normalize(&mut self, selection: Selection) {
        let offered: Vec<String> = self
            .field_options(&selection.table_ids)
            .into_iter()
            .map(|choice| choice.value)
            .collect();
        self.selection = Selection {
            role_ids: selection.role_ids,
            table_ids: selection.table_ids,
            field_keys: selection
                .field_keys
                .into_iter()
                .filter(|key| offered.contains(key))
                .collect(),
            mode: selection.mode,
        };
    }

    fn mode(&self) -> Mode {
        Mode::parse(&self.selection.mode).unwrap_or_default()
    }

    /// Withhold, or mask: offered for every field chosen only when each is
    /// marked personal, since the kind shapes what a masked reader sees.
    fn mode_picker(&self, maskable: bool) -> Result<Html> {
        let mut field = Field::in_form(FORM, "mode")
            .id("field-restrictions-mode")
            .label("What these roles see")
            .kind(InputKind::Select)
            .options(vec![
                Choice::new("Nothing: the field is withheld", "withhold"),
                Choice::new("A partial value they may reveal with a reason", "mask"),
            ])
            .value(self.mode().as_str());
        field = if maskable {
            field.hint("A partial value shows, for example, a•••@example.com or •••• 1234. Revealing the full value needs privacy.field.reveal and is recorded.")
        } else {
            field
                .hint("Only fields that hold a person's data can show a partial value.")
                .attrs(Attrs {
                    disabled: true,
                    ..Attrs::default()
                })
        };
        render(field)
    }

    /// Whether every chosen field is marked personal.
    fn chosen_maskable(&self) -> bool {
        let fields = self.fields();
        !fields.is_empty()
            && fields.iter().all(|(table, field)| {
                self.catalog
                    .field(table, field)
                    .is_some_and(|(_, f)| f.personal.is_some())
            })
    }

    fn role_ids(&self) -> Vec<i64> {
        self.selection
            .role_ids
            .iter()
            .filter_map(|id| params::positive_id(Some(id)))
            .collect()
    }

    fn fields(&self) -> Vec<(String, String)> {
        self.selection
            .field_keys
            .iter()
            .filter_map(|key| key.split_once(SEPARATOR))
            .map(|(table, field)| (table.to_owned(), field.to_owned()))
            .collect()
    }

    // --- events --------------------------------------------------------------

    async fn event(&mut self, scope: &Scope, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "new" => {
                self.flash = None;
                self.dialog = Dialog::Add;
                self.selection = Selection::default();
            }
            "edit" => {
                if let Some(restriction) = get("key").and_then(|key| self.find(key)) {
                    if restriction.restrictable {
                        self.flash = None;
                        self.selection = Selection {
                            role_ids: restriction.role_ids.iter().map(i64::to_string).collect(),
                            mode: restriction.mode.as_str().to_owned(),
                            ..Selection::default()
                        };
                        self.dialog = Dialog::Edit(restriction);
                    }
                }
            }
            "close_dialog" => self.close_dialog(),
            "validate" => {
                if !matches!(self.dialog, Dialog::None) {
                    self.normalize(Selection {
                        role_ids: list(value, "role_ids"),
                        table_ids: list(value, "table_ids"),
                        field_keys: list(value, "field_keys"),
                        mode: scalar(value, "mode"),
                    });
                }
            }
            // A chip's remove button names the input and the value; dropping
            // the value is a validate without the browser's help.
            "deselect" => {
                if let (Some(key), Some(option)) = (get("name").and_then(picker), get("option")) {
                    let mut selection = self.selection.clone();
                    let kept = match key {
                        "role_ids" => &mut selection.role_ids,
                        "table_ids" => &mut selection.table_ids,
                        _ => &mut selection.field_keys,
                    };
                    kept.retain(|value| value != option);
                    self.normalize(selection);
                }
            }
            "restrict" => self.restrict(scope).await?,
            "save_roles" => self.save_roles(scope).await?,
            // Asked from the list or from the edit dialog; the edit dialog
            // closes so the confirmation is the one dialog open.
            "request_remove" => {
                let restriction = get("key").and_then(|key| self.find(key));
                self.close_dialog();
                self.pending_removal = restriction;
            }
            "cancel_remove" => self.pending_removal = None,
            "remove" => self.remove(scope).await?,
            "grid" => {
                let values: HashMap<String, String> = value
                    .as_object()
                    .into_iter()
                    .flatten()
                    .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_owned())))
                    .collect();
                self.columns.handle(&values);
            }
            "clear-flash" => self.flash = None,
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    async fn restrict(&mut self, scope: &Scope) -> Result<()> {
        if !matches!(self.dialog, Dialog::Add) {
            return Ok(());
        }
        let (fields, roles) = (self.fields(), self.role_ids());
        if fields.is_empty() || roles.is_empty() {
            return Ok(());
        }
        let mode = if self.chosen_maskable() {
            self.mode()
        } else {
            Mode::Withhold
        };
        match self
            .pages
            .restrictions
            .put_many(scope, &fields, &roles, mode)
            .await
        {
            Ok(saved) => {
                self.flash(StatusKind::Success, saved_message(&saved));
                self.close_dialog();
                self.load(scope).await
            }
            Err(error) => self.refusal(error, &roles),
        }
    }

    async fn save_roles(&mut self, scope: &Scope) -> Result<()> {
        let Dialog::Edit(restriction) = &self.dialog else {
            return Ok(());
        };
        let roles = self.role_ids();
        if roles.is_empty() {
            return Ok(());
        }
        let (table, field) = (restriction.table_id.clone(), restriction.field_id.clone());
        let mode = if restriction.maskable {
            self.mode()
        } else {
            Mode::Withhold
        };
        match self
            .pages
            .restrictions
            .put(scope, &table, &field, &roles, mode)
            .await
        {
            Ok(saved) => {
                self.flash(StatusKind::Success, saved_message(&[saved]));
                self.close_dialog();
                self.load(scope).await
            }
            Err(error) => self.refusal(error, &roles),
        }
    }

    /// A refused or failed write is a flash in the open dialog, never a
    /// page-clearing error; only a lost session propagates.
    fn refusal(&mut self, error: Error, roles: &[i64]) -> Result<()> {
        let message = match error {
            Error::Unauthorized => return Err(Error::Unauthorized),
            Error::Forbidden => DENIED,
            Error::Invalid
                if roles
                    .iter()
                    .any(|id| !self.roles.iter().any(|r| r.id == *id)) =>
            {
                "Choose roles of this tenant."
            }
            Error::Invalid => {
                "Choose fields the catalog offers; one of these is no longer restrictable."
            }
            _ => "The restriction could not be saved.",
        };
        self.flash(StatusKind::Error, message);
        Ok(())
    }

    async fn remove(&mut self, scope: &Scope) -> Result<()> {
        let Some(restriction) = self.pending_removal.take() else {
            return Ok(());
        };
        match self
            .pages
            .restrictions
            .remove(scope, &restriction.table_id, &restriction.field_id)
            .await
        {
            Ok(Removed::Removed) => self.flash(
                StatusKind::Success,
                format!("{} is no longer restricted.", subject(&restriction)),
            ),
            Ok(Removed::NotFound) => {
                self.flash(StatusKind::Info, "That restriction was already removed.")
            }
            Err(Error::Unauthorized) => return Err(Error::Unauthorized),
            Err(Error::Forbidden) => self.flash(StatusKind::Error, DENIED),
            Err(_) => self.flash(StatusKind::Error, "The restriction could not be removed."),
        }
        self.load(scope).await
    }

    // --- rendering -----------------------------------------------------------

    pub(crate) async fn body(&self, scope: &Scope) -> Result<String> {
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
        let mut rows = Vec::with_capacity(self.restrictions.len());
        for restriction in &self.restrictions {
            let key = key(restriction);
            let dom = format!("field-restriction-{}", key_id(&key));
            let subject = subject(restriction);
            let since =
                jiff::Timestamp::try_from(restriction.since).map_err(|_| Error::Internal)?;
            let rendered = display.render(
                since,
                bilibi_datetime::Format::DateTime,
                bilibi_datetime::Precision::Minute,
            );
            let mut actions = render(IconButton {
                id: &format!("{dom}-edit"),
                context: IconContext::Inline,
                event: "edit",
                name: "key",
                value: &key,
                disabled: !restriction.restrictable,
                title: if restriction.restrictable {
                    ""
                } else {
                    "This field is no longer in the catalog; remove the restriction instead"
                },
                ..IconButton::new(
                    "edit",
                    &format!("Change the roles {subject} is restricted for"),
                )
            })?;
            actions.push(&render(IconButton {
                id: &format!("{dom}-remove"),
                context: IconContext::Inline,
                kind: IconKind::Danger,
                event: "request_remove",
                name: "key",
                value: &key,
                ..IconButton::new("delete", &format!("Remove the restriction on {subject}"))
            })?);
            let mut row = FlexRow::new(key.clone())
                .slot(
                    "table",
                    render(TextCell {
                        class: "whitespace-nowrap text-ink",
                        text: &restriction.table_label,
                    })?,
                )
                .slot(
                    "field",
                    render(TextCell {
                        class: "whitespace-nowrap text-ink",
                        text: &restriction.field_label,
                    })?,
                )
                .slot(
                    "restricted_for",
                    render(RoleBadges {
                        names: &restriction.role_names,
                    })?,
                )
                .slot(
                    "mode",
                    render(TextCell {
                        class: "whitespace-nowrap text-ink",
                        text: match restriction.mode {
                            Mode::Withhold => "Nothing",
                            Mode::Mask => "A partial value",
                        },
                    })?,
                )
                .slot(
                    "since",
                    render(WhenCell {
                        id: format!("{dom}-since"),
                        iso: rendered.iso,
                        text: rendered.text,
                        company: rendered.company,
                        utc: rendered.utc,
                        mode: rendered.mode.as_str(),
                    })?,
                );
            row.dom_id = Some(dom);
            row.actions = actions;
            rows.push(row);
        }
        let mut table = FlexTable::new("field-restrictions", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.framed = false;
        table.actions = true;
        table.caption = "Field Restrictions";
        if self.restrictions.is_empty() {
            table.empty = Some(TableEmpty::State(EmptyState::missing(
                "No fields are restricted",
                "Use Restrict fields to hide fields from some roles.",
            )));
        }
        let flash = self.flash_map();
        let dialog = match &self.dialog {
            Dialog::None => Html::EMPTY,
            Dialog::Add => self.add_dialog(flash)?,
            Dialog::Edit(restriction) => self.edit_dialog(flash, restriction)?,
        };
        let confirm = match &self.pending_removal {
            Some(restriction) => render(ConfirmDialog {
                id: "field-restrictions-remove-confirm",
                consequence: &format!(
                    "{} will be visible to every role again.",
                    subject(restriction)
                ),
                detail: "Anyone who may open the record, its list or its audit log will see the value at once.",
                confirm: "Remove restriction",
                working: "Removing…",
                confirm_event: "remove",
                cancel_event: "cancel_remove",
            })?,
            None => Html::EMPTY,
        };
        let view = View {
            flash,
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "field-restrictions-pin",
                nav_pin: "nav-admin-authz-field-restrictions",
                ..IconButton::new("bilimbi-pin", "Pin Field Restrictions to sidebar")
            })?,
            actions: render(Button {
                variant: ButtonVariant::Primary,
                id: "field-restrictions-new",
                button_type: "button",
                event: "new",
                body: Html::text("Restrict fields"),
                ..Button::default()
            })?,
            table: render(table)?,
            dialog,
            confirm,
        };
        Ok(render(view)?.as_str().to_owned())
    }

    fn roles_picker(&self) -> Result<Html> {
        render(
            MultiSelect::in_form(FORM, "role_ids")
                .id("field-restrictions-roles")
                .label("Choose the roles to restrict field access")
                .placeholder("Add roles")
                .chips("deselect")
                .options(self.role_options())
                .values(self.selection.role_ids.clone()),
        )
    }

    fn add_dialog(&self, flash: FlashMap<'_>) -> Result<Html> {
        let field_options = self.field_options(&self.selection.table_ids);
        let (chosen_fields, chosen_roles) = (self.chosen_fields(), self.chosen_roles());
        let ready = !chosen_fields.is_empty() && !chosen_roles.is_empty();
        let fields = MultiSelect::in_form(FORM, "field_keys")
            .id("field-restrictions-fields")
            .label("Fields")
            .placeholder(if field_options.is_empty() {
                "Choose a table first"
            } else {
                "Add fields"
            })
            .chips("deselect")
            .attrs(Attrs {
                disabled: field_options.is_empty(),
                ..Attrs::default()
            })
            .options(field_options)
            .values(self.selection.field_keys.clone());
        render(AddDialog {
            flash,
            roles: self.roles_picker()?,
            tables: render(
                MultiSelect::in_form(FORM, "table_ids")
                    .id("field-restrictions-tables")
                    .label("Tables")
                    .placeholder("Add tables")
                    .chips("deselect")
                    .options(self.table_options())
                    .values(self.selection.table_ids.clone()),
            )?,
            fields: render(fields)?,
            mode: self.mode_picker(self.chosen_maskable())?,
            outcome: outcome_sentence(&chosen_fields, &chosen_roles),
            ready,
            submit_label: match chosen_fields.len() {
                0 => "Restrict fields".to_owned(),
                1 => "Restrict field".to_owned(),
                n => format!("Restrict {n} fields"),
            },
        })
    }

    fn edit_dialog(&self, flash: FlashMap<'_>, restriction: &RestrictionSummary) -> Result<Html> {
        let chosen_roles = self.chosen_roles();
        let subject = subject(restriction);
        let outcome = if chosen_roles.is_empty() {
            "No role is left. Use Remove restriction to let every role see the value again."
                .to_owned()
        } else {
            outcome_sentence(std::slice::from_ref(&subject), &chosen_roles)
        };
        render(EditDialog {
            flash,
            title: format!("Restrict {subject}"),
            roles: self.roles_picker()?,
            mode: self.mode_picker(restriction.maskable)?,
            outcome,
            ready: !chosen_roles.is_empty(),
            key: &key(restriction),
        })
    }
}

impl LiveState<Scope, Error> for Screen {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let before = self.flash.clone();
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => self.load(request.scope).await?,
                LiveInput::Event(value) => self.event(request.scope, value).await?,
            }
            let html = self.body(request.scope).await?;
            let notice = match &self.flash {
                Some((StatusKind::Success, message)) if self.flash != before => message.clone(),
                _ => String::new(),
            };
            Ok(LiveBody {
                html,
                notice,
                ..LiveBody::default()
            })
        })
    }
}

/// The values of one posted picker: an array (every checked box) or one
/// string; blanks dropped, duplicates collapsed, bounded before use.
/// A single-value control (the mode select posts `restriction[mode]`).
fn scalar(payload: &Value, key: &str) -> String {
    payload
        .get(format!("{FORM}[{key}]"))
        .and_then(Value::as_str)
        .filter(|text| text.len() <= 32)
        .unwrap_or_default()
        .to_owned()
}
fn list(payload: &Value, key: &str) -> Vec<String> {
    let mut values: Vec<String> = Vec::new();
    let mut keep = |value: &Value| {
        if let Some(text) = value.as_str() {
            if !text.is_empty() && text.len() <= 401 && !values.iter().any(|v| v == text) {
                values.push(text.to_owned());
            }
        }
    };
    match payload.get(format!("{FORM}[{key}][]")) {
        Some(Value::Array(items)) => items.iter().take(MAX_LIST).for_each(&mut keep),
        Some(single) => keep(single),
        None => {}
    }
    values
}

/// The input name a chip reports back, mapped to the picker it belongs to.
fn picker(name: &str) -> Option<&'static str> {
    match name {
        "restriction[role_ids][]" => Some("role_ids"),
        "restriction[table_ids][]" => Some("table_ids"),
        "restriction[field_keys][]" => Some("field_keys"),
        _ => None,
    }
}

fn key(restriction: &RestrictionSummary) -> String {
    format!(
        "{}{SEPARATOR}{}",
        restriction.table_id, restriction.field_id
    )
}

fn subject(restriction: &RestrictionSummary) -> String {
    format!("{} › {}", restriction.table_label, restriction.field_label)
}

/// One sentence saying what saving will do, once there is something to say:
/// the roles named will not see the fields named.
fn outcome_sentence(fields: &[String], roles: &[String]) -> String {
    if fields.is_empty() || roles.is_empty() {
        return String::new();
    }
    format!(
        "{} will read {} as Restricted. Every other role sees the value.",
        join(roles),
        join(fields)
    )
}

fn saved_message(saved: &[RestrictionSummary]) -> String {
    match saved {
        [one] => format!(
            "{} is restricted for {}.",
            subject(one),
            join(&one.role_names)
        ),
        [first, ..] => format!(
            "{} fields are restricted for {}.",
            saved.len(),
            join(&first.role_names)
        ),
        [] => String::new(),
    }
}

fn join(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [first, second] => format!("{first} and {second}"),
        [head @ .., last] => format!("{} and {last}", head.join(", ")),
    }
}
