//! Personal data pages (`base/privacy.pages`): data-subject requests
//! (`/privacy/requests`, one request at `/privacy/requests/{id}`) and legal
//! holds (`/privacy/holds`), retention (`/privacy/retention`), plus the request document download
//! (`/privacy/requests/{id}/document`). No Bilimbi counterpart; the lists
//! follow the Events page (`apps/base/events/src/web.rs`).
//!
//! Every write is the module's agent contract (ADR 01), called through the
//! page's declared `PAGE_WRITES`: the page holds what the person asked for and
//! acts on that held request, never on a client value. A refusal is a flash;
//! a Step-up refusal goes to the host, which asks for the code and comes back.
use crate::{
    ClassView, Hold, HoldTarget, Privacy, RequestKind, RequestView, RetentionAction, HOLD_MANAGE,
    REQUEST_APPROVE, REQUEST_RECEIVE, RETENTION_MANAGE,
};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::{PartyProofs, Scope};
use bilibi_ui::{
    download::{Download, DownloadFuture, DownloadRequest, FileBody},
    forms::{Choice, Field, Form, InputKind},
    list_state::FILTERS_FORM,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome,
    ConfirmDialog, CreateButton, Fact, Facts, FilterToolbar, FlashMap, FlexRow, FlexTable, Header,
    Html, IconButton, IconKind, ListColumns, ListSpec, ListState, Modal, Pagination, RecordLink,
    SectionHeading, StatusKind, TableEmpty, ToolbarControl, NO_FLASH,
};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

pub const REQUESTS_PATH: &str = "/privacy/requests";
pub const HOLDS_PATH: &str = "/privacy/holds";
pub const RETENTION_PATH: &str = "/privacy/retention";
pub(crate) const SERVICE: &str = "base/privacy.pages";
pub(crate) const DOWNLOADS: &str = "base/privacy.downloads";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const REQUEST_FORM: &str = "request";
const HOLD_FORM: &str = "hold";
const NOTE_FORM: &str = "note";
const PERIOD_FORM: &str = "period";

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("privacy list spec")
});

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub privacy: Arc<Privacy>,
    pub contracts: Arc<PageContracts>,
    pub parties: Arc<PartyProofs>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
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
fn clear_flash(flash: &mut Option<(StatusKind, String)>, value: &Value) {
    let key = value.get("key").and_then(Value::as_str);
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| Some(kind.name()) == key)
    {
        *flash = None;
    }
}
async fn display(
    pages: &Pages,
    client: &tokio_postgres::Client,
    scope: &Scope,
) -> Result<DisplayContext> {
    let locale = pages.locales.resolve(client, scope, None).await?.locale;
    pages.display.display(client, scope, None, locale).await
}
/// An RFC 3339 instant in the reader's display settings, or a dash.
fn when(display: &DisplayContext, at: Option<&str>) -> String {
    at.and_then(|at| at.parse::<jiff::Timestamp>().ok())
        .map(|at| display.render(at, Format::DateTime, Precision::Minute).text)
        .unwrap_or_else(|| "—".into())
}
fn status_badge(request: &RequestView) -> Result<Html> {
    let kind = match request.status.as_str() {
        "completed" => BadgeKind::Success,
        "refused" => BadgeKind::Neutral,
        _ if request.late => BadgeKind::Danger,
        "running" => BadgeKind::Warning,
        _ => BadgeKind::Neutral,
    };
    let label = match request.status.as_str() {
        "received" => "Received",
        "verified" => "Identity verified",
        "approved" => "Approved",
        "running" if request.late => "Running · late",
        "running" => "Running",
        "completed" => "Completed",
        "refused" => "Refused",
        _ => "Unknown",
    };
    let label = if request.late
        && !matches!(request.status.as_str(), "running" | "completed" | "refused")
    {
        format!("{label} · late")
    } else {
        label.to_owned()
    };
    render(Badge::status(kind, &label))
}
/// Field errors from a refused contract, keyed by the form's field names.
fn field_errors(error: &ContractError) -> BTreeMap<String, Vec<String>> {
    match error {
        ContractError::InvalidInput(fields) => fields.clone().into_iter().collect(),
        _ => BTreeMap::new(),
    }
}
/// What a refused contract tells the person; Step-up and sign-in go to the host.
fn refusal(error: ContractError, denied: &str, failed: &str) -> Result<(StatusKind, String)> {
    match error {
        ContractError::Backend(error @ (Error::StepUpRequired { .. } | Error::Unauthorized)) => {
            Err(error)
        }
        ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => {
            Ok((StatusKind::Error, denied.into()))
        }
        ContractError::Backend(Error::Conflict) => Ok((
            StatusKind::Error,
            "The record changed while you were looking at it. It now shows its latest state."
                .into(),
        )),
        ContractError::InvalidInput(_)
        | ContractError::Backend(Error::Invalid | Error::NotFound) => Ok((
            StatusKind::Error,
            "Check the highlighted fields and try again.".into(),
        )),
        _ => Ok((StatusKind::Error, failed.into())),
    }
}
fn call<'a>(
    scope: &'a Scope,
    credential: &'a str,
    event: &'static str,
    contract: &'static str,
) -> Call<'a> {
    Call {
        scope,
        credential,
        service: SERVICE,
        event,
        contract,
    }
}

#[derive(Template)]
#[template(path = "privacy_page.html")]
struct PageView<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    header: Html,
    body: Html,
    dialog: Html,
}

#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums text-ink">{{ text }}</span>"#,
    ext = "html"
)]
struct Mono<'a> {
    text: &'a str,
}

// ---------------------------------------------------------------- requests

fn request_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("id", "Request"),
        BuiltinColumn::new("kind", "Kind"),
        BuiltinColumn::new("person", "Person"),
        BuiltinColumn::new("status", "Status"),
        BuiltinColumn::new("received", "Received"),
        BuiltinColumn::new("due", "Due"),
    ]
}

/// The New request dialog's values and refusals.
#[derive(Default)]
struct RequestDialog {
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
    companies: Vec<Choice>,
}

struct Requests {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    rows: Vec<RequestView>,
    dialog: Option<RequestDialog>,
    flash: Option<(StatusKind, String)>,
    redirect: Option<(String, String)>,
}
impl Requests {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: parse(query)?,
            columns: ListColumns::mount("privacy-requests", request_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            dialog: None,
            flash: None,
            redirect: None,
        })
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        match value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?
        {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self
                .state
                .put_page(value.get("page").and_then(Value::as_str)),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, value),
            "new_request" => {
                let client = self.pages.db.get().await?;
                let companies = self
                    .pages
                    .parties
                    .selectable_organisations(&**client, scope, REQUEST_RECEIVE)
                    .await?
                    .into_iter()
                    .filter(|company| !company.archived)
                    .map(|company| Choice::new(company.name, company.id.get().to_string()))
                    .collect();
                self.flash = None;
                self.dialog = Some(RequestDialog {
                    companies,
                    ..RequestDialog::default()
                });
            }
            "close_modal" => self.dialog = None,
            "create" => {
                let Some(dialog) = self.dialog.as_mut() else {
                    return Ok(());
                };
                let values = params::form_values(value, REQUEST_FORM);
                let list = |key: &str| -> Vec<String> {
                    values.get(key).map_or_else(Vec::new, |text| {
                        text.split([',', ';', '\n'])
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(String::from)
                            .collect()
                    })
                };
                let number = |key: &str| {
                    values
                        .get(key)
                        .and_then(|v| v.trim().parse::<i64>().ok())
                        .unwrap_or(0)
                };
                let input = json!({
                    "kind": values.get("kind").cloned().unwrap_or_default(),
                    "person_id": number("person_id"), "company_id": number("company_id"),
                    "emails": list("emails"), "phones": list("phones"),
                });
                dialog.values = values;
                match self
                    .pages
                    .contracts
                    .submit(
                        &call(scope, credential, "create", "base.privacy.request.receive"),
                        input,
                    )
                    .await
                {
                    Ok(result) => {
                        let id = result["result"]["id"].as_i64().unwrap_or_default();
                        self.dialog = None;
                        self.redirect = Some((
                            format!("{REQUESTS_PATH}/{id}"),
                            format!("Request {id} recorded. Verify the requester's identity next."),
                        ));
                    }
                    Err(error) => {
                        dialog.errors = field_errors(&error);
                        self.flash = Some(refusal(
                            error,
                            "You do not have permission to record requests.",
                            "The request could not be recorded.",
                        )?);
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let display = display(&pages, &client, scope).await?;
        let can_receive = effective_capabilities(&**client, scope)
            .await?
            .contains(REQUEST_RECEIVE);
        let mut all = pages.privacy.requests(&**client, scope, false, 500).await?;
        let search = self.state.search.trim().to_lowercase();
        if !search.is_empty() {
            all.retain(|r| {
                r.id.to_string() == search
                    || r.person.get().to_string() == search
                    || r.kind.as_str().contains(&search)
                    || r.status.contains(&search)
            });
        }
        let page = self.state.paginate(&all);
        let counts = page.counts;
        self.rows = page.entries.to_vec();
        if self.rows_changed {
            self.columns.load(&self.rows, |row| row.id.to_string());
        }
        let actions = if can_receive {
            render(CreateButton {
                noun: "Request",
                id: "privacy-new-request",
                event: "new_request",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Data-subject requests"),
            subtitle: "Access, portability and erasure requests, answered in every store.",
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let mut body = render(FilterToolbar::new(
            "privacy-requests-filters",
            vec![ToolbarControl::search(
                "privacy-requests-search",
                "search",
                "Search requests",
                &form["search"],
                "Search by request, person, kind or status...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(self.rows.len());
        for request in &self.rows {
            let key = request.id.to_string();
            let href = format!("{REQUESTS_PATH}/{key}");
            let link = render(RecordLink {
                workspace: false,
                kind: "privacy.request",
                record_id: &key,
                href: &href,
                class: "",
                body: Html::text(&format!("#{key}")),
            })?;
            let mut row = FlexRow::new(key.clone())
                .slot("id", link)
                .slot("kind", Html::text(request.kind.label()))
                .slot(
                    "person",
                    render(Mono {
                        text: &request.person.get().to_string(),
                    })?,
                )
                .slot("status", status_badge(request)?)
                .slot(
                    "received",
                    Html::text(&when(&display, Some(&request.received_at))),
                )
                .slot("due", Html::text(&when(&display, Some(&request.due_at))));
            row.dom_id = Some(format!("privacy-request-{key}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("privacy-requests", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Data-subject requests";
        table.framed = false;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No requests yet."
            } else {
                "No requests match this search."
            })));
        }
        let mut list = render(table)?;
        list.push(&render(Pagination::new(
            "privacy-requests-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        body.push(&render(Card {
            id: "privacy-requests-card",
            inner_class: "p-0",
            body: list,
            ..Card::default()
        })?);
        let dialog = match &self.dialog {
            Some(dialog) => {
                let field = |key: &str, label: &str| {
                    Field::in_form(REQUEST_FORM, key)
                        .id(format!("privacy-request-{}", key.replace('_', "-")))
                        .label(label)
                        .value(dialog.values.get(key).cloned().unwrap_or_default())
                        .errors(dialog.errors.get(key).cloned().unwrap_or_default())
                };
                let mut fields = render(
                    field("kind", "Kind")
                        .kind(InputKind::Select)
                        .prompt("Choose what the person asks for")
                        .options(vec![
                            Choice::new("Access (a copy of their data)", "access"),
                            Choice::new(
                                "Portability (their data, machine-readable)",
                                "portability",
                            ),
                            Choice::new("Erasure", "erasure"),
                        ])
                        .required(),
                )?;
                fields.push(&render(
                    field("person_id", "Person ID")
                        .kind(InputKind::Number)
                        .hint("The person's ID from Contacts.")
                        .required(),
                )?);
                fields.push(&render(
                    field("company_id", "Filed under company")
                        .kind(InputKind::Select)
                        .prompt("Choose a company")
                        .options(dialog.companies.clone())
                        .hint("The export or certificate is kept under this company.")
                        .required(),
                )?);
                fields.push(&render(
                    field("emails", "Emails the person gave").hint("Separate several with commas."),
                )?);
                fields.push(&render(
                    field("phones", "Phone numbers the person gave")
                        .hint("Separate several with commas."),
                )?);
                let mut actions = render(Button {
                    button_type: "button",
                    event: "close_modal",
                    body: Html::text("Cancel"),
                    ..Button::default()
                })?;
                actions.push(&render(Button {
                    button_type: "submit",
                    variant: ButtonVariant::Primary,
                    working: "Recording…",
                    body: Html::text("Record request"),
                    ..Button::default()
                })?);
                let form = render(Form {
                    id: "privacy-request-form",
                    event: "create",
                    body: fields,
                    actions,
                    ..Form::default()
                })?;
                render(Modal {
                    id: "privacy-request-modal",
                    title: "New request",
                    cancel_event: "close_modal",
                    description: "Record what a person asked for. Their identity is verified next.",
                    flash: Some(flash_map(&self.flash)),
                    body: form,
                    ..Modal::default()
                })?
            }
            None => Html::EMPTY,
        };
        PageView {
            id: "privacy-requests",
            flash: if self.dialog.is_some() {
                NO_FLASH
            } else {
                flash_map(&self.flash)
            },
            header,
            body,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

// ----------------------------------------------------------------- request

fn store_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("store", "Store"),
        BuiltinColumn::new("found", "Linked to the person"),
        BuiltinColumn::new("matched", "Matched by email or phone"),
        BuiltinColumn::new("state", "Answer"),
        BuiltinColumn::new("result", "Result"),
    ]
}
/// A store row: what it holds (before running) and its answer (after).
#[derive(Clone)]
struct StoreRow {
    store: String,
    label: String,
    found: Option<i64>,
    matched: Option<i64>,
    state: String,
    result: String,
    attempts: i32,
}
enum Dialog {
    Verify {
        values: HashMap<String, String>,
        errors: BTreeMap<String, Vec<String>>,
    },
    Refuse {
        values: HashMap<String, String>,
        errors: BTreeMap<String, Vec<String>>,
    },
    Approve,
}
struct RequestPage {
    pages: Pages,
    id: i64,
    columns: ListColumns,
    rows_changed: bool,
    request: Option<RequestView>,
    rows: Vec<StoreRow>,
    dialog: Option<Dialog>,
    flash: Option<(StatusKind, String)>,
}
fn summary(result: &Option<Value>) -> String {
    let Some(result) = result else {
        return "—".into();
    };
    let mut parts = Vec::new();
    if let Some(actions) = result.get("actions").and_then(Value::as_array) {
        for action in actions {
            parts.push(format!(
                "{} {} {}",
                action.get("count").and_then(Value::as_i64).unwrap_or(0),
                action.get("kind").and_then(Value::as_str).unwrap_or(""),
                action.get("action").and_then(Value::as_str).unwrap_or("")
            ));
        }
    }
    if let Some(kept) = result
        .get("kept")
        .and_then(Value::as_array)
        .filter(|k| !k.is_empty())
    {
        parts.push(format!("{} kept", kept.len()));
    }
    if let Some(files) = result
        .get("files")
        .and_then(Value::as_i64)
        .filter(|f| *f > 0)
    {
        parts.push(format!("{files} files listed"));
    }
    if let Some(deferral) = result.get("deferral").filter(|d| !d.is_null()) {
        parts.push(format!(
            "deferred until {}",
            deferral
                .get("complete_by")
                .and_then(Value::as_str)
                .unwrap_or("?")
        ));
    }
    if parts.is_empty() {
        "Nothing held".into()
    } else {
        parts.join(" · ")
    }
}
impl RequestPage {
    fn new(pages: Pages, id: i64) -> Result<Self> {
        Ok(Self {
            pages,
            id,
            columns: ListColumns::mount("privacy-request-stores", store_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            request: None,
            rows: Vec::new(),
            dialog: None,
            flash: None,
        })
    }
    fn version(&self) -> i64 {
        self.request.as_ref().map_or(0, |r| r.version)
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let id = self.id;
        match value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?
        {
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, value),
            "open_verify" => {
                self.dialog = Some(Dialog::Verify {
                    values: HashMap::new(),
                    errors: BTreeMap::new(),
                })
            }
            "open_refuse" => {
                self.dialog = Some(Dialog::Refuse {
                    values: HashMap::new(),
                    errors: BTreeMap::new(),
                })
            }
            "request_approve" => self.dialog = Some(Dialog::Approve),
            "close_modal" | "cancel_approve" => self.dialog = None,
            "verify" | "refuse" => {
                let verify = value["type"] == "verify";
                let values = params::form_values(value, NOTE_FORM);
                let note = values.get("note").cloned().unwrap_or_default();
                let (event, contract, input) = if verify {
                    (
                        "verify",
                        "base.privacy.request.verify",
                        json!({"id": id, "version": self.version(), "verification": note}),
                    )
                } else {
                    (
                        "refuse",
                        "base.privacy.request.refuse",
                        json!({"id": id, "version": self.version(), "reason": note}),
                    )
                };
                match self
                    .pages
                    .contracts
                    .submit(&call(scope, credential, event, contract), input)
                    .await
                {
                    Ok(_) => {
                        self.dialog = None;
                        self.flash = Some((StatusKind::Success, if verify { "Identity verified. A person with approval rights can now approve it." } else { "Request refused." }.into()));
                    }
                    Err(error) => {
                        let mut errors = field_errors(&error);
                        if let Some(message) =
                            errors.remove(if verify { "verification" } else { "reason" })
                        {
                            errors.insert("note".into(), message);
                        }
                        self.dialog = Some(if verify {
                            Dialog::Verify { values, errors }
                        } else {
                            Dialog::Refuse { values, errors }
                        });
                        self.flash = Some(refusal(
                            error,
                            "You do not have permission to change this request.",
                            "The request could not be changed.",
                        )?);
                    }
                }
            }
            "approve" => {
                if !matches!(self.dialog.take(), Some(Dialog::Approve)) {
                    return Ok(());
                }
                let input = json!({"id": id, "version": self.version()});
                match self.pages.contracts.submit(&call(scope, credential, "approve", "base.privacy.request.approve"), input).await {
                    Ok(_) => self.flash = Some((StatusKind::Success, "Approved. Every store is being answered in the background.".into())),
                    Err(error) => {
                        self.flash = Some(refusal(
                            error,
                            "You cannot approve this request. An erasure needs someone other than whoever entered it, with approval rights.",
                            "The request could not be approved.",
                        )?)
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let display = display(&pages, &client, scope).await?;
        let capabilities = effective_capabilities(&**client, scope).await?;
        let (can_receive, can_approve) = (
            capabilities.contains(REQUEST_RECEIVE),
            capabilities.contains(REQUEST_APPROVE),
        );
        let request = pages.privacy.request(&**client, scope, self.id).await?;
        let open = matches!(request.status.as_str(), "received" | "verified");
        let located = if open {
            pages.privacy.locate(&client, scope, self.id).await?
        } else {
            Vec::new()
        };
        let mut rows: BTreeMap<String, StoreRow> = BTreeMap::new();
        for (store, label) in pages.privacy.stores() {
            rows.insert(
                store.into(),
                StoreRow {
                    store: store.into(),
                    label: label.into(),
                    found: None,
                    matched: None,
                    state: "—".into(),
                    result: "—".into(),
                    attempts: 0,
                },
            );
        }
        for (store, _, found) in &located {
            if let Some(row) = rows.get_mut(store) {
                row.found = Some(found.kinds.iter().map(|k| k.by_person).sum());
                row.matched = Some(found.kinds.iter().map(|k| k.by_identifier).sum());
            }
        }
        for progress in &request.stores {
            let row = rows
                .entry(progress.store.clone())
                .or_insert_with(|| StoreRow {
                    store: progress.store.clone(),
                    label: progress.label.clone(),
                    found: None,
                    matched: None,
                    state: String::new(),
                    result: String::new(),
                    attempts: 0,
                });
            row.state = match progress.state.as_str() {
                "pending" if progress.attempts > 0 => format!(
                    "Retrying ({})",
                    progress.last_error.as_deref().unwrap_or("failed")
                ),
                "pending" => "Working".into(),
                "answered" => "Answered".into(),
                "deferred" => "Deferred".into(),
                other => other.into(),
            };
            row.result = summary(&progress.result);
            row.attempts = progress.attempts;
        }
        self.rows = rows.into_values().collect();
        if self.rows_changed {
            self.columns.load(&self.rows, |row| row.store.clone());
        }
        let mut actions = Html::EMPTY;
        if can_receive && request.status == "received" {
            actions.push(&render(Button {
                variant: ButtonVariant::Primary,
                event: "open_verify",
                id: "privacy-verify",
                body: Html::text("Verify identity"),
                ..Button::default()
            })?);
        }
        if can_approve && request.status == "verified" {
            actions.push(&render(Button {
                variant: ButtonVariant::Primary,
                event: "request_approve",
                id: "privacy-approve",
                body: Html::text("Approve"),
                ..Button::default()
            })?);
        }
        if can_receive && open {
            actions.push(&render(Button {
                event: "open_refuse",
                id: "privacy-refuse",
                body: Html::text("Refuse"),
                ..Button::default()
            })?);
        }
        if request.export_artifact.is_some() || request.certificate_artifact.is_some() {
            let href = format!("{REQUESTS_PATH}/{}/document", request.id);
            let label = if request.export_artifact.is_some() {
                "Download export"
            } else {
                "Download certificate"
            };
            actions.push(&render(Button {
                href: &href,
                id: "privacy-document",
                body: Html::text(label),
                ..Button::default()
            })?);
        }
        let title = format!("{} request #{}", request.kind.label(), request.id);
        let header = render(Header {
            title: Html::text(&title),
            subtitle: "A data-subject request and every store's answer.",
            actions,
            ..Header::default()
        })?;
        let identifiers = if request.identifiers.is_empty() {
            "None given".to_owned()
        } else {
            request
                .identifiers
                .iter()
                .map(|i| i.value().to_owned())
                .collect::<Vec<_>>()
                .join(", ")
        };
        let facts = render(Facts {
            id: "privacy-request-facts",
            items: vec![
                Fact {
                    title: "Status",
                    id: "privacy-request-status",
                    value: status_badge(&request)?,
                },
                Fact {
                    title: "Person",
                    id: "privacy-request-person",
                    value: Html::text(&request.person.get().to_string()),
                },
                Fact {
                    title: "Emails and phones given",
                    id: "privacy-request-identifiers",
                    value: Html::text(&identifiers),
                },
                Fact {
                    title: "Received",
                    id: "privacy-request-received",
                    value: Html::text(&format!(
                        "{} by user {}",
                        when(&display, Some(&request.received_at)),
                        request.received_by
                    )),
                },
                Fact {
                    title: "Due",
                    id: "privacy-request-due",
                    value: Html::text(&when(&display, Some(&request.due_at))),
                },
                Fact {
                    title: "Identity check",
                    id: "privacy-request-verification",
                    value: Html::text(
                        request
                            .verification
                            .as_deref()
                            .unwrap_or("Not yet verified"),
                    ),
                },
                Fact {
                    title: "Approved",
                    id: "privacy-request-approved",
                    value: Html::text(&request.approved_by.map_or_else(
                        || "Not yet approved".into(),
                        |by| {
                            format!(
                                "{} by user {by}",
                                when(&display, request.approved_at.as_deref())
                            )
                        },
                    )),
                },
                Fact {
                    title: "Outcome",
                    id: "privacy-request-outcome",
                    value: Html::text(&match (&request.refused_reason, &request.completed_at) {
                        (Some(reason), _) => format!("Refused: {reason}"),
                        (None, Some(at)) => format!("Completed {}", when(&display, Some(at))),
                        _ => "Open".into(),
                    }),
                },
            ],
        })?;
        let mut body = render(Card {
            id: "privacy-request-card",
            body: facts,
            ..Card::default()
        })?;
        body.push(&render(SectionHeading {
            title: "Stores",
            id: "privacy-request-stores-heading",
            description: if open {
                "What each store holds about the person. Verifying confirms the email and phone matches; approving runs every store."
            } else {
                "Every store mounted on this server must answer before the request completes."
            },
            ..SectionHeading::default()
        })?);
        let mut table_rows = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let count = |n: Option<i64>| n.map_or_else(|| "—".to_owned(), |n| n.to_string());
            let mut flex = FlexRow::new(row.store.clone())
                .slot("store", Html::text(&row.label))
                .slot("found", Html::text(&count(row.found)))
                .slot("matched", Html::text(&count(row.matched)))
                .slot("state", Html::text(&row.state))
                .slot("result", Html::text(&row.result));
            flex.dom_id = Some(format!(
                "privacy-store-{}",
                row.store.replace(['/', '.', '_'], "-")
            ));
            let _ = row.attempts;
            table_rows.push(flex);
        }
        let mut table = FlexTable::new(
            "privacy-request-stores",
            self.columns.flex_columns(),
            table_rows,
        );
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Stores";
        table.framed = false;
        if self.rows.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No module on this server keeps personal data.",
            )));
        }
        body.push(&render(Card {
            id: "privacy-request-stores-card",
            inner_class: "p-0",
            body: render(table)?,
            ..Card::default()
        })?);
        let note_form = |event: &'static str,
                         label: &str,
                         hint: &str,
                         values: &HashMap<String, String>,
                         errors: &BTreeMap<String, Vec<String>>,
                         submit: &str|
         -> Result<Html> {
            let field = Field::in_form(NOTE_FORM, "note")
                .id("privacy-note")
                .label(label)
                .kind(InputKind::Textarea)
                .hint(hint)
                .value(values.get("note").cloned().unwrap_or_default())
                .errors(errors.get("note").cloned().unwrap_or_default())
                .required();
            let mut actions = render(Button {
                button_type: "button",
                event: "close_modal",
                body: Html::text("Cancel"),
                ..Button::default()
            })?;
            actions.push(&render(Button {
                button_type: "submit",
                variant: ButtonVariant::Primary,
                body: Html::text(submit),
                ..Button::default()
            })?);
            render(Form {
                id: "privacy-note-form",
                event,
                body: render(field)?,
                actions,
                ..Form::default()
            })
        };
        let dialog = match &self.dialog {
            Some(Dialog::Verify { values, errors }) => render(Modal {
                id: "privacy-verify-modal",
                title: "Verify identity",
                cancel_event: "close_modal",
                description: "Record how you checked that the requester is this person. Verifying also confirms the email and phone matches in the Stores table.",
                flash: Some(flash_map(&self.flash)),
                body: note_form("verify", "How the identity was checked", "For example: matched NRIC against the account holder, in person.", values, errors, "Verify")?,
                ..Modal::default()
            })?,
            Some(Dialog::Refuse { values, errors }) => render(Modal {
                id: "privacy-refuse-modal",
                title: "Refuse request",
                cancel_event: "close_modal",
                description: "Say why the request is refused. The reason is kept with the request.",
                flash: Some(flash_map(&self.flash)),
                body: note_form("refuse", "Reason", "For example: identity could not be verified.", values, errors, "Refuse")?,
                ..Modal::default()
            })?,
            Some(Dialog::Approve) => {
                let (consequence, detail) = if request.kind == RequestKind::Erasure {
                    (
                        format!("Erase person {}'s data in every store?", request.person.get()),
                        "Personal fields are replaced with [erased], files are removed and earlier audit values are cleared. Records under a legal hold are kept. This cannot be undone.".to_owned(),
                    )
                } else {
                    (format!("Export person {}'s data from every store?", request.person.get()), "Each store adds its records to one export, downloadable for a limited time.".to_owned())
                };
                render(ConfirmDialog {
                    id: "privacy-approve-confirm",
                    consequence: &consequence,
                    detail: &detail,
                    confirm: "Approve",
                    working: "Approving…",
                    confirm_event: "approve",
                    cancel_event: "cancel_approve",
                })?
            }
            None => Html::EMPTY,
        };
        let shows_modal = matches!(
            self.dialog,
            Some(Dialog::Verify { .. } | Dialog::Refuse { .. })
        );
        PageView {
            id: "privacy-request",
            flash: if shows_modal {
                NO_FLASH
            } else {
                flash_map(&self.flash)
            },
            header,
            body,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

// ------------------------------------------------------------------- holds

fn hold_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("id", "Hold"),
        BuiltinColumn::new("target", "Holds"),
        BuiltinColumn::new("reason", "Reason"),
        BuiltinColumn::new("placed", "Placed"),
        BuiltinColumn::new("status", "Status"),
    ]
}
enum HoldDialog {
    Place {
        values: HashMap<String, String>,
        errors: BTreeMap<String, Vec<String>>,
    },
    Release {
        hold: i64,
        values: HashMap<String, String>,
        errors: BTreeMap<String, Vec<String>>,
    },
}
struct Holds {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    rows: Vec<Hold>,
    dialog: Option<HoldDialog>,
    flash: Option<(StatusKind, String)>,
}
fn target_text(target: &HoldTarget) -> String {
    match target {
        HoldTarget::Person { person } => format!("Person {person} (everything)"),
        HoldTarget::Record { kind, id } => format!("{kind} {id}"),
    }
}
impl Holds {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: parse(query)?,
            columns: ListColumns::mount("privacy-holds", hold_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            dialog: None,
            flash: None,
        })
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        match value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?
        {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self
                .state
                .put_page(value.get("page").and_then(Value::as_str)),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, value),
            "new_hold" => {
                self.flash = None;
                self.dialog = Some(HoldDialog::Place {
                    values: HashMap::new(),
                    errors: BTreeMap::new(),
                });
            }
            "open_release" => {
                let hold = value
                    .get("value")
                    .and_then(Value::as_str)
                    .and_then(|v| v.parse::<i64>().ok())
                    .unwrap_or(0);
                // Released or gone since the list was drawn: the redraw shows that.
                if self
                    .rows
                    .iter()
                    .any(|h| h.id == hold && h.released_at.is_none())
                {
                    self.flash = None;
                    self.dialog = Some(HoldDialog::Release {
                        hold,
                        values: HashMap::new(),
                        errors: BTreeMap::new(),
                    });
                }
            }
            "close_modal" => self.dialog = None,
            "place" => {
                let values = params::form_values(value, HOLD_FORM);
                let number = |key: &str| {
                    values
                        .get(key)
                        .and_then(|v| v.trim().parse::<i64>().ok())
                        .unwrap_or(0)
                };
                let reason = values.get("reason").cloned().unwrap_or_default();
                let input = if values.get("target").map(String::as_str) == Some("record") {
                    json!({"record_kind": values.get("record_kind").cloned().unwrap_or_default(), "record_id": number("record_id"), "reason": reason})
                } else {
                    json!({"person_id": number("person_id"), "reason": reason})
                };
                match self
                    .pages
                    .contracts
                    .submit(
                        &call(scope, credential, "place", "base.privacy.hold.place"),
                        input,
                    )
                    .await
                {
                    Ok(_) => {
                        self.dialog = None;
                        self.flash = Some((
                            StatusKind::Success,
                            "Legal hold placed. Erasure and sweeps now keep what it names.".into(),
                        ));
                    }
                    Err(error) => {
                        let errors = field_errors(&error);
                        self.flash = Some(refusal(
                            error,
                            "You do not have permission to place legal holds.",
                            "The hold could not be placed.",
                        )?);
                        self.dialog = Some(HoldDialog::Place { values, errors });
                    }
                }
            }
            "release" => {
                let Some(HoldDialog::Release { hold, .. }) = &self.dialog else {
                    return Ok(());
                };
                let hold = *hold;
                let values = params::form_values(value, NOTE_FORM);
                let input =
                    json!({"id": hold, "reason": values.get("note").cloned().unwrap_or_default()});
                match self
                    .pages
                    .contracts
                    .submit(
                        &call(scope, credential, "release", "base.privacy.hold.release"),
                        input,
                    )
                    .await
                {
                    Ok(_) => {
                        self.dialog = None;
                        self.flash = Some((StatusKind::Success, format!("Hold {hold} released. The next erasure or sweep acts on what it kept.")));
                    }
                    Err(error) => {
                        let mut errors = field_errors(&error);
                        if let Some(message) = errors.remove("reason") {
                            errors.insert("note".into(), message);
                        }
                        self.flash = Some(refusal(
                            error,
                            "You do not have permission to release legal holds.",
                            "The hold could not be released.",
                        )?);
                        self.dialog = Some(HoldDialog::Release {
                            hold,
                            values,
                            errors,
                        });
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let display = display(&pages, &client, scope).await?;
        let can_manage = effective_capabilities(&**client, scope)
            .await?
            .contains(HOLD_MANAGE);
        let mut all = pages.privacy.holds(&**client, scope, 500).await?;
        let search = self.state.search.trim().to_lowercase();
        if !search.is_empty() {
            all.retain(|h| {
                h.reason.to_lowercase().contains(&search)
                    || target_text(&h.target).to_lowercase().contains(&search)
                    || h.id.to_string() == search
            });
        }
        let page = self.state.paginate(&all);
        let counts = page.counts;
        self.rows = page.entries.to_vec();
        if self.rows_changed {
            self.columns.load(&self.rows, |row| row.id.to_string());
        }
        let actions = if can_manage {
            render(CreateButton {
                noun: "Hold",
                id: "privacy-new-hold",
                event: "new_hold",
                button_type: "button",
                ..CreateButton::default()
            })?
        } else {
            Html::EMPTY
        };
        let header = render(Header {
            title: Html::text("Legal holds"),
            subtitle: "People and records erasure and retention sweeps must keep.",
            actions,
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let mut body = render(FilterToolbar::new(
            "privacy-holds-filters",
            vec![ToolbarControl::search(
                "privacy-holds-search",
                "search",
                "Search holds",
                &form["search"],
                "Search by hold, target or reason...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(self.rows.len());
        for hold in &self.rows {
            let key = hold.id.to_string();
            let status = match &hold.released_at {
                None => render(Badge::status(BadgeKind::Warning, "Active"))?,
                Some(at) => render(Badge::status(
                    BadgeKind::Neutral,
                    &format!("Released {}", when(&display, Some(at))),
                ))?,
            };
            let mut row = FlexRow::new(key.clone())
                .slot(
                    "id",
                    render(Mono {
                        text: &format!("#{key}"),
                    })?,
                )
                .slot("target", Html::text(&target_text(&hold.target)))
                .slot("reason", Html::text(&hold.reason))
                .slot(
                    "placed",
                    Html::text(&format!(
                        "{} by user {}",
                        when(&display, Some(&hold.placed_at)),
                        hold.placed_by
                    )),
                )
                .slot("status", status);
            row.dom_id = Some(format!("privacy-hold-{key}"));
            if can_manage && hold.released_at.is_none() {
                let id = format!("privacy-hold-release-{key}");
                row.actions = render(IconButton {
                    id: &id,
                    kind: IconKind::Neutral,
                    event: "open_release",
                    name: "value",
                    value: &key,
                    ..IconButton::new("hero-lock-open", "Release hold")
                })?;
            }
            rows.push(row);
        }
        let mut table = FlexTable::new("privacy-holds", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Legal holds";
        table.framed = false;
        table.actions = can_manage;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No legal holds."
            } else {
                "No holds match this search."
            })));
        }
        let mut list = render(table)?;
        list.push(&render(Pagination::new(
            "privacy-holds-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        body.push(&render(Card {
            id: "privacy-holds-card",
            inner_class: "p-0",
            body: list,
            ..Card::default()
        })?);
        let cancel = || {
            render(Button {
                button_type: "button",
                event: "close_modal",
                body: Html::text("Cancel"),
                ..Button::default()
            })
        };
        let dialog = match &self.dialog {
            Some(HoldDialog::Place { values, errors }) => {
                let field = |key: &str, label: &str| {
                    Field::in_form(HOLD_FORM, key)
                        .id(format!("privacy-hold-{}", key.replace('_', "-")))
                        .label(label)
                        .value(values.get(key).cloned().unwrap_or_default())
                        .errors(errors.get(key).cloned().unwrap_or_default())
                };
                let mut fields = render(
                    field("target", "Hold")
                        .kind(InputKind::Select)
                        .options(vec![
                            Choice::new("Everything about a person", "person"),
                            Choice::new("One record", "record"),
                        ])
                        .required(),
                )?;
                fields.push(&render(
                    field("person_id", "Person ID")
                        .kind(InputKind::Number)
                        .hint("For a person hold."),
                )?);
                fields.push(&render(field("record_kind", "Record kind").hint("For a record hold: the record's audit kind, such as person, user or affiliation."))?);
                fields.push(&render(
                    field("record_id", "Record ID").kind(InputKind::Number),
                )?);
                fields.push(&render(
                    field("reason", "Reason")
                        .hint("A case reference, never personal data.")
                        .required(),
                )?);
                let mut actions = cancel()?;
                actions.push(&render(Button {
                    button_type: "submit",
                    variant: ButtonVariant::Primary,
                    body: Html::text("Place hold"),
                    ..Button::default()
                })?);
                render(Modal {
                    id: "privacy-hold-modal",
                    title: "Place a legal hold",
                    cancel_event: "close_modal",
                    description:
                        "Erasure and retention sweeps keep what a hold names until it is released.",
                    flash: Some(flash_map(&self.flash)),
                    body: render(Form {
                        id: "privacy-hold-form",
                        event: "place",
                        body: fields,
                        actions,
                        ..Form::default()
                    })?,
                    ..Modal::default()
                })?
            }
            Some(HoldDialog::Release {
                hold,
                values,
                errors,
            }) => {
                let field = Field::in_form(NOTE_FORM, "note")
                    .id("privacy-release-note")
                    .label("Why it is released")
                    .kind(InputKind::Textarea)
                    .value(values.get("note").cloned().unwrap_or_default())
                    .errors(errors.get("note").cloned().unwrap_or_default())
                    .required();
                let mut actions = cancel()?;
                actions.push(&render(Button {
                    button_type: "submit",
                    variant: ButtonVariant::Primary,
                    body: Html::text("Release hold"),
                    ..Button::default()
                })?);
                let title = format!("Release hold #{hold}");
                render(Modal {
                    id: "privacy-release-modal",
                    title: &title,
                    cancel_event: "close_modal",
                    description: "The next erasure or sweep acts on what the hold kept.",
                    flash: Some(flash_map(&self.flash)),
                    body: render(Form {
                        id: "privacy-release-form",
                        event: "release",
                        body: render(field)?,
                        actions,
                        ..Form::default()
                    })?,
                    ..Modal::default()
                })?
            }
            None => Html::EMPTY,
        };
        PageView {
            id: "privacy-holds",
            flash: if self.dialog.is_some() {
                NO_FLASH
            } else {
                flash_map(&self.flash)
            },
            header,
            body,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

// --------------------------------------------------------------- retention

fn retention_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("class", "Records"),
        BuiltinColumn::new("clock", "Clock starts"),
        BuiltinColumn::new("action", "Then"),
        BuiltinColumn::new("default", "Default"),
        BuiltinColumn::new("minimum", "Legal minimum"),
        BuiltinColumn::new("period", "This tenant keeps"),
        BuiltinColumn::new("sweep", "Last sweep"),
    ]
}
fn days_text(days: Option<u32>) -> String {
    match days {
        None => "Forever".into(),
        Some(1) => "1 day".into(),
        Some(days) => format!("{days} days"),
    }
}
struct PeriodDialog {
    class: String,
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
}
struct Retention {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    rows: Vec<ClassView>,
    dialog: Option<PeriodDialog>,
    flash: Option<(StatusKind, String)>,
}
impl Retention {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: parse(query)?,
            columns: ListColumns::mount("privacy-retention", retention_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            dialog: None,
            flash: None,
        })
    }
    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        match value
            .get("type")
            .and_then(Value::as_str)
            .ok_or(Error::Invalid)?
        {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self
                .state
                .put_page(value.get("page").and_then(Value::as_str)),
            "grid" => match self.columns.handle(&params::string_values(value)) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => clear_flash(&mut self.flash, value),
            "open_period" => {
                let class = value.get("value").and_then(Value::as_str).unwrap_or("");
                // Gone since the list was drawn: the redraw shows that.
                if let Some(row) = self.rows.iter().find(|row| row.key == class) {
                    self.flash = None;
                    self.dialog = Some(PeriodDialog {
                        class: row.key.to_owned(),
                        values: HashMap::from([(
                            "days".to_owned(),
                            row.tenant_days.map(|d| d.to_string()).unwrap_or_default(),
                        )]),
                        errors: BTreeMap::new(),
                    });
                }
            }
            "close_modal" => self.dialog = None,
            "set_retention" => {
                let Some(dialog) = &self.dialog else {
                    return Ok(());
                };
                let class = dialog.class.clone();
                let values = params::form_values(value, PERIOD_FORM);
                let days = values.get("days").map(|d| d.trim()).unwrap_or("");
                let input = if days.is_empty() {
                    json!({"class": class})
                } else {
                    match days.parse::<u64>() {
                        Ok(days) => json!({"class": class, "days": days}),
                        Err(_) => {
                            self.dialog = Some(PeriodDialog {
                                class,
                                values,
                                errors: BTreeMap::from([(
                                    "days".to_owned(),
                                    vec!["Give a whole number of days, or leave it empty for the default.".to_owned()],
                                )]),
                            });
                            return Ok(());
                        }
                    }
                };
                match self
                    .pages
                    .contracts
                    .submit(
                        &call(
                            scope,
                            credential,
                            "set_retention",
                            "base.privacy.retention.set",
                        ),
                        input,
                    )
                    .await
                {
                    Ok(_) => {
                        self.dialog = None;
                        self.flash = Some((
                            StatusKind::Success,
                            "Retention period saved. The next sweep uses it.".into(),
                        ));
                    }
                    Err(error) => {
                        let errors = field_errors(&error);
                        self.flash = Some(refusal(
                            error,
                            "You do not have permission to set retention periods.",
                            "The retention period could not be saved.",
                        )?);
                        self.dialog = Some(PeriodDialog {
                            class,
                            values,
                            errors,
                        });
                    }
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let display = display(&pages, &client, scope).await?;
        let can_manage = effective_capabilities(&**client, scope)
            .await?
            .contains(RETENTION_MANAGE);
        let mut all = pages.privacy.retention_classes(&client, scope).await?;
        let search = self.state.search.trim().to_lowercase();
        if !search.is_empty() {
            all.retain(|c| {
                c.label.to_lowercase().contains(&search) || c.key.to_lowercase().contains(&search)
            });
        }
        let page = self.state.paginate(&all);
        let counts = page.counts;
        self.rows = page.entries.to_vec();
        if self.rows_changed {
            self.columns.load(&self.rows, |row| row.key.to_owned());
        }
        let header = render(Header {
            title: Html::text("Retention"),
            subtitle: "How long this tenant keeps each kind of record before it is purged or anonymised. Legal holds keep what they name.",
            ..Header::default()
        })?;
        let form = self.state.filters_form();
        let mut body = render(FilterToolbar::new(
            "privacy-retention-filters",
            vec![ToolbarControl::search(
                "privacy-retention-search",
                "search",
                "Search record classes",
                &form["search"],
                "Search by records or key...",
            )],
        ))?;
        let mut rows = Vec::with_capacity(self.rows.len());
        for class in &self.rows {
            let action = match class.action {
                RetentionAction::Purge => "Purge",
                RetentionAction::Anonymise => "Anonymise",
            };
            let period = match (class.tenant_days, class.effective_days) {
                (Some(_), days) => render(Badge::status(BadgeKind::Success, &days_text(days)))?,
                (None, days) => Html::text(&format!("{} (default)", days_text(days))),
            };
            let sweep = match &class.last_sweep {
                Some(sweep) => Html::text(&format!(
                    "{}: {} acted on, {} kept",
                    when(&display, Some(&sweep.finished_at)),
                    sweep.acted,
                    sweep.kept
                )),
                None => Html::text("Not swept yet"),
            };
            let mut row = FlexRow::new(class.key.to_owned())
                .slot("class", Html::text(class.label))
                .slot("clock", Html::text(class.clock_start))
                .slot("action", Html::text(action))
                .slot("default", Html::text(&days_text(class.default_days)))
                .slot(
                    "minimum",
                    Html::text(
                        &class
                            .minimum_days
                            .map_or("None".into(), |d| days_text(Some(d))),
                    ),
                )
                .slot("period", period)
                .slot("sweep", sweep);
            row.dom_id = Some(format!(
                "privacy-retention-{}",
                class.key.replace(['/', '.'], "-")
            ));
            if can_manage {
                let id = format!(
                    "privacy-retention-set-{}",
                    class.key.replace(['/', '.'], "-")
                );
                row.actions = render(IconButton {
                    id: &id,
                    kind: IconKind::Neutral,
                    event: "open_period",
                    name: "value",
                    value: class.key,
                    ..IconButton::new("hero-pencil", "Set period")
                })?;
            }
            rows.push(row);
        }
        let mut table = FlexTable::new("privacy-retention", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Record classes";
        table.framed = false;
        table.actions = can_manage;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No mounted module keeps records on a retention clock."
            } else {
                "No record classes match this search."
            })));
        }
        let mut list = render(table)?;
        list.push(&render(Pagination::new(
            "privacy-retention-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        body.push(&render(Card {
            id: "privacy-retention-card",
            inner_class: "p-0",
            body: list,
            ..Card::default()
        })?);
        let dialog = match &self.dialog {
            Some(dialog) => {
                let class = self.rows.iter().find(|row| row.key == dialog.class);
                let hint = class.map_or(String::new(), |class| {
                    format!(
                        "Leave empty for the default ({}).{}",
                        days_text(class.default_days),
                        class.minimum_days.map_or(String::new(), |m| format!(
                            " No shorter than the legal minimum of {}.",
                            days_text(Some(m))
                        ))
                    )
                });
                let field = Field::in_form(PERIOD_FORM, "days")
                    .id("privacy-retention-days")
                    .label("Days to keep")
                    .kind(InputKind::Number)
                    .hint(&hint)
                    .value(dialog.values.get("days").cloned().unwrap_or_default())
                    .errors(dialog.errors.get("days").cloned().unwrap_or_default());
                let mut actions = render(Button {
                    button_type: "button",
                    event: "close_modal",
                    body: Html::text("Cancel"),
                    ..Button::default()
                })?;
                actions.push(&render(Button {
                    button_type: "submit",
                    variant: ButtonVariant::Primary,
                    body: Html::text("Save period"),
                    ..Button::default()
                })?);
                let title = format!(
                    "Retention: {}",
                    class.map_or(dialog.class.as_str(), |c| c.label)
                );
                render(Modal {
                    id: "privacy-retention-modal",
                    title: &title,
                    cancel_event: "close_modal",
                    description: "Sweeps purge or anonymise records once this period has passed since the clock started.",
                    flash: Some(flash_map(&self.flash)),
                    body: render(Form {
                        id: "privacy-retention-form",
                        event: "set_retention",
                        body: render(field)?,
                        actions,
                        ..Form::default()
                    })?,
                    ..Modal::default()
                })?
            }
            None => Html::EMPTY,
        };
        PageView {
            id: "privacy-retention",
            flash: if self.dialog.is_some() {
                NO_FLASH
            } else {
                flash_map(&self.flash)
            },
            header,
            body,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

// --------------------------------------------------------------- the pages

enum State {
    Requests(Requests),
    Request(Box<RequestPage>),
    Holds(Holds),
    Retention(Retention),
}
impl State {
    fn build(
        pages: &Pages,
        operation: &str,
        path: &HashMap<String, String>,
        query: &HashMap<String, String>,
    ) -> Result<Self> {
        Ok(match operation {
            "requests" => Self::Requests(Requests::new(pages.clone(), query)?),
            "request" => {
                let id = path
                    .get("id")
                    .and_then(|id| id.parse::<i64>().ok())
                    .filter(|id| *id > 0)
                    .ok_or(Error::NotFound)?;
                Self::Request(Box::new(RequestPage::new(pages.clone(), id)?))
            }
            "holds" => Self::Holds(Holds::new(pages.clone(), query)?),
            "retention" => Self::Retention(Retention::new(pages.clone(), query)?),
            _ => return Err(Error::NotFound),
        })
    }
    async fn render(&mut self, scope: &Scope) -> Result<String> {
        match self {
            Self::Requests(page) => page.render(scope).await,
            Self::Request(page) => page.render(scope).await,
            Self::Holds(page) => page.render(scope).await,
            Self::Retention(page) => page.render(scope).await,
        }
    }
}
impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            match self {
                Self::Requests(page) => page.rows_changed = true,
                Self::Request(page) => page.rows_changed = true,
                Self::Holds(page) => page.rows_changed = true,
                Self::Retention(page) => page.rows_changed = true,
            }
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => match self {
                    Self::Requests(page) => page.state = parse(values)?,
                    Self::Holds(page) => page.state = parse(values)?,
                    Self::Retention(page) => page.state = parse(values)?,
                    Self::Request(_) => {}
                },
                LiveInput::Event(value) => match self {
                    Self::Requests(page) => {
                        page.event(request.scope, request.credential, value).await?
                    }
                    Self::Request(page) => {
                        page.event(request.scope, request.credential, value).await?
                    }
                    Self::Holds(page) => {
                        page.event(request.scope, request.credential, value).await?
                    }
                    Self::Retention(page) => {
                        page.event(request.scope, request.credential, value).await?
                    }
                },
            }
            if let Self::Requests(page) = self {
                if let Some((path, message)) = page.redirect.take() {
                    return Ok(LiveBody {
                        redirect: Some(path),
                        flash: Some((StatusKind::Success, message)),
                        ..LiveBody::default()
                    });
                }
            }
            let html = self.render(request.scope).await?;
            let (query, rows) = match self {
                Self::Requests(page) => (page.state.to_params(), page.rows.len()),
                Self::Holds(page) => (page.state.to_params(), page.rows.len()),
                Self::Retention(page) => (page.state.to_params(), page.rows.len()),
                Self::Request(page) => (HashMap::new(), page.rows.len()),
            };
            Ok(LiveBody {
                html,
                query,
                rows,
                ..LiveBody::default()
            })
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "requests" => Some("Data-subject requests"),
            "request" => Some("Data-subject request"),
            "holds" => Some("Legal holds"),
            "retention" => Some("Retention"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::build(self, request.operation, request.path, request.query)?;
            let html = state.render(scope).await?;
            Ok(PageBody {
                html,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(State::build(
                self,
                request.operation,
                request.path,
                request.query,
            )?);
            Ok(Some(state))
        })
    }
}

/// `/privacy/requests/{id}/document`: the export bundle or the certificate,
/// read through Artifacts so the download is audited.
pub(crate) struct Documents {
    pub db: Db,
    pub privacy: Arc<Privacy>,
}
impl Download<Scope, Error> for Documents {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let id = request
                .path
                .get("id")
                .and_then(|id| id.parse::<i64>().ok())
                .filter(|id| *id > 0)
                .ok_or(Error::NotFound)?;
            let mut client = self.db.get().await?;
            let (content_type, bytes, name) = self
                .privacy
                .request_document(&mut client, request.scope, id)
                .await?;
            Ok(FileBody::attachment(bytes, &content_type, &name))
        })
    }
}
