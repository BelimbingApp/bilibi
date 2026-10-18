//! `/workflow/reference/{id}` (`base/workflow.pages`, operation `reference`):
//! the generic Workflow action and history adapter, ported from Bilimbi
//! `apps/base/workflow/lib/workflow/web/reference_live.{ex,html.heex}`.
//!
//! It is the documented owner integration: a screen reads
//! `Engine::available_actions`, `history` and `pending_work`, renders each
//! offered action as a form that echoes its `ActionRequest` fields, and runs
//! `Engine::execute_action` on submit. The page never writes the owner row,
//! never completes work itself and never carries the actor, tenant or
//! capability: the gate is what makes a double-click, a resend after a
//! reconnect or a stale page safe. Each offered action is rendered with a
//! fresh idempotency key; two submissions of one rendering replay the first
//! result instead of acting twice, and a submission against a record another
//! session changed conflicts with the versions the page echoed.
//!
//! The route requires `admin.reference.record.approve`, which this module
//! declares and grants to no system role. The `reference.record` subject is
//! contributed only by the disposable fixture executables (see README), so a
//! production install answers Bilimbi's generic "Workflow could not complete
//! the request. Refresh the page and review the record." here.
use crate::{
    history::Fact,
    pending::{PendingEntry, PendingQuery},
    ActionRequest, AvailableAction, Engine,
};
use askama::Template;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    BuiltinColumn, Cell, ColumnType, ColumnsOutcome, EmptyState, FlashMap, FlexRow, FlexTable,
    Html, Instant, ListColumns, StatusKind, TableEmpty,
};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};

/// The route capability. Declared in the vocabulary; no system role holds it.
pub const APPROVE: &str = "admin.reference.record.approve";
/// The bounded, non-business subject the reference adapters own.
pub const SUBJECT: &str = "reference.record";
const WORK_LIST: &str = "workflow-reference-work-rows";
const HISTORY_LIST: &str = "workflow-reference-history-rows";
const HISTORY_LIMIT: i64 = 500;
const WORK_LIMIT: i64 = 50;

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub struct Pages {
    pub engine: Arc<Engine>,
    pub db: Db,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

/// Bilimbi's `failure_copy/1`: one sentence per refusal the person can act on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Failure {
    MissingCapability,
    StaleAction,
    SubjectNotFound,
    InvalidSubject,
    Changed,
    Other,
}
impl Failure {
    fn copy(self) -> &'static str {
        match self {
            Self::MissingCapability => "Your role does not allow this action.",
            Self::StaleAction => "That action is no longer available. Refresh the page.",
            Self::SubjectNotFound => "This record is not available.",
            Self::InvalidSubject => "This record address is not valid.",
            Self::Changed => "This record changed. Refresh the page and try again.",
            Self::Other => {
                "Workflow could not complete the request. Refresh the page and review the record."
            }
        }
    }
    /// A kernel refusal while loading the record. An unregistered subject
    /// kind (`Invalid`) is the production answer, and Bilimbi words it with
    /// its generic copy rather than "not available", so it is `Other`.
    fn on_load(error: &Error) -> Self {
        match error {
            Error::NotFound => Self::SubjectNotFound,
            Error::Forbidden => Self::MissingCapability,
            _ => Self::Other,
        }
    }
    fn on_execute(error: &Error) -> Self {
        match error {
            Error::Forbidden => Self::MissingCapability,
            Error::Conflict => Self::Changed,
            Error::NotFound => Self::SubjectNotFound,
            _ => Self::Other,
        }
    }
}

/// One offered action with the idempotency key this rendering echoes.
#[derive(Clone, Debug)]
struct Offered {
    entry: AvailableAction,
    idempotency_key: String,
}
impl Offered {
    fn new(entry: AvailableAction) -> Self {
        Self {
            entry,
            idempotency_key: format!("reference:{}", uuid::Uuid::new_v4()),
        }
    }
}

/// A cell for the page's own fragments of the built-in columns.
#[derive(Template)]
#[template(source = r#"<span class="truncate">{{ text }}</span>"#, ext = "html")]
struct TextCell<'a> {
    text: &'a str,
}

/// The whole page, laid out as Bilimbi's `reference_live.html.heex`.
#[derive(Template)]
#[template(path = "reference.html")]
struct View<'a> {
    flash: FlashMap<'a>,
    failure: Option<&'static str>,
    actions: &'a [Offered],
    no_actions: EmptyState,
    work_count: usize,
    work_table: Html,
    history_table: Html,
}

pub struct State {
    pages: Pages,
    /// `None` when the address was not a positive integer.
    id: Option<i64>,
    subject_version: Option<i64>,
    actions: Vec<Offered>,
    history: Vec<Fact>,
    work: Vec<PendingEntry>,
    failure: Option<Failure>,
    flash: Option<(StatusKind, String)>,
    work_columns: ListColumns,
    history_columns: ListColumns,
}

fn columns(id: &str, builtins: Vec<BuiltinColumn>) -> Result<ListColumns> {
    ListColumns::mount(id, builtins).map_err(|_| Error::Internal)
}
fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
/// The event payload's values as strings, the shape every op is read in.
fn value(event: &Value, key: &str) -> Option<String> {
    match event.get(key)? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}
fn positive(text: &str) -> Option<i64> {
    text.parse::<i64>().ok().filter(|id| *id > 0)
}

impl State {
    fn new(pages: &Pages, path: &HashMap<String, String>) -> Result<Self> {
        let mut history_when = BuiltinColumn::new("transitioned_at", "When");
        history_when.kind = ColumnType::Datetime;
        Ok(Self {
            pages: pages.clone(),
            id: path.get("id").and_then(|id| positive(id)),
            subject_version: None,
            actions: vec![],
            history: vec![],
            work: vec![],
            failure: None,
            flash: None,
            work_columns: columns(
                WORK_LIST,
                vec![
                    BuiltinColumn::new("label", "Work"),
                    BuiltinColumn::new("status", "Status"),
                ],
            )?,
            history_columns: columns(
                HISTORY_LIST,
                vec![BuiltinColumn::new("status", "Status"), history_when],
            )?,
        })
    }

    /// Reads the record through the engine; every refusal becomes the page's
    /// failure copy rather than a page-clearing error.
    async fn load(&mut self, scope: &Scope) -> Result<()> {
        let Some(id) = self.id else {
            return self.empty(Failure::InvalidSubject);
        };
        let engine = &self.pages.engine;
        let loaded = async {
            let available = engine.available_actions(scope, SUBJECT, id).await?;
            let history = engine
                .history(scope, SUBJECT, id, None, HISTORY_LIMIT)
                .await?;
            let work = engine
                .pending_work(scope, &PendingQuery::new(WORK_LIMIT).subject(SUBJECT, id))
                .await?;
            Ok::<_, Error>((available, history.entries, work.entries))
        }
        .await;
        match loaded {
            Ok((available, history, work)) => {
                self.subject_version = Some(available.subject_version);
                self.actions = available.actions.into_iter().map(Offered::new).collect();
                self.history = history;
                self.work = work;
                self.failure = None;
                self.reload_columns();
                Ok(())
            }
            Err(Error::Unauthorized) => Err(Error::Unauthorized),
            Err(error) => self.empty(Failure::on_load(&error)),
        }
    }
    fn empty(&mut self, failure: Failure) -> Result<()> {
        self.failure = Some(failure);
        self.subject_version = None;
        self.actions.clear();
        self.history.clear();
        self.work.clear();
        self.reload_columns();
        Ok(())
    }
    fn reload_columns(&mut self) {
        self.work_columns
            .load(&self.work, |e| e.item.id.to_string());
        self.history_columns
            .load(&self.history, |f| f.id.to_string());
    }

    /// Bilimbi's `execute/3`: the action must still be one this rendering
    /// offered; the kernel decides everything else under the subject lock.
    async fn execute(&mut self, scope: &Scope, event: &Value) -> Result<()> {
        let key = value(event, "action").unwrap_or_default();
        let work_item_id = value(event, "work_item_id").and_then(|id| positive(&id));
        let idempotency_key = value(event, "idempotency_key").unwrap_or_default();
        let (Some(id), Some(version)) = (self.id, self.subject_version) else {
            self.failure = Some(Failure::StaleAction);
            return Ok(());
        };
        let Some(offered) = self
            .actions
            .iter()
            .find(|o| o.entry.key == key && o.entry.work_item_id == work_item_id)
        else {
            self.failure = Some(Failure::StaleAction);
            return Ok(());
        };
        // The key is the one this page rendered for the action, or the one an
        // earlier rendering did: a resend of a submission already answered
        // replays its result. Anything else is not a page submission.
        if !idempotency_key.starts_with("reference:")
            || idempotency_key.len() != "reference:".len() + 36
            || uuid::Uuid::parse_str(&idempotency_key["reference:".len()..]).is_err()
        {
            self.failure = Some(Failure::StaleAction);
            return Ok(());
        }
        let request = ActionRequest::new(&offered.entry.key, idempotency_key, version)
            .for_entry(&offered.entry);
        match self
            .pages
            .engine
            .execute_action(scope, SUBJECT, id, request)
            .await
        {
            Ok(_) => {
                self.flash = Some((StatusKind::Success, "Action completed.".into()));
                self.load(scope).await
            }
            Err(Error::Unauthorized) => Err(Error::Unauthorized),
            Err(error) => {
                self.failure = Some(Failure::on_execute(&error));
                Ok(())
            }
        }
    }

    fn grid(columns: &mut ListColumns, event: &Value) {
        let params: HashMap<String, String> = event
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, _)| Some((key.clone(), value(event, key)?)))
            .collect();
        // Built-in columns here carry no sort key, so Sort never arrives;
        // Update and Noop both re-render what the arrangement now shows.
        let _ = matches!(columns.handle(&params), ColumnsOutcome::Update);
    }

    async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.pages.db.get().await?;
        let locale = self
            .pages
            .locales
            .resolve(&**client, scope, None)
            .await?
            .locale;
        self.pages
            .display
            .display(&**client, scope, None, locale)
            .await
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let display = self.display(scope).await?;
        let flash = match &self.flash {
            Some((StatusKind::Success, message)) => FlashMap {
                success: message,
                ..bilibi_ui::NO_FLASH
            },
            Some((StatusKind::Error, message)) => FlashMap {
                error: message,
                ..bilibi_ui::NO_FLASH
            },
            _ => bilibi_ui::NO_FLASH,
        };
        let view = View {
            flash,
            failure: self.failure.map(Failure::copy),
            actions: &self.actions,
            no_actions: EmptyState::missing(
                "No actions available",
                "Actions appear when your role and the current work allow them.",
            ),
            work_count: self.work.len(),
            work_table: self.work_table()?,
            history_table: self.history_table(&display)?,
        };
        view.render().map_err(|_| Error::Internal)
    }

    fn work_table(&self) -> Result<Html> {
        let mut rows = Vec::new();
        for entry in &self.work {
            let mut row = FlexRow::new(entry.item.id.to_string());
            row.dom_id = Some(format!("workflow-work-{}", entry.item.id));
            row = row
                .slot(
                    "label",
                    render(TextCell {
                        text: &entry.item.label,
                    })?,
                )
                .slot(
                    "status",
                    render(TextCell {
                        text: &entry.item.status,
                    })?,
                );
            rows.push(row);
        }
        let mut table = FlexTable::new(WORK_LIST, self.work_columns.flex_columns(), rows);
        table.event = "work_grid";
        table.zoom = self.work_columns.zoom;
        table.suggestions = self.work_columns.flex_suggestions();
        table.add_query = &self.work_columns.add_query;
        table.caption = "Pending work";
        table.empty = Some(TableEmpty::State(EmptyState::missing(
            "No pending work",
            "",
        )));
        render(table)
    }

    fn history_table(&self, display: &DisplayContext) -> Result<Html> {
        let mut rows = Vec::new();
        for fact in &self.history {
            let mut row = FlexRow::new(fact.id.to_string());
            row.dom_id = Some(format!("workflow-history-{}", fact.id));
            row = row.slot("status", render(TextCell { text: &fact.status })?);
            // The kernel returns RFC 3339 UTC; the shared `<time>` markup then
            // follows the reader's clock like Bilimbi's `<.datetime>`.
            let at = fact
                .transitioned_at
                .parse::<jiff::Timestamp>()
                .map_err(|_| Error::Internal)?;
            let rendered = display.render(at, Format::DateTime, Precision::Minute);
            row = row.cell(
                "transitioned_at",
                Cell::instant(Instant::new(
                    &rendered.iso,
                    &rendered.text,
                    &rendered.company,
                    &rendered.utc,
                    rendered.mode.as_str(),
                )),
            );
            rows.push(row);
        }
        let mut table = FlexTable::new(HISTORY_LIST, self.history_columns.flex_columns(), rows);
        table.event = "history_grid";
        table.zoom = self.history_columns.zoom;
        table.suggestions = self.history_columns.flex_suggestions();
        table.add_query = &self.history_columns.add_query;
        table.caption = "Status history";
        table.empty = Some(TableEmpty::State(EmptyState::missing("No history yet", "")));
        render(table)
    }
}

impl Page<Scope, Error> for Pages {
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "reference" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::new(self, request.path)?;
            state.load(scope).await?;
            Ok(PageBody {
                html: state.body(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            if request.operation != "reference" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut state = State::new(self, request.path)?;
            state.load(scope).await?;
            let state: Box<dyn LiveState<Scope, Error>> = Box::new(state);
            Ok(Some(state))
        })
    }
}

impl LiveState<Scope, Error> for State {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope;
            match request.input {
                LiveInput::Refresh | LiveInput::Params(_) => self.load(scope).await?,
                LiveInput::Event(event) => match value(event, "type").as_deref() {
                    Some("execute") => self.execute(scope, event).await?,
                    Some("clear_failure") => self.failure = None,
                    Some("clear-flash") => {
                        if self.flash.as_ref().is_some_and(|(kind, _)| {
                            value(event, "key").as_deref() == Some(kind.name())
                        }) {
                            self.flash = None;
                        }
                    }
                    Some("work_grid") => Self::grid(&mut self.work_columns, event),
                    Some("history_grid") => Self::grid(&mut self.history_columns, event),
                    _ => return Err(Error::Invalid),
                },
            }
            let html = self.body(scope).await?;
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
}
