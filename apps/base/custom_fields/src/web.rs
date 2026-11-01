//! The administration page (`base/custom_fields.pages`): `/admin/custom-fields`
//! lists the organisation's custom fields for every record type that offers
//! them, read first and edited in place. One list: the record type and the
//! status are a filter and a column, not separate pages. The list follows `base/picklists`' values page.
//!
//! Every change goes through the `base.custom_fields.*` contracts agents call
//! (`PageContracts::submit`), which recheck the capability in their own
//! transaction. A control the person may not use is hidden; a refused event is
//! a flash on the page, a refused inline edit an outcome on its field. Archive
//! and restore confirm through the shared dialog, which holds the field the
//! person picked, never a client value.
use crate::{
    definitions::{CustomFields, Definition, Kind, HELP_MAX, KEY_MAX},
    LABEL_MAX, MANAGE, PAGES,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice, ColumnsOutcome,
    CommitStates, CommitStatus, ConfirmDialog, CreateButton, Field, FilterToolbar, FlashMap,
    FlexRow, FlexTable, Form, Header, Html, IconButton, IconKind, InlineEdit, InlineField,
    InputKind, ListColumns, ListSpec, ListState, Modal, Pagination, RowMode, StatusKind,
    TableEmpty, ToolbarControl, NO_FLASH,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

pub(crate) const INDEX: &str = "/admin/custom-fields";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission to change custom fields.";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 50,
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([
            ("record_type".into(), bilibi_ui::Filter::string("")),
            (
                "status".into(),
                bilibi_ui::Filter::one_of(&["active", "all"], "active"),
            ),
        ]),
        omit_blank: vec!["search".into(), "record_type".into()],
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("custom field list spec")
});

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub fields: Arc<CustomFields>,
    pub contracts: Arc<PageContracts>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn flash_map(flash: &Option<(StatusKind, String)>) -> FlashMap<'_> {
    let mut map = NO_FLASH;
    if let Some((kind, message)) = flash {
        match kind {
            StatusKind::Success => map.success = message,
            StatusKind::Error => map.error = message,
            StatusKind::Warning => map.warning = message,
            StatusKind::Info => map.info = message,
        }
    }
    map
}
/// The shared dismiss, kept for every page that shows a flash.
fn clear_flash(flash: &mut Option<(StatusKind, String)>, key: Option<&str>) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| Some(kind.name()) == key)
    {
        *flash = None;
    }
}

#[derive(Template)]
#[template(path = "custom_fields.html")]
struct View<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    header: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}

#[derive(Template)]
#[template(
    source = r#"<span class="font-mono text-xs text-ink">{{ text }}</span>"#,
    ext = "html"
)]
struct KeyCell<'a> {
    text: &'a str,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Archive,
    Restore,
}
/// The new-field dialog: what the person typed and what the contract refused.
#[derive(Default)]
struct Draft {
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
}

/// A contract's answer as the page reports it: done, or the sentence to show.
fn outcome(
    result: std::result::Result<Value, ContractError>,
) -> Result<std::result::Result<(), String>> {
    Ok(match result {
        Ok(_) => Ok(()),
        // Base Step-up and a lost session: the host prompts and comes back.
        Err(ContractError::Backend(
            error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
        )) => return Err(error),
        Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
            Err(DENIED.into())
        }
        Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => {
            Err("This field no longer exists; reload the page.".into())
        }
        Err(ContractError::InvalidInput(fields)) => Err(fields
            .iter()
            .flat_map(|(field, messages)| {
                messages
                    .iter()
                    .map(move |message| format!("{} {message}", field.replace('_', " ")))
            })
            .collect::<Vec<_>>()
            .join("; ")),
        Err(_) => Err(
            "The change was not saved. Try again, and tell your administrator if it keeps failing."
                .into(),
        ),
    })
}

fn columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("label", "Label"),
        BuiltinColumn::new("key", "Key"),
        BuiltinColumn::new("record_type", "Record"),
        BuiltinColumn::new("kind", "Type"),
        BuiltinColumn::new("help", "Help text"),
        BuiltinColumn::new("status", "Status"),
    ]
}

/// A field as one id: record types and keys hold no `.`, so it splits back.
fn row_id(definition: &Definition) -> String {
    format!("{}.{}", definition.record_type, definition.key)
}

struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    /// Every definition the person may see, as last read.
    definitions: Vec<Definition>,
    shown: usize,
    statuses: CommitStates,
    notice: String,
    draft: Option<Draft>,
    confirm: Option<(Confirm, Definition)>,
    flash: Option<(StatusKind, String)>,
}

impl Index {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("custom-fields", columns()).map_err(|_| Error::Internal)?,
            rows_changed: true,
            definitions: Vec::new(),
            shown: 0,
            statuses: CommitStates::default(),
            notice: String::new(),
            draft: None,
            confirm: None,
            flash: None,
        })
    }
    fn definition(&self, id: &str) -> Option<&Definition> {
        self.definitions
            .iter()
            .find(|definition| row_id(definition) == id)
    }
    fn call<'a>(
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract,
        }
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        self.notice.clear();
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self.state.apply_filters(
                &params::form_values_checked(value, FILTERS_FORM, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(
                &params::string_values_checked(value, params::FIELD_LIMIT)
                    .map_err(Error::refused)?,
            ) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, get("key")),
            "new" => self.draft = Some(Draft::default()),
            "close_new" => self.draft = None,
            "create" => {
                let posted = params::form_values_checked(value, "field", params::FIELD_LIMIT)
                    .map_err(Error::refused)?;
                let text = |key: &str| posted.get(key).cloned().unwrap_or_default();
                let mut input = json!({
                    "record_type": text("record_type"),
                    "key": text("key"),
                    "label": text("label"),
                    "kind": text("kind"),
                });
                if !text("help").trim().is_empty() {
                    input["help"] = json!(text("help"));
                }
                if !text("picklist").is_empty() {
                    input["picklist"] = json!(text("picklist"));
                }
                let result = self
                    .pages
                    .contracts
                    .submit(
                        &Self::call(scope, credential, "create", "base.custom_fields.define"),
                        input,
                    )
                    .await;
                let draft = Draft {
                    values: posted,
                    errors: BTreeMap::new(),
                };
                match result {
                    Ok(_) => {
                        self.draft = None;
                        self.flash = Some((
                            StatusKind::Success,
                            format!(
                                "“{}” was added.",
                                draft.values.get("label").map_or("", String::as_str).trim()
                            ),
                        ));
                    }
                    Err(ContractError::InvalidInput(fields)) => {
                        self.draft = Some(Draft {
                            errors: fields,
                            ..draft
                        });
                    }
                    other => {
                        self.draft = Some(draft);
                        if let Err(message) = outcome(other)? {
                            self.flash = Some((StatusKind::Error, message));
                        }
                    }
                }
            }
            "save_field" => {
                let field = get("field").unwrap_or_default();
                if !["label", "help"].contains(&field) {
                    return Err(Error::Invalid);
                }
                let id = get("id").unwrap_or_default();
                let submitted = get("value").unwrap_or_default();
                let version: i64 = get("version")
                    .unwrap_or_default()
                    .parse()
                    .map_err(|_| Error::Invalid)?;
                let Some(current) = self.definition(id).cloned() else {
                    self.notice = "This field no longer exists; reload the page.".into();
                    return Ok(());
                };
                let mut input = json!({
                    "record_type": current.record_type,
                    "key": current.key,
                    "label": current.label,
                    "version": version,
                });
                if let Some(help) = &current.help {
                    input["help"] = json!(help);
                }
                input[field] = json!(submitted);
                let result = self
                    .pages
                    .contracts
                    .submit(
                        &Self::call(scope, credential, "save_field", "base.custom_fields.update"),
                        input,
                    )
                    .await;
                let status = match result {
                    // The refusal names the field itself; keep only the reasons.
                    Err(ContractError::InvalidInput(fields)) => {
                        let reasons: Vec<String> = fields.into_values().flatten().collect();
                        let reasons: Vec<&str> = reasons.iter().map(String::as_str).collect();
                        CommitStatus::refusal(field, submitted, &reasons)
                    }
                    other => match outcome(other)? {
                        Ok(()) => CommitStatus::Saved,
                        Err(message) if message == DENIED => {
                            self.notice = DENIED.into();
                            self.statuses.clear_saved();
                            CommitStatus::Error(DENIED.into())
                        }
                        Err(message) => CommitStatus::refusal(field, submitted, &[&message]),
                    },
                };
                self.statuses.put(format!("{id}-{field}"), status);
            }
            "request_archive" | "request_restore" => {
                let step = if get("type") == Some("request_archive") {
                    Confirm::Archive
                } else {
                    Confirm::Restore
                };
                if let Some(definition) = self.definition(get("key").unwrap_or_default()).cloned() {
                    self.flash = None;
                    self.confirm = Some((step, definition));
                }
            }
            "cancel_confirm" => self.confirm = None,
            "archive" | "restore" => {
                let Some((step, definition)) = self.confirm.take() else {
                    return Ok(());
                };
                let (event, contract, done) = match step {
                    Confirm::Archive => (
                        "archive",
                        "base.custom_fields.archive",
                        format!("“{}” was archived.", definition.label),
                    ),
                    Confirm::Restore => (
                        "restore",
                        "base.custom_fields.restore",
                        format!("“{}” was restored.", definition.label),
                    ),
                };
                if get("type") != Some(event) {
                    return Err(Error::Invalid);
                }
                let result = self
                    .pages
                    .contracts
                    .submit(
                        &Self::call(scope, credential, event, contract),
                        json!({"record_type": definition.record_type, "key": definition.key, "version": definition.version}),
                    )
                    .await;
                self.flash = Some(match outcome(result)? {
                    Ok(()) => (StatusKind::Success, done),
                    Err(message) => (StatusKind::Error, message),
                });
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    fn inline(
        &self,
        definition: &Definition,
        field: &str,
        label: &str,
        text: &str,
        edits: bool,
    ) -> InlineField {
        let id = row_id(definition);
        let mut inline = InlineField::new(
            format!("custom-field-{}-{field}", definition.id),
            field,
            label,
            text,
        )
        .version(definition.version)
        .status(self.statuses.get(&format!("{id}-{field}")));
        inline.id_value = id;
        if field != "label" {
            inline = inline.allow_empty();
        }
        if !edits {
            inline = inline.readonly();
        }
        inline
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let edits = effective_capabilities(&**client, scope)
            .await?
            .contains(MANAGE);
        self.definitions = pages
            .fields
            .definitions(&**client, scope, None, true)
            .await?;
        drop(client);
        let all_status = self.state.filters.get("status").is_some_and(|s| s == "all");
        let record_type: String = self
            .state
            .filters
            .get("record_type")
            .cloned()
            .unwrap_or_default();
        let search = self.state.search.to_lowercase();
        let shown: Vec<Definition> = self
            .definitions
            .iter()
            .filter(|definition| all_status || !definition.archived())
            .filter(|definition| record_type.is_empty() || definition.record_type == *record_type)
            .filter(|definition| {
                search.is_empty()
                    || definition.key.contains(&search)
                    || definition.label.to_lowercase().contains(&search)
            })
            .cloned()
            .collect();
        let page = self.state.paginate(&shown);
        let (entries, counts) = (page.entries, page.counts);
        self.shown = entries.len();
        if self.rows_changed {
            self.columns.load(entries, row_id);
        }
        let mut actions = Html::EMPTY;
        if edits && pages.fields.record_types().next().is_some() {
            actions.push(&render(CreateButton {
                noun: "Field",
                id: "custom-field-new",
                event: "new",
                button_type: "button",
                ..CreateButton::default()
            })?);
        }
        let header = render(Header {
            title: Html::text("Custom fields"),
            subtitle: "Extra fields your organisation adds to its records. A field's key and type cannot be changed.",
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let mut records = vec![Choice::new("All records", "")];
        records.extend(
            pages
                .fields
                .record_types()
                .map(|record| Choice::new(record.label, record.id)),
        );
        let toolbar = render(FilterToolbar::new(
            "custom-fields-filters",
            vec![
                ToolbarControl::search(
                    "custom-fields-search",
                    "search",
                    "Search fields",
                    &form["search"],
                    "Search by label or key...",
                ),
                ToolbarControl::select(
                    "custom-fields-record-type",
                    "record_type",
                    "Record",
                    &form["record_type"],
                    records,
                ),
                ToolbarControl::select(
                    "custom-fields-status",
                    "status",
                    "Status",
                    &form["status"],
                    vec![
                        Choice::new("Active fields", "active"),
                        Choice::new("Include archived", "all"),
                    ],
                ),
            ],
        ))?;
        let compact = self.columns.mode() == RowMode::Compact;
        let mut rows = Vec::with_capacity(entries.len());
        for definition in entries {
            let record = pages
                .fields
                .record_type(&definition.record_type)
                .map_or(definition.record_type.as_str(), |record| record.label);
            let kind = match (&definition.picklist, compact) {
                (Some(list), false) => format!("{} · {list}", definition.kind.label()),
                _ => definition.kind.label().to_owned(),
            };
            let mut row = FlexRow::new(row_id(definition))
                .slot(
                    "label",
                    render(InlineEdit {
                        field: self.inline(definition, "label", "Label", &definition.label, edits),
                    })?,
                )
                .slot(
                    "key",
                    render(KeyCell {
                        text: &definition.key,
                    })?,
                )
                .slot("record_type", Html::text(record))
                .slot("kind", Html::text(&kind))
                .slot(
                    "help",
                    render(InlineEdit {
                        field: self.inline(
                            definition,
                            "help",
                            "Help text",
                            definition.help.as_deref().unwrap_or(""),
                            edits,
                        ),
                    })?,
                )
                .slot(
                    "status",
                    render(if definition.archived() {
                        Badge::status(BadgeKind::Neutral, "Archived")
                    } else {
                        Badge::status(BadgeKind::Success, "Active")
                    })?,
                );
            if edits {
                row.actions = self.actions(definition)?;
            }
            row.dom_id = Some(format!("custom-field-{}", definition.id));
            rows.push(row);
        }
        let mut table = FlexTable::new("custom-fields", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Custom fields";
        table.framed = false;
        table.actions = edits;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(
                if pages.fields.record_types().next().is_none() {
                    "No installed module offers custom fields yet."
                } else if search.is_empty() && record_type.is_empty() && all_status {
                    "No custom fields have been added."
                } else if search.is_empty() && record_type.is_empty() {
                    "No active custom fields. Include archived fields to see them."
                } else {
                    "No field matches this search."
                },
            )));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "custom-fields-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "custom-fields-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match (&self.confirm, &self.draft) {
            (Some((Confirm::Archive, definition)), _) => render(ConfirmDialog {
                id: "custom-field-archive-confirm",
                consequence: &format!("Archive “{}”?", definition.label),
                detail: "People stop seeing it in forms, lists and agent tools. Records keep the values already saved, and you can restore the field later.",
                confirm: "Archive",
                working: "Archiving…",
                confirm_event: "archive",
                cancel_event: "cancel_confirm",
            })?,
            (Some((Confirm::Restore, definition)), _) => render(ConfirmDialog {
                id: "custom-field-restore-confirm",
                consequence: &format!("Restore “{}”?", definition.label),
                detail: "It comes back with the values records kept.",
                confirm: "Restore",
                working: "Restoring…",
                confirm_event: "restore",
                cancel_event: "cancel_confirm",
            })?,
            (None, Some(draft)) => self.new_dialog(draft)?,
            (None, None) => Html::EMPTY,
        };
        View {
            id: "custom-fields-page",
            flash: flash_map(&self.flash),
            header,
            toolbar,
            card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// Archive or restore.
    fn actions(&self, definition: &Definition) -> Result<Html> {
        let id = row_id(definition);
        let (icon, label, event, action, kind) = if definition.archived() {
            (
                "unarchive",
                "Restore",
                "request_restore",
                "restore",
                IconKind::Neutral,
            )
        } else {
            (
                "archive",
                "Archive",
                "request_archive",
                "archive",
                IconKind::Danger,
            )
        };
        let dom = format!("custom-field-{}-{action}", definition.id);
        render(IconButton {
            id: &dom,
            kind,
            event,
            name: "key",
            value: &id,
            ..IconButton::new(icon, label)
        })
    }

    fn new_dialog(&self, draft: &Draft) -> Result<Html> {
        let value = |key: &str| draft.values.get(key).cloned().unwrap_or_default();
        let errors = |key: &str| draft.errors.get(key).cloned().unwrap_or_default();
        let fields = &self.pages.fields;
        let mut body = Html::EMPTY;
        body.push(&render(
            Field::in_form("field", "record_type")
                .id("custom-field-new-record-type")
                .label("Record")
                .kind(InputKind::Select)
                .prompt("Choose…")
                .options(
                    fields
                        .record_types()
                        .map(|record| Choice::new(record.label, record.id))
                        .collect(),
                )
                .value(value("record_type"))
                .errors(errors("record_type"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("field", "label")
                .id("custom-field-new-label")
                .label("Label")
                .value(value("label"))
                .attr("maxlength", &LABEL_MAX.to_string())
                .errors(errors("label"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("field", "key")
                .id("custom-field-new-key")
                .label("Key")
                .value(value("key"))
                .hint("A lower-case letter, then letters, digits and '_'. It never changes.")
                .attr("maxlength", &KEY_MAX.to_string())
                .errors(errors("key"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("field", "kind")
                .id("custom-field-new-kind")
                .label("Type")
                .kind(InputKind::Select)
                .prompt("Choose…")
                .options(
                    Kind::ALL
                        .into_iter()
                        .map(|kind| Choice::new(kind.label(), kind.as_str()))
                        .collect(),
                )
                .value(value("kind"))
                .hint("It never changes. A number is kept exactly as written.")
                .errors(errors("kind"))
                .required(),
        )?);
        body.push(&render(
            Field::in_form("field", "picklist")
                .id("custom-field-new-picklist")
                .label("Pick-list")
                .kind(InputKind::Select)
                .prompt("Choose…")
                .options(
                    fields
                        .choice_lists()
                        .into_iter()
                        .map(|(key, label)| Choice::new(label, key))
                        .collect(),
                )
                .value(value("picklist"))
                .hint("Only for a Pick-list field: the values people choose from.")
                .errors(errors("picklist")),
        )?);
        body.push(&render(
            Field::in_form("field", "help")
                .id("custom-field-new-help")
                .label("Help text")
                .value(value("help"))
                .attr("maxlength", &HELP_MAX.to_string())
                .errors(errors("help")),
        )?);
        let mut actions = render(Button {
            event: "close_new",
            button_type: "button",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            variant: ButtonVariant::Primary,
            button_type: "submit",
            working: "Adding…",
            body: Html::text("Add field"),
            ..Button::default()
        })?);
        let form = render(Form {
            id: "custom-field-new-form",
            event: "create",
            body,
            actions,
            ..Form::default()
        })?;
        render(Modal {
            id: "custom-field-new-dialog",
            title: "New custom field",
            description: "It appears on the record straight away.",
            cancel_event: "close_new",
            flash: Some(flash_map(&self.flash)),
            body: form,
            ..Modal::default()
        })
    }
}

// ------------------------------------------------------------- live adapters

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => {
                    self.state =
                        ListState::parse(values, SPEC.clone()).map_err(|_| Error::Internal)?
                }
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.shown,
                notice: self.notice.clone(),
                ..LiveBody::default()
            })
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "index" => Some("Custom fields"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let html = match request.operation {
                "index" => {
                    Index::new(self.clone(), request.query)?
                        .render(scope)
                        .await?
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "index" => Box::new(Index::new(self.clone(), request.query)?),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}
