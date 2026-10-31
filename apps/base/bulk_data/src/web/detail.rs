//! `/imports/{id}`: one run. While it waits it shows the mapping (which
//! column, or fixed value, fills each field; the source key; what a match
//! does) with Check file and Import; while a job works it shows progress and
//! Cancel; and it lists each row's result for the dry run or the real run,
//! with the file of failed rows. The mapping is kept here as the person edits
//! it, and sent with the contract that starts a pass.
use super::{banner, dismiss, render, status, view, when, Confirmation, Pages, INDEX};
use crate::{
    mapping::FieldErrors, FieldMap, Kind, Mapping, Outcome, Pass, Policy, Run, RunState,
    SavedMapping, TypeInfo, CREATE,
};
use bilibi_agent_api::{page::Call, Error as ContractError};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    choices,
    list_state::{Filter, ListState, PageCounts, FILTERS_FORM},
    page::{LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, Subscription},
    params, ActionLink, Alert, BackLink, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant,
    Card, ColumnType, ColumnsOutcome, Fact, Facts, FilterToolbar, FlexRow, FlexTable, Form, Header,
    Html, InputKind, ListColumns, ListSpec, Pagination, RadioGroup, RowMode, SectionHeading,
    StatusKind, TableEmpty, ToolbarControl,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

const SERVICE: &str = super::PAGES;
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const FIXED: &str = "fixed";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([
            (
                "pass".into(),
                Filter::one_of(&["latest", "preview", "run"], "latest"),
            ),
            (
                "outcome".into(),
                Filter::one_of(&["all", "error", "create", "update", "skip"], "all"),
            ),
        ]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("import row list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let mut row = BuiltinColumn::new("row", "Row");
    row.kind = ColumnType::Integer;
    vec![
        row,
        BuiltinColumn::new("key", "Key"),
        BuiltinColumn::new("outcome", "Result"),
        BuiltinColumn::new("record", "Record"),
        BuiltinColumn::new("message", "Why"),
    ]
}

enum Confirm {
    Start,
    Cancel,
}

pub(super) struct Detail {
    pages: Pages,
    id: i64,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    run: Option<Run>,
    info: Option<TypeInfo>,
    saved: Vec<SavedMapping>,
    parents: BTreeMap<&'static str, Vec<Run>>,
    /// The mapping as the person is editing it.
    draft: Option<Mapping>,
    errors: FieldErrors,
    confirm: Option<Confirm>,
    flash: Option<(StatusKind, String)>,
}

fn dom(name: &str) -> String {
    name.replace('_', "-")
}
fn normal(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .collect::<String>()
        .to_ascii_lowercase()
}

/// A first mapping: each field whose name or label a column carries.
fn guess(info: &TypeInfo, header: &[String]) -> Mapping {
    let fields = info
        .fields
        .iter()
        .filter(|field| {
            field
                .reference
                .is_none_or(|reference| reference.lookup.is_some())
        })
        .filter_map(|field| {
            let column = header.iter().find(|column| {
                let column = normal(column);
                column == normal(&field.name) || column == normal(&field.label)
            })?;
            Some(FieldMap {
                field: field.name.clone(),
                column: Some(column.clone()),
                ..FieldMap::default()
            })
        })
        .collect();
    Mapping {
        fields,
        source_key: None,
        duplicates: Policy::Skip,
    }
}

impl Detail {
    pub(super) fn new(
        pages: Pages,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        let id = path
            .get("id")
            .and_then(|id| id.parse::<i64>().ok())
            .filter(|id| *id > 0)
            .ok_or(Error::NotFound)?;
        Ok(Self {
            pages,
            id,
            state: ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)?,
            columns: ListColumns::mount("import-rows", builtins()).map_err(|_| Error::Internal)?,
            rows_changed: true,
            run: None,
            info: None,
            saved: Vec::new(),
            parents: BTreeMap::new(),
            draft: None,
            errors: FieldErrors::new(),
            confirm: None,
            flash: None,
        })
    }
    pub(super) fn title(&self) -> Option<String> {
        self.run.as_ref().map(|run| run.file_name.clone())
    }
    fn say(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }

    /// The mapping from the form's current values.
    fn read_form(&mut self, value: &Value) {
        let (Some(run), Some(info)) = (&self.run, &self.info) else {
            return;
        };
        let Some(object) = value.as_object() else {
            return;
        };
        let form: HashMap<&str, &str> = object
            .iter()
            .filter_map(|(key, value)| {
                let key = key.strip_prefix("map[")?.strip_suffix(']')?;
                Some((key, value.as_str().filter(|value| value.len() <= 1000)?))
            })
            .collect();
        let column = |choice: &str| {
            choice
                .strip_prefix('c')
                .and_then(|index| index.parse::<usize>().ok())
                .and_then(|index| run.header.get(index).cloned())
        };
        let mut fields = Vec::new();
        for field in &info.fields {
            let choice = form
                .get(format!("f_{}", field.name).as_str())
                .copied()
                .unwrap_or("");
            let entry = if choice == FIXED {
                FieldMap {
                    field: field.name.clone(),
                    value: Some(
                        form.get(format!("v_{}", field.name).as_str())
                            .copied()
                            .unwrap_or("")
                            .to_owned(),
                    ),
                    ..FieldMap::default()
                }
            } else if let Some(column) = column(choice) {
                FieldMap {
                    field: field.name.clone(),
                    column: Some(column),
                    parent_run: form
                        .get(format!("p_{}", field.name).as_str())
                        .and_then(|run| run.parse::<i64>().ok()),
                    ..FieldMap::default()
                }
            } else {
                continue;
            };
            fields.push(entry);
        }
        self.draft = Some(Mapping {
            fields,
            source_key: form.get("source_key").copied().and_then(column),
            duplicates: match form.get("duplicates").copied() {
                Some("update") => Policy::Update,
                Some("create") => Policy::Create,
                _ => Policy::Skip,
            },
        });
    }

    /// Runs one contract as the page's person; true when it went through.
    async fn submit(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &'static str,
        contract: &'static str,
        input: Value,
    ) -> Result<bool> {
        let call = Call {
            scope,
            credential,
            service: SERVICE,
            event,
            contract,
        };
        match self.pages.contracts.submit(&call, input).await {
            Ok(_) => {
                self.errors.clear();
                Ok(true)
            }
            Err(ContractError::Backend(
                error @ (Error::StepUpRequired { .. } | Error::Unauthorized),
            )) => Err(error),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.say(StatusKind::Error, "You do not have permission to do that.");
                Ok(false)
            }
            Err(ContractError::InvalidInput(fields)) => {
                let general: Vec<String> = fields
                    .iter()
                    .filter(|(key, _)| {
                        !key.starts_with("fields.") && *key != "source_key" && *key != "duplicates"
                    })
                    .flat_map(|(key, messages)| {
                        messages.iter().map(move |message| match key.as_str() {
                            "company" | "run_id" => message.clone(),
                            key => format!("{}: {message}", key.replace('_', " ")),
                        })
                    })
                    .collect();
                self.errors = fields;
                self.say(
                    StatusKind::Error,
                    if general.is_empty() {
                        "The mapping needs fixing first; see the fields marked below.".to_owned()
                    } else {
                        general.join(" ")
                    },
                );
                Ok(false)
            }
            Err(ContractError::Backend(Error::Conflict)) => {
                self.say(StatusKind::Error, "This import changed since you opened it. Its current state is shown; try again.");
                Ok(false)
            }
            Err(ContractError::NotFound | ContractError::Backend(Error::NotFound)) => {
                self.say(StatusKind::Error, "This import no longer exists.");
                Ok(false)
            }
            Err(_) => {
                self.say(
                    StatusKind::Error,
                    "Imports are unavailable right now; nothing was changed.",
                );
                Ok(false)
            }
        }
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        let event = get("type").ok_or(Error::Invalid)?;
        match event {
            "map" => {
                self.read_form(value);
                self.rows_changed = false;
            }
            "preview" | "start" => {
                let (Some(run), Some(mapping)) = (&self.run, &self.draft) else {
                    return Ok(());
                };
                let input = json!({"run_id": run.id, "version": run.version, "mapping": mapping});
                let (rows, file) = (run.row_count, run.file_name.clone());
                self.confirm = None;
                let (event, contract) = if event == "preview" {
                    ("preview", "base.bulk_data.import.preview")
                } else {
                    ("start", "base.bulk_data.import.start")
                };
                if self
                    .submit(scope, credential, event, contract, input)
                    .await?
                {
                    self.state.filters.insert("pass".into(), "latest".into());
                    self.state.put_page(None);
                    self.say(
                        StatusKind::Success,
                        if event == "preview" {
                            format!("Checking the {rows} rows of {file}; nothing is written. Each row's result appears below.")
                        } else {
                            format!("Importing the {rows} rows of {file}. You get a notice when it finishes.")
                        },
                    );
                }
            }
            "request_start" if self.run.as_ref().is_some_and(|run| run.state.open()) => {
                self.flash = None;
                self.confirm = Some(Confirm::Start);
            }
            "request_cancel" if self.run.as_ref().is_some_and(|run| run.state.working()) => {
                self.flash = None;
                self.confirm = Some(Confirm::Cancel);
            }
            "request_start" | "request_cancel" => {}
            "dismiss_confirm" => self.confirm = None,
            "cancel" => {
                self.confirm = None;
                let Some(run) = &self.run else { return Ok(()) };
                let input = json!({"run_id": run.id, "version": run.version});
                if self
                    .submit(
                        scope,
                        credential,
                        "cancel",
                        "base.bulk_data.import.cancel",
                        input,
                    )
                    .await?
                {
                    self.say(
                        StatusKind::Success,
                        "Stopped. Rows already written stay, and are listed below.",
                    );
                }
            }
            "apply_mapping" => {
                let chosen = params::form_values(value, "saved")
                    .get("id")
                    .and_then(|id| id.parse::<i64>().ok());
                match self.saved.iter().find(|saved| Some(saved.id) == chosen) {
                    Some(saved) => {
                        let mut mapping = saved.mapping.clone();
                        // Keep the parent imports this run already names.
                        if let Some(draft) = &self.draft {
                            for entry in &mut mapping.fields {
                                entry.parent_run = draft
                                    .fields
                                    .iter()
                                    .find(|old| old.field == entry.field)
                                    .and_then(|old| old.parent_run);
                            }
                        }
                        let name = saved.name.clone();
                        self.draft = Some(mapping);
                        self.errors.clear();
                        self.say(
                            StatusKind::Info,
                            format!(
                                "Applied the saved mapping {name}. Check it before you import."
                            ),
                        );
                    }
                    None => self.say(StatusKind::Info, "Choose a saved mapping first."),
                }
            }
            "save_mapping" => {
                let name = params::form_values(value, "save")
                    .get("name")
                    .map(|name| name.trim().to_owned())
                    .unwrap_or_default();
                let (Some(run), Some(draft)) = (&self.run, &self.draft) else {
                    return Ok(());
                };
                if name.is_empty() {
                    self.errors
                        .insert("name".into(), vec!["Name the mapping to save it.".into()]);
                    return Ok(());
                }
                let mut mapping = draft.clone();
                for entry in &mut mapping.fields {
                    entry.parent_run = None;
                }
                let input =
                    json!({"record_type": run.record_type, "name": name, "mapping": mapping});
                if self
                    .submit(
                        scope,
                        credential,
                        "save_mapping",
                        "base.bulk_data.mapping.save",
                        input,
                    )
                    .await?
                {
                    self.say(
                        StatusKind::Success,
                        format!(
                            "Saved the mapping {name}. It is offered for files with these columns."
                        ),
                    );
                }
            }
            FILTERS_FORM => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => dismiss(&mut self.flash, get("key")),
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    pub(super) async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let object = pages.db.get().await?;
        let client: &tokio_postgres::Client = &object;
        let run = pages.bulk.run(client, scope, self.id).await?;
        let info = match pages
            .bulk
            .record_type(client, scope, &run.record_type)
            .await
        {
            Ok(info) => Some(info),
            Err(Error::Forbidden | Error::NotFound) => None,
            Err(error) => return Err(error),
        };
        let may_create = effective_capabilities(client, scope)
            .await?
            .contains(CREATE);
        if info.is_some() && run.state.open() {
            self.saved = pages
                .bulk
                .mappings(client, scope, &run.record_type, Some(&run.header))
                .await?;
            self.parents = pages.bulk.parents(client, scope, &run.record_type).await?;
        }
        let display = pages.display(client, scope).await?;
        let pass = match self.state.filters["pass"].as_str() {
            "preview" => Pass::Preview,
            "run" => Pass::Run,
            _ if run.pass == Some(Pass::Run) || run.run.done() > 0 => Pass::Run,
            _ => Pass::Preview,
        };
        let outcome = Outcome::parse(&self.state.filters["outcome"]);
        let (results, total) = pages
            .bulk
            .rows(
                client,
                scope,
                run.id,
                pass,
                outcome,
                self.state.offset() as i64,
                self.state.page_size as i64,
            )
            .await?;
        drop(object);
        if self.draft.is_none() {
            self.draft = run
                .mapping
                .clone()
                .or_else(|| info.as_ref().map(|info| guess(info, &run.header)));
        }
        let label = pages.bulk.label(&run.record_type).to_owned();
        let mut parts = Vec::new();
        parts.push(render(Header {
            title: Html::text(&run.file_name),
            subtitle: "",
            title_actions: status(&run)?,
            actions: render(BackLink {
                href: INDEX,
                id: "import-back",
                title: "Back to imports",
                class: "",
            })?,
        })?);
        parts.push(self.facts(&run, &label, &display)?);
        if run.state.open() && may_create {
            match &info {
                Some(info) => parts.push(self.mapping_section(&run, info)?),
                None => parts.push(render(Alert {
                    kind: StatusKind::Info,
                    id: "import-no-access",
                    class: "",
                    body: Html::text(&format!(
                        "You can no longer import {}, so this file cannot be checked or imported.",
                        label.to_lowercase()
                    )),
                })?),
            }
        }
        if run.state.working() || run.stopped {
            parts.push(self.working(&run, may_create)?);
        }
        let counts = if pass == Pass::Run {
            run.run
        } else {
            run.preview
        };
        if counts.done() > 0 || run.state.working() {
            parts.push(self.results(&run, info.as_ref(), pass, &results, total, counts)?);
        }
        if self.confirm.is_some() {
            let banner = banner(&pages, "import-confirm-instance").await?;
            if let Some(dialog) = self.dialog(&run, &label, banner)? {
                parts.push(dialog);
            }
        }
        self.run = Some(run);
        self.info = info;
        view("import-detail", &self.flash, parts)
    }

    fn facts(
        &self,
        run: &Run,
        label: &str,
        display: &bilibi_datetime::DisplayContext,
    ) -> Result<Html> {
        let progress = if run.state.working() {
            format!("{} of {} rows", run.progress(), run.row_count)
        } else {
            run.row_count.to_string()
        };
        let mut items = vec![
            Fact {
                title: "Records",
                id: "import-records",
                value: Html::text(label),
            },
            Fact {
                title: if run.state.working() {
                    "Progress"
                } else {
                    "Rows"
                },
                id: "import-rows-count",
                value: Html::text(&progress),
            },
            Fact {
                title: "Columns",
                id: "import-columns",
                value: Html::text(&run.header.join(", ")),
            },
            Fact {
                title: "Uploaded",
                id: "import-uploaded",
                value: when(display, "import-uploaded-at", run.created_at)?,
            },
        ];
        if let Some(finished) = run.finished_at {
            items.push(Fact {
                title: "Finished",
                id: "import-finished",
                value: when(display, "import-finished-at", finished)?,
            });
        }
        if let (Some(trace), Some(Pass::Run)) = (&run.trace_id, run.pass) {
            items.push(Fact {
                title: "Trace ID",
                id: "import-trace",
                value: Html::text(trace),
            });
        }
        render(Card {
            id: "import-facts",
            body: render(Facts {
                id: "import-facts-list",
                items,
            })?,
            ..Card::default()
        })
    }

    fn mapping_section(&self, run: &Run, info: &TypeInfo) -> Result<Html> {
        let draft = self.draft.clone().unwrap_or_default();
        let column_choices = |extra: &[(String, String)]| {
            let mut options = extra.to_vec();
            options.extend(
                run.header
                    .iter()
                    .enumerate()
                    .map(|(index, name)| (name.clone(), format!("c{index}"))),
            );
            choices(&options)
        };
        let errors = |key: &str| self.errors.get(key).cloned().unwrap_or_default();
        let mut controls: Vec<(String, String, Html)> = Vec::new();
        for field in &info.fields {
            let entry = draft.fields.iter().find(|entry| entry.field == field.name);
            let choice = match entry {
                Some(FieldMap { value: Some(_), .. }) => FIXED.to_owned(),
                Some(FieldMap {
                    column: Some(column),
                    ..
                }) => run
                    .header
                    .iter()
                    .position(|name| name == column)
                    .map_or(String::new(), |index| format!("c{index}")),
                _ => String::new(),
            };
            let id = format!("import-map-{}", dom(&field.name));
            let mut html = render(
                bilibi_ui::Field::in_form("map", &format!("f_{}", field.name))
                    .id(id.clone())
                    .kind(InputKind::Select)
                    .label("Source")
                    .options(column_choices(&[
                        ("Not imported".into(), String::new()),
                        ("Fixed value".into(), FIXED.into()),
                    ]))
                    .value(choice.clone())
                    .errors(errors(&format!("fields.{}", field.name))),
            )?;
            if choice == FIXED {
                let value = entry
                    .and_then(|entry| entry.value.clone())
                    .unwrap_or_default();
                let mut input = bilibi_ui::Field::in_form("map", &format!("v_{}", field.name))
                    .id(format!("{id}-value"))
                    .label("Value")
                    .value(value);
                input = if !field.choices.is_empty() {
                    let options: Vec<(String, String)> = field
                        .choices
                        .iter()
                        .map(|choice| (choice.clone(), choice.clone()))
                        .collect();
                    input
                        .kind(InputKind::Select)
                        .prompt("Choose")
                        .options(choices(&options))
                } else if field.kind == Kind::Boolean {
                    input
                        .kind(InputKind::Select)
                        .prompt("Choose")
                        .options(choices(&[("Yes", "yes"), ("No", "no")]))
                } else if field.kind == Kind::Date {
                    input.kind(InputKind::Date)
                } else {
                    input
                };
                html.push(&render(input)?);
            } else if let (Some(reference), false) = (field.reference, choice.is_empty()) {
                if reference.parent.is_some() {
                    let mut options: Vec<(String, String)> = Vec::new();
                    if let Some(lookup) = &reference.lookup {
                        options.push((
                            format!(
                                "Look up the {} by {}",
                                lookup.noun,
                                lookup.by.replace('_', " ")
                            ),
                            String::new(),
                        ));
                    }
                    for parent in self.parents.get(field.name.as_str()).into_iter().flatten() {
                        options.push((
                            format!("Key of import {}: {}", parent.id, parent.file_name),
                            parent.id.to_string(),
                        ));
                    }
                    let mut select = bilibi_ui::Field::in_form("map", &format!("p_{}", field.name))
                        .id(format!("{id}-parent"))
                        .kind(InputKind::Select)
                        .label("Find it by")
                        .options(choices(&options))
                        .value(
                            entry
                                .and_then(|entry| entry.parent_run)
                                .map(|run| run.to_string())
                                .unwrap_or_default(),
                        );
                    if reference.lookup.is_none() {
                        select = select.prompt("Choose the parent import");
                    }
                    html.push(&render(select)?);
                }
            }
            let title = if field.required {
                format!("{} (required)", field.label)
            } else {
                field.label.clone()
            };
            controls.push((title, format!("{id}-fact"), html));
        }
        let source = run
            .header
            .iter()
            .position(|name| Some(name) == draft.source_key.as_ref())
            .map_or(String::new(), |index| format!("c{index}"));
        controls.push((
            "Source key".into(),
            "import-map-source-key-fact".into(),
            render(
                bilibi_ui::Field::in_form("map", "source_key")
                    .id("import-map-source-key")
                    .kind(InputKind::Select)
                    .label("Column")
                    .options(column_choices(&[("None".into(), String::new())]))
                    .value(source)
                    .hint("The column a later import of child records uses to name these rows.")
                    .errors(errors("source_key")),
            )?,
        ));
        if !info.matches.is_empty() {
            controls.push((
                "Existing records".into(),
                "import-map-duplicates-fact".into(),
                render(
                    RadioGroup::in_form(
                        "map",
                        "duplicates",
                        choices(&[
                            ("Leave the existing record alone", "skip"),
                            ("Update the existing record", "update"),
                            ("Create a new record anyway", "create"),
                        ]),
                    )
                    .id("import-map-duplicates")
                    .label("When a row matches one existing record")
                    .value(draft.duplicates.as_str())
                    .hint(format!(
                        "Rows are matched on {}. A row matching more than one is not imported.",
                        info.matches.join(" or ").replace('_', " ")
                    )),
                )?,
            ));
        }
        let items = controls
            .iter()
            .map(|(title, id, value)| Fact {
                title,
                id,
                value: value.clone(),
            })
            .collect();
        let mut body = render(Form {
            id: "import-map-form",
            event: "map",
            change: true,
            class: "",
            body: render(Facts {
                id: "import-map-fields",
                items,
            })?,
            actions: Html::EMPTY,
        })?;
        let mut actions = render(Button {
            id: "import-check",
            button_type: "button",
            event: "preview",
            working: "Starting…",
            body: Html::text("Check file"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            variant: ButtonVariant::Primary,
            id: "import-start",
            button_type: "button",
            event: "request_start",
            body: Html::text(&format!("Import {} rows", run.row_count)),
            ..Button::default()
        })?);
        body.push(&actions);
        let mut section = render(SectionHeading {
            title: "Columns",
            id: "import-mapping-heading",
            description: "Choose the column, or a fixed value, that fills each field. Check the file to see what every row would do; nothing is written until you import.",
            ..SectionHeading::default()
        })?;
        section.push(&render(Card {
            id: "import-mapping",
            body,
            ..Card::default()
        })?);
        section.push(&self.saved_mappings()?);
        Ok(section)
    }

    fn saved_mappings(&self) -> Result<Html> {
        let mut body = Html::EMPTY;
        if !self.saved.is_empty() {
            let options: Vec<(String, String)> = self
                .saved
                .iter()
                .map(|saved| (saved.name.clone(), saved.id.to_string()))
                .collect();
            body.push(&render(Form {
                id: "import-apply-form",
                event: "apply_mapping",
                change: false,
                class: "",
                body: render(
                    bilibi_ui::Field::in_form("saved", "id")
                        .id("import-saved-mapping")
                        .kind(InputKind::Select)
                        .label("Saved mapping")
                        .prompt("Choose a mapping saved for these columns")
                        .options(choices(&options)),
                )?,
                actions: render(Button {
                    id: "import-apply",
                    button_type: "submit",
                    working: "Applying…",
                    body: Html::text("Apply"),
                    ..Button::default()
                })?,
            })?);
        }
        body.push(&render(Form {
            id: "import-save-form",
            event: "save_mapping",
            change: false,
            class: "",
            body: render(
                bilibi_ui::Field::in_form("save", "name")
                    .id("import-save-name")
                    .label("Save this mapping as")
                    .hint("Shared with your company for files with these columns. Saving a name again replaces it.")
                    .errors(self.errors.get("name").cloned().unwrap_or_default()),
            )?,
            actions: render(Button { id: "import-save", button_type: "submit", working: "Saving…", body: Html::text("Save mapping"), ..Button::default() })?,
        })?);
        render(Card {
            id: "import-saved",
            title: "Saved mappings",
            body,
            ..Card::default()
        })
    }

    fn working(&self, run: &Run, may_create: bool) -> Result<Html> {
        let what = if run.state == RunState::Running {
            "import"
        } else {
            "check"
        };
        let text = if run.stopped {
            format!("The {what} stopped before it finished, after {} of {} rows: your access changed, or it failed every attempt. Cancel it to map and start again.", run.progress(), run.row_count)
        } else {
            format!(
                "{} of {} rows done. This page follows along; you can leave it.",
                run.progress(),
                run.row_count
            )
        };
        let mut body = render(Alert {
            kind: if run.stopped {
                StatusKind::Warning
            } else {
                StatusKind::Info
            },
            id: "import-progress",
            class: "",
            body: Html::text(&text),
        })?;
        if may_create {
            body.push(&render(Button {
                id: "import-cancel",
                button_type: "button",
                event: "request_cancel",
                body: Html::text(if run.state == RunState::Running {
                    "Cancel import"
                } else {
                    "Cancel check"
                }),
                ..Button::default()
            })?);
        }
        render(Card {
            id: "import-working",
            body,
            ..Card::default()
        })
    }

    fn results(
        &mut self,
        run: &Run,
        info: Option<&TypeInfo>,
        pass: Pass,
        results: &[crate::RowResult],
        total: i64,
        counts: crate::Counts,
    ) -> Result<Html> {
        if self.rows_changed {
            self.columns
                .load(results, |row: &crate::RowResult| row.row.to_string());
        }
        let preview = pass == Pass::Preview;
        let download = if counts.error > 0 {
            let href = format!(
                "/imports/{}/failed-rows{}",
                run.id,
                if preview { "?pass=preview" } else { "" }
            );
            render(ActionLink {
                id: "import-failed-rows",
                icon: "arrow-down-tray",
                title: "Download the rows that were not imported, with the reason, to fix and upload again",
                href: &href,
                method: "",
                body: Html::text("Download failed rows"),
            })?
        } else {
            Html::EMPTY
        };
        let mut html = render(SectionHeading {
            title: if preview {
                "Check results"
            } else {
                "Import results"
            },
            id: "import-results-heading",
            description: if preview {
                "What each row would do. Nothing was written."
            } else {
                "What each row did."
            },
            actions: download,
            ..SectionHeading::default()
        })?;
        let would = |done: &str, will: &str| {
            if preview {
                will.to_owned()
            } else {
                done.to_owned()
            }
        };
        let (created, updated, skipped, failed) = (
            would("Created", "To create"),
            would("Updated", "To update"),
            would("Skipped", "To skip"),
            would("Not imported", "With errors"),
        );
        let count = |n: i64| Html::text(&n.to_string());
        html.push(&render(Card {
            id: "import-counts",
            body: render(Facts {
                id: "import-counts-list",
                items: vec![
                    Fact {
                        title: &created,
                        id: "import-count-create",
                        value: count(counts.create),
                    },
                    Fact {
                        title: &updated,
                        id: "import-count-update",
                        value: count(counts.update),
                    },
                    Fact {
                        title: &skipped,
                        id: "import-count-skip",
                        value: count(counts.skip),
                    },
                    Fact {
                        title: &failed,
                        id: "import-count-error",
                        value: count(counts.error),
                    },
                ],
            })?,
            ..Card::default()
        })?);
        let form = self.state.filters_form();
        html.push(&render(FilterToolbar::new(
            "import-results-filters",
            vec![
                ToolbarControl::select(
                    "import-results-pass",
                    "pass",
                    "Which pass",
                    &form["pass"],
                    choices(&[
                        ("Latest", "latest"),
                        ("Check", "preview"),
                        ("Import", "run"),
                    ]),
                ),
                ToolbarControl::select(
                    "import-results-outcome",
                    "outcome",
                    "Filter by result",
                    &form["outcome"],
                    choices(&[
                        ("All rows", "all"),
                        ("Errors", "error"),
                        ("Create", "create"),
                        ("Update", "update"),
                        ("Skip", "skip"),
                    ]),
                ),
            ],
        ))?);
        let compact = self.columns.mode() == RowMode::Compact;
        let path = info.and_then(|info| info.path);
        let mut rows = Vec::with_capacity(results.len());
        for result in results {
            let key = result.row.to_string();
            let badge = match (result.outcome, preview) {
                (Outcome::Create, true) => Badge::status(BadgeKind::Plain, "Would create"),
                (Outcome::Update, true) => Badge::status(BadgeKind::Plain, "Would update"),
                (Outcome::Skip, true) => Badge::status(BadgeKind::Neutral, "Would skip"),
                (Outcome::Create, false) => Badge::status(BadgeKind::Success, "Created"),
                (Outcome::Update, false) => Badge::status(BadgeKind::Success, "Updated"),
                (Outcome::Skip, false) => Badge::status(BadgeKind::Neutral, "Skipped"),
                (Outcome::Error, true) => Badge::status(BadgeKind::Danger, "Error"),
                (Outcome::Error, false) => Badge::status(BadgeKind::Danger, "Not imported"),
            };
            let record = match (&result.record_id, path) {
                (Some(id), Some(path)) => render(ActionLink {
                    id: &format!("import-row-record-{key}"),
                    icon: "",
                    title: "Open the record",
                    href: &path.replace("{id}", id),
                    method: "",
                    body: Html::text(id),
                })?,
                (Some(id), None) => Html::text(id),
                (None, _) => Html::text("—"),
            };
            let message = result.message.clone().unwrap_or_default();
            let message = if compact && message.chars().count() > 80 {
                format!("{}…", message.chars().take(79).collect::<String>())
            } else {
                message
            };
            let mut row = FlexRow::new(key.clone())
                .slot("row", Html::text(&key))
                .slot(
                    "key",
                    Html::text(result.source_key.as_deref().unwrap_or("—")),
                )
                .slot("outcome", render(badge)?)
                .slot("record", record)
                .slot("message", Html::text(&message));
            row.dom_id = Some(format!("import-row-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("import-rows", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Row results";
        table.framed = false;
        if total == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if run.state.working() {
                "No rows done yet."
            } else {
                "No rows with this result."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "import-rows-pagination",
            PageCounts::new(self.state.page, self.state.page_size, total as u64),
            &PAGE_SIZES,
        ))?);
        html.push(&render(Card {
            id: "import-results",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?);
        Ok(html)
    }

    fn dialog(&self, run: &Run, label: &str, banner: Html) -> Result<Option<Html>> {
        let Some(confirm) = &self.confirm else {
            return Ok(None);
        };
        Ok(Some(match confirm {
            Confirm::Start => {
                let consequence = format!(
                    "Import {} rows of {} as {}?",
                    run.row_count,
                    run.file_name,
                    label.to_lowercase()
                );
                render(Confirmation {
                    id: "import-start-confirm",
                    consequence: &consequence,
                    detail: "Each row is written through the same rules as a single edit, as you, in the background. Rows that fail are listed and can be downloaded to fix. Written rows are not undone by cancelling.",
                    confirm: "Import",
                    working: "Starting…",
                    confirm_event: "start",
                    cancel_event: "dismiss_confirm",
                    banner,
                })?
            }
            Confirm::Cancel => render(Confirmation {
                id: "import-cancel-confirm",
                consequence: "Stop this run?",
                detail: "Rows already written stay and are listed. The rest are left out.",
                confirm: "Stop",
                working: "Stopping…",
                confirm_event: "cancel",
                cancel_event: "dismiss_confirm",
                banner,
            })?,
        }))
    }
}

impl LiveState<Scope, Error> for Detail {
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
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![Subscription {
            receiver: self.pages.bulk.subscribe(),
        }]
    }
}
