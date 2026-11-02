//! The work calendar pages (issue 209): the calendars and holiday sets
//! lists, a calendar and a set read first and edited in place, and a
//! calendar's month of resolved days drawn with the shared day tile. The
//! lists, dialogs and in-place editors follow Core
//! Currency's pages.
//!
//! Reads use `WorkCalendars`, which rechecks the capability on each call.
//! Every write calls a `core.work_calendar.*` contract through
//! `PageContracts` (declared in `contributions.rs`), which rechecks it inside
//! its transaction and fences the version the page read. A control the person
//! may not use is hidden; a refusal is a message on the page or on its fact,
//! never a cleared page.
mod calendar;
mod calendars;
mod holiday_set;
mod holiday_sets;
mod month;
mod week;

use crate::WorkCalendars;
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_geonames::Geonames;
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    FlashMap, Html, PageVariant, StatusKind, NO_FLASH,
};
use jiff::civil::Date;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

pub(crate) const SERVICE: &str = "core/work_calendar.pages";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission to change work calendars.";
const FAILED: &str =
    "The change was not saved. Try again, and tell your administrator if it keeps failing.";

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub calendars: Arc<WorkCalendars>,
    pub settings: Arc<Settings>,
    pub geonames: Arc<Geonames>,
    pub contracts: Arc<PageContracts>,
}

/// Why a contract refused a page's write. Base Step-up and a lost session
/// are not here: the host prompts and comes back.
enum Refusal {
    Denied,
    /// The record changed since the page read it (its version moved on).
    Stale,
    /// The contract's own words, by input field.
    Invalid(BTreeMap<String, Vec<String>>),
    Failed,
}
impl Refusal {
    /// The page's sentence for it; `noun` names what changed ("calendar").
    fn sentence(&self, noun: &str) -> String {
        match self {
            Self::Denied => DENIED.into(),
            Self::Stale => format!(
                "This {noun} changed since you opened it. Reload the page to see the change, then try again."
            ),
            Self::Invalid(fields) => fields
                .iter()
                .flat_map(|(field, messages)| {
                    messages
                        .iter()
                        .map(move |message| bilibi_ui::error_sentence(&label(field), message))
                })
                .collect::<Vec<_>>()
                .join(" "),
            Self::Failed => FAILED.into(),
        }
    }
    /// Each message by the form key it belongs to (`week.monday.periods` is
    /// `monday_periods`, `exception.type` is `type`, `add.0.name` is `name`)
    /// for the fields the form `shows`, and the rest as one sentence for the
    /// page, so no refusal goes unsaid.
    fn fields(self, shows: impl Fn(&str) -> bool) -> (BTreeMap<String, Vec<String>>, String) {
        let Self::Invalid(fields) = self else {
            return (BTreeMap::new(), String::new());
        };
        let (mut keyed, mut rest): (BTreeMap<String, Vec<String>>, _) =
            (BTreeMap::new(), BTreeMap::new());
        for (field, messages) in fields {
            let key = form_key(&field);
            if shows(&key) {
                keyed.entry(key).or_default().extend(messages);
            } else {
                rest.insert(field, messages);
            }
        }
        let sentence = if rest.is_empty() {
            String::new()
        } else {
            Self::Invalid(rest).sentence("")
        };
        (keyed, sentence)
    }
}
/// The form key a contract's input field is posted as.
fn form_key(field: &str) -> String {
    let field = field
        .trim_start_matches("week.")
        .trim_start_matches("exception.");
    let parts: Vec<&str> = field.split('.').collect();
    match parts.as_slice() {
        [day] if week::is_day(day) => format!("{day}_periods"),
        [day, part] if week::is_day(day) => format!("{day}_{part}"),
        ["add" | "remove", _, part] => (*part).to_owned(),
        ["add" | "remove", _] => "date".to_owned(),
        _ => field.to_owned(),
    }
}
/// A contract field's words, for a sentence: `admin1_code` is "Division".
fn label(field: &str) -> String {
    let key = form_key(field);
    let named = match key.as_str() {
        "code" => "Code",
        "name" => "Name",
        "timezone" => "Zone",
        "substitute" => "Holiday substitution",
        "effective_from" => "Applies from",
        "starts_on" => "From",
        "ends_on" => "To",
        "type" => "Type",
        "periods" => "Hours",
        "country_iso" => "Country",
        "admin1_code" => "Division",
        "holiday_set" => "Holiday set",
        "date" => "Date",
        "version" => "Version",
        "week" => "Week",
        other => return week::label(other).unwrap_or_else(|| other.replace('_', " ")),
    };
    named.into()
}
type Outcome = std::result::Result<Value, Refusal>;
fn outcome(result: bilibi_agent_api::Result<Value>) -> Result<Outcome> {
    Ok(match result {
        Ok(value) => Ok(value),
        Err(ContractError::Backend(
            error @ (Error::Unauthorized | Error::StepUpRequired { .. }),
        )) => return Err(error),
        Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
            Err(Refusal::Denied)
        }
        Err(ContractError::Backend(Error::Conflict)) => Err(Refusal::Stale),
        // The commit's own preview refused: say why it did.
        Err(ContractError::PlanRejected(results)) => Err(rejected(&results[0]["error"])?),
        Err(ContractError::InvalidInput(fields)) => Err(Refusal::Invalid(fields)),
        Err(_) => Err(Refusal::Failed),
    })
}
/// A plan's refused preview, by the error it carries.
fn rejected(error: &Value) -> Result<Refusal> {
    Ok(match error["code"].as_str() {
        Some("invalid_input") => Refusal::Invalid(
            serde_json::from_value(error["fields"].clone()).map_err(|_| Error::Internal)?,
        ),
        Some("missing_capability") => Refusal::Denied,
        _ if error["reason"] == "record_changed" => Refusal::Stale,
        _ => Refusal::Failed,
    })
}

impl Pages {
    fn call<'a>(
        scope: &'a Scope,
        credential: &'a str,
        event: &'a str,
        contract: &'static str,
    ) -> Call<'a> {
        Call {
            scope,
            credential,
            event,
            contract,
            service: SERVICE,
        }
    }
    /// A form the person filled in: the contract's preview and commit at once.
    async fn submit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Outcome> {
        outcome(
            self.contracts
                .submit(&Self::call(scope, credential, event, contract), input)
                .await,
        )
    }
    /// The dry-run half of a confirmation.
    async fn preview(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<std::result::Result<(), Refusal>> {
        let value = match outcome(
            self.contracts
                .preview(&Self::call(scope, credential, event, contract), input)
                .await,
        )? {
            Ok(value) => value,
            Err(refusal) => return Ok(Err(refusal)),
        };
        if value["valid"] != true {
            return Ok(Err(rejected(&value["results"][0]["error"])?));
        }
        Ok(Ok(()))
    }
    async fn commit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &str,
        contract: &'static str,
        input: Value,
    ) -> Result<Outcome> {
        outcome(
            self.contracts
                .commit(&Self::call(scope, credential, event, contract), input)
                .await,
        )
    }
    async fn allowed(&self, scope: &Scope, capability: &str) -> Result<bool> {
        Ok(self.capabilities(scope).await?.contains(capability))
    }
    /// Everything the person may do now, for a view deciding several things.
    async fn capabilities(
        &self,
        scope: &Scope,
    ) -> Result<std::collections::BTreeSet<&'static str>> {
        let client = self.db.get().await?;
        effective_capabilities(&**client, scope).await
    }
    /// The zone people here work in: the current company's
    /// `localization.timezone`, which a company without its own takes from
    /// the tenant. A new calendar starts in it.
    async fn zone(&self, scope: &Scope) -> Result<String> {
        let client = self.db.get().await?;
        let key = bilibi_datetime::TIMEZONE_KEY;
        let resolved = match scope.current_company() {
            Some(company) => match self
                .settings
                .get(&**client, scope, Target::Company(company), key)
                .await
            {
                Ok(resolved) => resolved,
                // An archived or unreachable company holds no settings.
                Err(Error::NotFound | Error::Forbidden) => {
                    self.settings
                        .get(&**client, scope, Target::Tenant, key)
                        .await?
                }
                Err(error) => return Err(error),
            },
            None => {
                self.settings
                    .get(&**client, scope, Target::Tenant, key)
                    .await?
            }
        };
        Ok(resolved
            .value
            .as_str()
            .filter(|zone| bilibi_datetime::valid_timezone(zone))
            .unwrap_or(bilibi_datetime::DEFAULT_TIMEZONE)
            .to_owned())
    }
    /// Today in that zone: the default month, year and first date.
    async fn today(&self, scope: &Scope) -> Result<Date> {
        let zone = self.zone(scope).await?;
        Ok(crate::resolve::today(&zone, jiff::Timestamp::now()))
    }
    async fn state(
        &self,
        request: &PageRequest<'_, Scope>,
    ) -> Result<Box<dyn LiveState<Scope, Error>>> {
        Ok(match request.operation {
            "calendars" => Box::new(calendars::Calendars::new(self.clone(), request)?),
            "calendar" => Box::new(calendar::Record::new(self.clone(), request)?),
            "month" => Box::new(month::Month::new(self.clone(), request)?),
            "holiday_sets" => Box::new(holiday_sets::Sets::new(self.clone(), request)?),
            "holiday_set" => Box::new(holiday_set::Record::new(self.clone(), request)?),
            _ => return Err(Error::NotFound),
        })
    }
}
impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        Some(match operation {
            "calendars" => "Work Calendars",
            "calendar" => "Work Calendar",
            "month" => "Month",
            "holiday_sets" => "Holiday Sets",
            _ => "Holiday Set",
        })
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let pages = self.clone();
            let request = &request;
            match request.operation {
                "calendars" => first(calendars::Calendars::new(pages, request)?, scope).await,
                "calendar" => first(calendar::Record::new(pages, request)?, scope).await,
                "month" => first(month::Month::new(pages, request)?, scope).await,
                "holiday_sets" => first(holiday_sets::Sets::new(pages, request)?, scope).await,
                "holiday_set" => first(holiday_set::Record::new(pages, request)?, scope).await,
                _ => Err(Error::NotFound),
            }
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move { Ok(Some(self.state(&request).await?)) })
    }
}

/// A page's state that may name the record it shows.
trait Titled {
    /// The record's name for the document title, once loaded.
    fn title(&self) -> Option<String> {
        None
    }
}
/// The first render: the state's first update, titled by its record.
async fn first<S: LiveState<Scope, Error> + Titled>(
    mut state: S,
    scope: &Scope,
) -> Result<PageBody> {
    let body = state
        .update(LiveRequest {
            scope,
            credential: "",
            input: LiveInput::Refresh,
        })
        .await?;
    Ok(PageBody {
        html: body.html,
        live: true,
        title: state.title(),
    })
}

/// The record a path names: a code as stored, else no such page.
fn code(path: &HashMap<String, String>) -> Result<String> {
    path.get("code")
        .filter(|code| crate::model::valid_code(code))
        .cloned()
        .ok_or(Error::NotFound)
}
fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}
fn event(value: &Value) -> Result<&str> {
    value
        .get("type")
        .and_then(Value::as_str)
        .ok_or(Error::Invalid)
}
/// One string of an event's payload, empty when absent.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}
/// An inline edit's version, as the fact carried it.
fn version(value: &Value) -> Result<i64> {
    text(value, "version").parse().map_err(|_| Error::Invalid)
}
/// The posted fields of one form namespace (`calendar[name]` → `name`).
fn form(value: &Value, namespace: &str) -> Result<HashMap<String, String>> {
    bilibi_ui::params::form_values_checked(value, namespace, bilibi_ui::params::FIELD_LIMIT)
        .map_err(Error::refused)
}
/// "3 November 2026".
fn long_date(date: Date) -> String {
    date.strftime("%-d %B %Y").to_string()
}

/// Parts one under another, a step apart (`ui::stack`).
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::stack(parts=parts, wide=wide) %}{% endcall %}"#,
    ext = "html"
)]
struct Stack {
    parts: Vec<Html>,
    wide: bool,
}
/// Controls on one line (`ui::cluster`).
#[derive(Template)]
#[template(
    source = r#"{% import "components.html" as ui %}{% call ui::cluster(parts=parts) %}{% endcall %}"#,
    ext = "html"
)]
struct Cluster {
    parts: Vec<Html>,
}
/// A page section: its heading, then its cards.
fn section(heading: bilibi_ui::SectionHeading, cards: Vec<Html>) -> Result<Html> {
    let mut parts = vec![render(heading)?];
    parts.extend(cards);
    render(Stack { parts, wide: false })
}

/// A dialog's values and the contract's refusals.
#[derive(Default)]
struct Draft {
    values: HashMap<String, String>,
    errors: BTreeMap<String, Vec<String>>,
}

/// The whole page around its sections.
#[derive(Template)]
#[template(path = "work_calendar_page.html")]
struct View<'a> {
    id: &'a str,
    variant: PageVariant,
    flash: FlashMap<'a>,
    record_kind: &'a str,
    record_id: &'a str,
    header: Html,
    sections: Vec<Html>,
    dialog: Html,
}
/// A form of the pages (a dialog's, or adding a holiday): its fields in
/// rows, the week editor when it has one, then its actions.
#[derive(Template, Default)]
#[template(path = "work_calendar_form.html")]
struct DialogForm<'a> {
    id: &'a str,
    event: &'a str,
    change: bool,
    submit: &'a str,
    rows: Vec<Vec<Html>>,
    week_heading: &'a str,
    week: Html,
    actions: Html,
}

/// The page's one flash message.
#[derive(Default)]
struct Flash(Option<(StatusKind, String)>);
impl Flash {
    fn put(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.0 = Some((kind, message.into()));
    }
    fn clear(&mut self) {
        self.0 = None;
    }
    fn dismiss(&mut self, key: &str) {
        if self.0.as_ref().is_some_and(|(kind, _)| kind.name() == key) {
            self.0 = None;
        }
    }
    fn map(&self) -> FlashMap<'_> {
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.0 {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        flash
    }
}

/// The body a live update answers with.
fn body(html: String, query: HashMap<String, String>, rows: usize, notice: &str) -> LiveBody {
    LiveBody {
        html,
        query,
        rows,
        notice: notice.to_owned(),
        ..LiveBody::default()
    }
}

/// An inline edit's outcome on its own fact. A forbidden write is the whole
/// outcome: the page says so and no fact says Saved.
fn commit_status(
    outcome: Outcome,
    label: &str,
    submitted: &str,
    noun: &str,
    notice: &mut String,
    statuses: &mut bilibi_ui::CommitStates,
) -> bilibi_ui::CommitStatus {
    use bilibi_ui::CommitStatus;
    match outcome {
        Ok(_) => CommitStatus::Saved,
        Err(Refusal::Invalid(fields)) => {
            let reasons: Vec<&str> = fields.values().flatten().map(String::as_str).collect();
            CommitStatus::refusal(label, submitted, &reasons)
        }
        Err(Refusal::Denied) => {
            *notice = DENIED.into();
            statuses.clear_saved();
            CommitStatus::Error(DENIED.into())
        }
        Err(refusal @ Refusal::Stale) => CommitStatus::Error(refusal.sentence(noun)),
        Err(Refusal::Failed) => CommitStatus::failure(),
    }
}
/// A dialog form's submit and Cancel.
fn dialog_actions(verb: &str, working: &str, cancel: &str) -> Result<Html> {
    let mut actions = render(bilibi_ui::Button {
        variant: bilibi_ui::ButtonVariant::Primary,
        button_type: "submit",
        working,
        body: Html::text(verb),
        ..bilibi_ui::Button::default()
    })?;
    actions.push(&render(bilibi_ui::Button {
        event: cancel,
        button_type: "button",
        body: Html::text("Cancel"),
        ..bilibi_ui::Button::default()
    })?);
    Ok(actions)
}
