//! `/system/software` (`base/software.pages`, operation `index`): the Software
//! page. Everything it shows comes from the module's own contracts through
//! `PageContracts` (`base.software.status` and `.catalogue` as reads; `.change`,
//! `.install`, `.undo` as reviewed plans committed with the owner's
//! authenticator code; `.cancel` as a plain submit), so the page and an agent
//! see the same facts and get the same refusals. No Bilimbi counterpart: the
//! Software page is Bilibi's own (the design is the Firstmate scout report
//! `bilibi-domain-chooser`).
//!
//! The page keeps only what the person is doing: the Domains ticked and the
//! versions chosen (Dev), the release number typed (Staging, Production), the
//! review being confirmed, and a flash. Every render reads the status again,
//! and a mounted page is woken by each job write through `follow.rs`, so the
//! builder's progress rows appear as they are written.
use crate::{follow::Follow, MANAGE};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_settings::Settings;
use bilibi_system::Instance;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest, Subscription,
    },
    params, Alert, Badge, BadgeKind, BuiltinColumn, Button, ButtonVariant, Card, Choice, CodeInput,
    ColumnType, ColumnsOutcome, ConfirmDialog, Fact, Facts, Field, FlashMap, FlexRow, FlexTable,
    Header, Html, IconButton, IconContext, InputKind, Instant, ListColumns, Modal, ModalWidth,
    RadioGroup, StatusKind, TableEmpty, NO_FLASH,
};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    sync::Arc,
    time::Duration,
};

pub const PATH: &str = "/system/software";
pub(crate) const PAGES: &str = "base/software.pages";
const DENIED: &str = "You do not have permission to change the software.";
const UNAVAILABLE: &str = "The software service did not answer. Nothing was queued; try again.";
/// How long a read catalogue is shown before it is read again.
const CATALOGUE_KEEP: Duration = Duration::from_secs(300);

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub contracts: Arc<PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
    pub follow: Arc<Follow>,
    pub software: Arc<crate::Software>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}
/// A read contract's answer, out of the door's envelope (`result`, `meta`).
async fn read(contracts: &PageContracts, scope: &Scope, key: &str) -> Result<Value> {
    let mut reply = contracts
        .read(scope, key, &json!({}))
        .await
        .map_err(backend)?;
    Ok(reply["result"].take())
}
/// A contract's refusal as the page's own error, for a read the route already
/// admitted: the owner's words never reach the page.
fn backend(error: ContractError) -> Error {
    match error {
        ContractError::Backend(error) => error,
        ContractError::Forbidden => Error::Forbidden,
        _ => Error::Internal,
    }
}
/// The first field message of a refused input, in the owner's words.
fn first_message(fields: &BTreeMap<String, Vec<String>>) -> String {
    fields
        .values()
        .flatten()
        .next()
        .cloned()
        .unwrap_or_else(|| "The request was not accepted.".into())
}
fn instant(display: &DisplayContext, at: &str) -> Option<Instant> {
    let at: Timestamp = at.parse().ok()?;
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Some(Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    ))
}
/// "2 min 5 s" between two recorded instants.
fn elapsed(from: &str, to: &str) -> Option<String> {
    let from: Timestamp = from.parse().ok()?;
    let to: Timestamp = to.parse().ok()?;
    let seconds = to.as_second() - from.as_second();
    Some(duration_text(seconds.max(0)))
}
fn duration_text(seconds: i64) -> String {
    if seconds < 60 {
        format!("{seconds} s")
    } else if seconds % 60 == 0 {
        format!("{} min", seconds / 60)
    } else {
        format!("{} min {} s", seconds / 60, seconds % 60)
    }
}
fn minutes_text(minutes: u64) -> String {
    if minutes == 1 {
        "about 1 minute".into()
    } else {
        format!("about {minutes} minutes")
    }
}
/// A Domain or plug-in id from the catalogue, safe inside a DOM id and a form
/// field name: the same rule the review applies to a request.
fn safe_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn dom(id: &str) -> String {
    id.replace('_', "-")
}
fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Kind {
    Change,
    Install,
    Undo,
    Go,
}
impl Kind {
    /// The page event the write is declared under (`PageWrites`).
    fn event(self) -> &'static str {
        match self {
            Self::Change => "build",
            Self::Install => "install",
            Self::Undo => "undo",
            Self::Go => "go",
        }
    }
    fn contract(self) -> &'static str {
        match self {
            Self::Change => "base.software.change",
            Self::Install => "base.software.install",
            Self::Undo => "base.software.undo",
            Self::Go => "base.software.go",
        }
    }
    fn parse(event: &str) -> Option<Self> {
        match event {
            "build" => Some(Self::Change),
            "install" => Some(Self::Install),
            "undo" => Some(Self::Undo),
            "go" => Some(Self::Go),
            _ => None,
        }
    }
}

/// A reviewed plan the person has not yet confirmed: exactly what the owner
/// previewed, and the code that commits it unchanged.
struct Review {
    kind: Kind,
    input: Value,
    plan_code: String,
    facts: Value,
    error: Option<String>,
}
enum Dialog {
    Review(Review),
    Cancel {
        job: i64,
        running: bool,
        change: bool,
    },
}

/// What the person has ticked and chosen on Dev.
#[derive(Default)]
struct Picks {
    ticked: BTreeSet<String>,
    versions: BTreeMap<String, String>,
    plugins: BTreeSet<String>,
    switch: String,
}

struct View {
    pages: Pages,
    picks: Picks,
    /// The picks start from what is installed, once the catalogue is read.
    picks_started: bool,
    /// Domains an agent proposed in the address (`?propose=finance,factory`):
    /// ticked when the picks start, chosen by nobody until Build is pressed.
    propose: Vec<String>,
    release: String,
    dialog: Option<Dialog>,
    flash: Option<(StatusKind, String)>,
    pick_error: Option<String>,
    install_error: Option<String>,
    columns: ListColumns,
    rows_changed: bool,
    catalogue: Option<(std::time::Instant, Value)>,
}

fn history_columns() -> Vec<BuiltinColumn> {
    let mut requested = BuiltinColumn::new("requested_at", "Requested");
    requested.kind = ColumnType::Datetime;
    let mut finished = BuiltinColumn::new("finished_at", "Finished");
    finished.kind = ColumnType::Datetime;
    vec![
        BuiltinColumn::new("id", "Job").right(),
        BuiltinColumn::new("kind", "What"),
        BuiltinColumn::new("state", "Outcome"),
        BuiltinColumn::new("summary", "Change"),
        requested,
        finished,
    ]
}

impl View {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        let propose = query
            .get("propose")
            .map(|list| {
                list.split(',')
                    .map(str::trim)
                    .filter(|id| safe_id(id))
                    .take(16)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            pages,
            picks: Picks {
                switch: "auto".into(),
                ..Picks::default()
            },
            picks_started: false,
            propose,
            release: String::new(),
            dialog: None,
            flash: None,
            pick_error: None,
            install_error: None,
            columns: ListColumns::mount("software-jobs", history_columns())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            catalogue: None,
        })
    }
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    fn call<'a>(&self, scope: &'a Scope, credential: &'a str, kind: Kind) -> Call<'a> {
        Call {
            scope,
            credential,
            service: PAGES,
            event: kind.event(),
            contract: kind.contract(),
        }
    }

    /// The published catalogue, read through the contract and kept for a while.
    async fn catalogue(&mut self, scope: &Scope) -> Result<Value> {
        if let Some((at, value)) = &self.catalogue {
            if at.elapsed() < CATALOGUE_KEEP {
                return Ok(value.clone());
            }
        }
        let value = read(&self.pages.contracts, scope, "base.software.catalogue").await?;
        self.catalogue = Some((std::time::Instant::now(), value.clone()));
        Ok(value)
    }

    /// The picks start as what runs now: every mounted Domain ticked at its
    /// installed version (or the newest release when that one is not in the
    /// catalogue), plus whatever the address proposed.
    fn start_picks(&mut self, status: &Value, catalogue: &Value) {
        if self.picks_started || catalogue["available"] != true {
            return;
        }
        self.picks_started = true;
        let installed: BTreeMap<&str, &str> = status["installed"]["domains"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|d| (text(&d["id"]), text(&d["version"])))
            .collect();
        for domain in catalogue["domains"].as_array().into_iter().flatten() {
            let id = text(&domain["id"]);
            if !safe_id(id) {
                continue;
            }
            let versions: Vec<&str> = domain["versions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| text(&v["version"]))
                .collect();
            let Some(newest) = newest(&versions) else {
                continue;
            };
            let chosen = match installed.get(id) {
                Some(version) if versions.contains(version) => version,
                _ => newest,
            };
            self.picks.versions.insert(id.to_owned(), chosen.to_owned());
            if installed.contains_key(id) || self.propose.iter().any(|p| p == id) {
                self.picks.ticked.insert(id.to_owned());
            }
        }
        for plugin in status["plugins"].as_array().into_iter().flatten() {
            if plugin["enabled"] == true {
                self.picks.plugins.insert(text(&plugin["id"]).to_owned());
            }
        }
    }

    /// The change request as the contract takes it: every Domain the server
    /// should have afterwards, and every catalogue plug-in's state.
    fn change_input(&self, catalogue: &Value) -> Value {
        let domains: Vec<Value> = self
            .picks
            .ticked
            .iter()
            .filter_map(|id| {
                self.picks
                    .versions
                    .get(id)
                    .map(|version| json!({"id": id, "version": version}))
            })
            .collect();
        let plugins: Vec<Value> = catalogue["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| text(&p["id"]))
            .filter(|id| safe_id(id))
            .map(|id| json!({"id": id, "enabled": self.picks.plugins.contains(id)}))
            .collect();
        let mut input = json!({"domains": domains, "switch": self.picks.switch});
        if !plugins.is_empty() {
            input["plugins"] = json!(plugins);
        }
        input
    }

    async fn event(
        &mut self,
        scope: &Scope,
        credential: &str,
        value: &Value,
        status: &Value,
        catalogue: &Value,
    ) -> Result<()> {
        let values = params::string_values(value);
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "pick" => self.pick(&values, catalogue),
            "reset_picks" => {
                self.picks_started = false;
                self.picks.ticked.clear();
                self.picks.plugins.clear();
                self.pick_error = None;
                self.start_picks(status, catalogue);
            }
            "reload_catalogue" => self.catalogue = None,
            "review" => {
                let input = self.change_input(catalogue);
                self.preview(scope, credential, Kind::Change, input).await?
            }
            "request_install" => {
                self.release = values.get("release").cloned().unwrap_or_default();
                match self.release.trim().parse::<i64>() {
                    Ok(release) if release >= 1 => {
                        self.install_error = None;
                        let input = json!({"release": release, "switch": "auto"});
                        self.preview(scope, credential, Kind::Install, input)
                            .await?
                    }
                    _ => self.install_error = Some(
                        "Enter the release number the source instance reports as tested, like 42."
                            .into(),
                    ),
                }
            }
            "request_undo" => {
                self.preview(scope, credential, Kind::Undo, json!({}))
                    .await?
            }
            "request_go" => {
                let job = &status["job"];
                if let Some(id) = job["id"].as_i64() {
                    if matches!(text(&job["kind"]), "change" | "install") && job["switch"] == "ask"
                    {
                        self.preview(scope, credential, Kind::Go, json!({"job": id}))
                            .await?
                    }
                }
            }
            "cancel_dialog" => self.dialog = None,
            "build" | "install" | "undo" | "go" => {
                let code = values.get("code").cloned().unwrap_or_default();
                self.commit(scope, credential, get("type").unwrap_or_default(), &code)
                    .await?
            }
            "request_cancel" => {
                let job = &status["job"];
                if let Some(id) = job["id"].as_i64() {
                    if job["cancel"] == "available" {
                        self.dialog = Some(Dialog::Cancel {
                            job: id,
                            running: job["state"] == "running",
                            change: job["kind"] == "change",
                        });
                    }
                }
            }
            "cancel_job" => {
                let Some(Dialog::Cancel { job, .. }) = self.dialog.take() else {
                    return Ok(());
                };
                let call = Call {
                    scope,
                    credential,
                    service: PAGES,
                    event: "cancel_job",
                    contract: "base.software.cancel",
                };
                match self
                    .pages
                    .contracts
                    .submit(&call, json!({"job": job}))
                    .await
                {
                    Ok(reply) => {
                        let state = text(&reply["result"]["state"]);
                        let message = if state == "cancelled" {
                            format!("Job {job} was cancelled. Nothing has changed.")
                        } else {
                            format!("Job {job} was asked to stop. It stops before the pause, so nothing changes.")
                        };
                        self.flash(StatusKind::Success, message);
                    }
                    Err(ContractError::Backend(
                        error @ (Error::StepUpRequired { .. } | Error::Unauthorized),
                    )) => return Err(error),
                    Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                        self.flash(StatusKind::Error, DENIED)
                    }
                    Err(ContractError::InvalidInput(fields)) => {
                        self.flash(StatusKind::Error, first_message(&fields))
                    }
                    Err(_) => self.flash(StatusKind::Error, UNAVAILABLE),
                }
            }
            "grid" => match self.columns.handle(&values) {
                ColumnsOutcome::Sort(_) => return Err(Error::Invalid),
                ColumnsOutcome::Update | ColumnsOutcome::Noop => self.rows_changed = false,
            },
            "clear-flash" => {
                if self
                    .flash
                    .as_ref()
                    .is_some_and(|(kind, _)| Some(kind.name()) == get("key"))
                {
                    self.flash = None;
                }
            }
            _ => return Err(Error::Invalid),
        }
        Ok(())
    }

    /// The picks form as its values change: ticks, versions, plug-ins, switch.
    fn pick(&mut self, values: &HashMap<String, String>, catalogue: &Value) {
        self.pick_error = None;
        for domain in catalogue["domains"].as_array().into_iter().flatten() {
            let id = text(&domain["id"]);
            if !safe_id(id) {
                continue;
            }
            match values.get(&format!("domain_{id}")).map(String::as_str) {
                Some("true") => {
                    self.picks.ticked.insert(id.to_owned());
                }
                Some(_) => {
                    self.picks.ticked.remove(id);
                }
                None => {}
            }
            if let Some(version) = values.get(&format!("version_{id}")) {
                let known = domain["versions"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|v| text(&v["version"]) == version);
                if known {
                    self.picks.versions.insert(id.to_owned(), version.clone());
                }
            }
        }
        for plugin in catalogue["plugins"].as_array().into_iter().flatten() {
            let id = text(&plugin["id"]);
            if !safe_id(id) {
                continue;
            }
            match values.get(&format!("plugin_{id}")).map(String::as_str) {
                Some("true") => {
                    self.picks.plugins.insert(id.to_owned());
                }
                Some(_) => {
                    self.picks.plugins.remove(id);
                }
                None => {}
            }
        }
        if let Some(switch) = values.get("switch") {
            if switch == "auto" || switch == "ask" {
                self.picks.switch = switch.clone();
            }
        }
    }

    /// The owner's review of a request, held with its plan code until the
    /// person confirms it with their authenticator code.
    async fn preview(
        &mut self,
        scope: &Scope,
        credential: &str,
        kind: Kind,
        input: Value,
    ) -> Result<()> {
        let call = self.call(scope, credential, kind);
        let refused = |this: &mut Self, message: String| match kind {
            Kind::Change => this.pick_error = Some(message),
            Kind::Install => this.install_error = Some(message),
            Kind::Undo | Kind::Go => this.flash(StatusKind::Error, message),
        };
        match self.pages.contracts.preview(&call, input.clone()).await {
            Ok(reply) => {
                let result = &reply["results"][0];
                if reply["valid"] == true && result["valid"] == true {
                    let preview = &result["preview"];
                    // The owner's facts, whichever envelope the door wraps them in.
                    let facts =
                        if preview.get("summary").is_none() && preview.get("result").is_some() {
                            preview["result"].clone()
                        } else {
                            preview.clone()
                        };
                    self.dialog = Some(Dialog::Review(Review {
                        kind,
                        input,
                        plan_code: text(&reply["plan_code"]).to_owned(),
                        facts,
                        error: None,
                    }));
                } else {
                    let fields: BTreeMap<String, Vec<String>> =
                        serde_json::from_value(result["error"]["fields"].clone())
                            .unwrap_or_default();
                    let message = if fields.is_empty() {
                        match text(&result["error"]["code"]) {
                            "missing_capability" => DENIED.to_owned(),
                            _ => "The request was not accepted.".to_owned(),
                        }
                    } else {
                        first_message(&fields)
                    };
                    refused(self, message);
                }
            }
            Err(ContractError::Backend(
                error @ (Error::StepUpRequired { .. } | Error::Unauthorized),
            )) => return Err(error),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.flash(StatusKind::Error, DENIED)
            }
            Err(ContractError::InvalidInput(fields)) => refused(self, first_message(&fields)),
            Err(_) => self.flash(StatusKind::Error, UNAVAILABLE),
        }
        Ok(())
    }

    /// Commits the reviewed plan with the owner's fresh authenticator code
    /// presented on the request, as an agent's `step_up_code` is: the owner's
    /// facade spends it inside the commit (`.always()`), so a wrong code is
    /// an error on this dialog, not a trip through the browser prompt.
    async fn commit(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &str,
        code: &str,
    ) -> Result<()> {
        let Some(Dialog::Review(review)) = &mut self.dialog else {
            return Ok(());
        };
        if Kind::parse(event) != Some(review.kind) {
            return Err(Error::Invalid);
        }
        let code = code.trim();
        if code.is_empty() {
            review.error = Some("Enter the 6-digit code from your authenticator app.".into());
            return Ok(());
        }
        let presented = scope.clone().presenting_step_up(code);
        let kind = review.kind;
        let call = Call {
            scope: &presented,
            credential,
            service: PAGES,
            event: kind.event(),
            contract: kind.contract(),
        };
        let outcome = self
            .pages
            .contracts
            .commit(&call, &review.plan_code, review.input.clone())
            .await;
        let label = text(&review.facts["instance"]["label"]).to_owned();
        match outcome {
            Ok(reply) => {
                let job = reply["results"][0]["result"]["result"]["id"].as_i64();
                let what = match kind {
                    Kind::Change => "The build",
                    Kind::Install => "The install",
                    Kind::Undo => "The undo",
                    Kind::Go => "",
                };
                let message = match (kind, job) {
                    (Kind::Go, Some(id)) => format!("Job {id} has your go-ahead. {label} pauses for about a minute, then switches. Its progress shows below."),
                    (Kind::Go, None) => format!("The go-ahead is given on {label}."),
                    (_, Some(id)) => format!("{what} is queued as job {id} on {label}. Its progress shows below, and the result also reaches your notifications."),
                    (_, None) => format!("{what} is queued on {label}."),
                };
                self.dialog = None;
                self.flash(StatusKind::Success, message);
            }
            Err(ContractError::Backend(Error::StepUpRequired { reason, operation })) => {
                let message = match reason {
                    // Enrolment is the host's prompt: it enrols and comes back.
                    "not_enrolled" => {
                        return Err(Error::StepUpRequired { reason, operation });
                    }
                    "invalid_code" => "That code was not accepted. Enter the current code from your authenticator app.",
                    "locked" => "Too many wrong codes. Wait a while, then try again with a fresh code.",
                    "impersonating" => "The software cannot be changed while viewing as someone else.",
                    "unavailable" => "No authenticator is set up on this server, so the software cannot be changed.",
                    _ => "Enter the current 6-digit code from your authenticator app.",
                };
                review.error = Some(message.into());
            }
            Err(ContractError::Backend(Error::Unauthorized)) => return Err(Error::Unauthorized),
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.dialog = None;
                self.flash(StatusKind::Error, DENIED);
            }
            Err(ContractError::InvalidInput(fields)) => {
                review.error = Some(first_message(&fields));
            }
            Err(ContractError::PlanRejected(results)) => {
                let fields: BTreeMap<String, Vec<String>> =
                    serde_json::from_value(results[0]["error"]["fields"].clone())
                        .unwrap_or_default();
                review.error = Some(if fields.is_empty() {
                    "The request was not accepted. Close this and look at the page again.".into()
                } else {
                    first_message(&fields)
                });
            }
            Err(ContractError::Backend(Error::Conflict)) => {
                review.error = Some(if kind == Kind::Go {
                    "The job changed in the meantime (it was stopped, or already has its go-ahead). Close this and look at the page again.".into()
                } else {
                    "Another job was queued in the meantime. Close this and look at the page again."
                        .into()
                });
            }
            Err(
                ContractError::PlanExpired
                | ContractError::StalePlan
                | ContractError::PlanAlreadyUsed
                | ContractError::InvalidPlanCode
                | ContractError::PlanMismatch,
            ) => {
                self.dialog = None;
                self.flash(
                    StatusKind::Error,
                    "The review is no longer current. Nothing was queued; review the change again.",
                );
            }
            Err(_) => review.error = Some(UNAVAILABLE.into()),
        }
        Ok(())
    }

    async fn render(&mut self, scope: &Scope) -> Result<String> {
        let pages = self.pages.clone();
        let client = pages.db.get().await?;
        let locale = pages.locales.resolve(&**client, scope, None).await?.locale;
        let display = pages
            .display
            .display(&**client, scope, None, locale)
            .await?;
        let instance = Instance::resolve(&pages.settings, &**client).await?;
        let can_manage = effective_capabilities(&**client, scope)
            .await?
            .contains(MANAGE);
        let status = read(&pages.contracts, scope, "base.software.status").await?;
        let builds = instance.role.builds();
        let catalogue = if builds && can_manage {
            let catalogue = self.catalogue(scope).await?;
            self.start_picks(&status, &catalogue);
            Some(catalogue)
        } else {
            None
        };
        let lost = match &self.dialog {
            Some(Dialog::Review(review)) if review.kind == Kind::Undo => {
                match review.facts["job"].as_i64() {
                    Some(job) => pages.software.undo_loss(scope, job).await?["count"].as_u64(),
                    None => None,
                }
            }
            _ => None,
        };
        self.page(
            &display,
            &instance,
            can_manage,
            &status,
            catalogue.as_ref(),
            lost,
        )
    }

    fn page(
        &mut self,
        display: &DisplayContext,
        instance: &Instance,
        can_manage: bool,
        status: &Value,
        catalogue: Option<&Value>,
        lost: Option<u64>,
    ) -> Result<String> {
        let builds = instance.role.builds();
        let label = instance.label.clone();
        let subtitle = if !can_manage {
            "What this Bilibi is made of. Only a server owner can change it.".to_owned()
        } else if builds {
            "Choose the Domains this Bilibi is built with. Only Dev builds; Staging and Production install what Dev tested.".to_owned()
        } else {
            format!("{label} installs a release that another instance tested. It never builds.")
        };
        let header = render(Header {
            title: Html::text("Software"),
            subtitle: &subtitle,
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "software-pin",
                nav_pin: "nav-admin-system-software",
                ..IconButton::new("bilimbi-pin", "Pin Software to sidebar")
            })?,
            ..Header::default()
        })?;
        let banner = instance
            .banner("software-instance", "mb-4")
            .map_err(|_| Error::Internal)?;
        let instance_card = self.instance_card(instance, status)?;
        let active = status["job"]["state"] == "queued" || status["job"]["state"] == "running";
        let job_card = if status["job"].is_null() {
            Html::EMPTY
        } else {
            self.job_card(display, instance, can_manage, status)?
        };
        let change_card = if !can_manage {
            Html::EMPTY
        } else if builds {
            match catalogue {
                Some(catalogue) => self.domains_card(instance, status, catalogue, active)?,
                None => Html::EMPTY,
            }
        } else {
            self.install_card(instance, status, active)?
        };
        let history_card = self.history_card(display, status)?;
        let dialog = match &self.dialog {
            Some(Dialog::Review(review)) => self.review_dialog(instance, lost, review)?,
            Some(Dialog::Cancel {
                job,
                running,
                change,
            }) => {
                let consequence = if *running {
                    format!("Stop job {job}?")
                } else {
                    format!("Cancel job {job} before it starts?")
                };
                let detail = if *running && *change {
                    format!("Nothing has changed yet, so stopping is safe: {label} keeps running as it is. Parts already compiled are kept, so the next build is quicker.")
                } else if *running {
                    format!("Nothing has changed yet, so stopping is safe: {label} keeps running as it is.")
                } else {
                    "It is removed from the queue. Nothing has changed.".to_owned()
                };
                render(ConfirmDialog {
                    id: "software-cancel-confirm",
                    consequence: &consequence,
                    detail: &detail,
                    confirm: if *running { "Stop" } else { "Cancel the job" },
                    working: "Stopping…",
                    confirm_event: "cancel_job",
                    cancel_event: "cancel_dialog",
                })?
            }
            None => Html::EMPTY,
        };
        let mut flash = NO_FLASH;
        if let Some((kind, message)) = &self.flash {
            match kind {
                StatusKind::Success => flash.success = message,
                StatusKind::Error => flash.error = message,
                StatusKind::Warning => flash.warning = message,
                StatusKind::Info => flash.info = message,
            }
        }
        PageView {
            flash,
            header,
            banner,
            instance_card,
            job_card,
            change_card,
            history_card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }

    /// What runs now, from the program itself.
    fn instance_card(&self, instance: &Instance, status: &Value) -> Result<Html> {
        let installed = &status["installed"];
        let role = format!(
            "{} · {}",
            instance.label,
            if instance.role.builds() {
                "builds software".to_owned()
            } else {
                match &status["instance"]["installs_from"] {
                    Value::Null => "installs tested releases; no source set yet".to_owned(),
                    source if source["kind"] == "same_machine" => format!(
                        "installs releases tested on {} (this machine)",
                        text(&source["instance"])
                    ),
                    source => format!("installs releases tested on {}", text(&source["address"])),
                }
            }
        );
        let platform = &installed["platform"];
        let mut bilibi = format!("Bilibi {}", text(&platform["version"]));
        if let Some(release) = platform["release"].as_str() {
            bilibi.push_str(&format!(" · release {release}"));
        }
        if let Some(commit) = platform["commit"].as_str() {
            bilibi.push_str(&format!(" · {}", short_commit(commit)));
        }
        let pin = match text(&platform["pin"]) {
            "pinned" => "Exact versions are known: the program carries its pin.".to_owned(),
            "stale" => format!(
                "The pin is stale, so the versions are not known exactly: {}",
                text(&platform["pin_reason"])
            ),
            _ => "Built without a pin, so the versions it holds are not known exactly.".to_owned(),
        };
        let domains: Vec<String> = installed["domains"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|d| match d["version"].as_str() {
                Some(version) => format!("{} {version}", text(&d["id"])),
                None => format!("{} (version unknown)", text(&d["id"])),
            })
            .collect();
        let domains = if domains.is_empty() {
            "None".to_owned()
        } else {
            domains.join(", ")
        };
        let plugins: Vec<String> = status["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["enabled"] == true)
            .map(|p| text(&p["name"]).to_owned())
            .collect();
        let plugins = if plugins.is_empty() {
            "None".to_owned()
        } else {
            plugins.join(", ")
        };
        let facts = render(Facts {
            id: "software-instance-facts",
            items: vec![
                Fact {
                    title: "This instance",
                    id: "software-fact-role",
                    value: Html::text(&role),
                },
                Fact {
                    title: "Running now",
                    id: "software-fact-platform",
                    value: Html::text(&bilibi),
                },
                Fact {
                    title: "Domains",
                    id: "software-fact-domains",
                    value: Html::text(&domains),
                },
                Fact {
                    title: "Plug-ins on",
                    id: "software-fact-plugins",
                    value: Html::text(&plugins),
                },
                Fact {
                    title: "Versions",
                    id: "software-fact-pin",
                    value: Html::text(&pin),
                },
            ],
        })?;
        render(Card {
            id: "software-running",
            title: "What runs here",
            body: facts,
            ..Card::default()
        })
    }

    /// The job waiting, running or last finished, with its steps as the
    /// worker reports them.
    fn job_card(
        &self,
        display: &DisplayContext,
        instance: &Instance,
        can_manage: bool,
        status: &Value,
    ) -> Result<Html> {
        let job = &status["job"];
        let id = job["id"].as_i64().unwrap_or_default();
        let kind = text(&job["kind"]);
        let state = text(&job["state"]);
        let steps_json = job["steps"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        let first = if kind == "install" { "fetch" } else { "build" };
        let part = steps_json
            .iter()
            .find(|s| s["key"] == first && s["state"] == "done");
        let built = if kind == "install" {
            part.and(job["request"]["release"].as_i64())
        } else {
            part.and_then(|s| s["detail"]["release"].as_i64())
        };
        let installer_started = steps_json
            .iter()
            .skip_while(|s| s["key"] != first)
            .skip(1)
            .any(|s| s["state"] != "pending");
        let building_done = kind == "change" && state == "running" && built.is_some();
        let parked = matches!(kind, "change" | "install")
            && state == "running"
            && built.is_some()
            && !installer_started;
        let awaiting_go = parked && job["switch"] == "ask";
        let handed_over = awaiting_go || (building_done && !installer_started);
        let ready = if kind == "install" { "copied" } else { "built" };
        let label = job["review"]["instance"]["label"]
            .as_str()
            .unwrap_or(&instance.label);
        let nothing_changed =
            job["result"]["migrated"] != true && job["result"]["release"].is_null();
        let unchanged = if nothing_changed {
            ". Nothing changed"
        } else {
            ""
        };
        let doing = match kind {
            "change" => "Building",
            "install" => "Installing",
            _ => "Undoing",
        };
        let heading = match state {
            "queued" => format!(
                "Job {id} is waiting for the {}",
                if kind == "change" {
                    "builder"
                } else {
                    "installer"
                }
            ),
            "running" if handed_over => format!(
                "Release {} is {ready} and waiting to be installed",
                built.unwrap_or_default()
            ),
            "running" if building_done => format!(
                "Installing release {} on {label}…",
                built.unwrap_or_default()
            ),
            // The job's own review names the instance it was asked on.
            "running" => format!("{doing} on {label}…"),
            "done" => format!("Job {id} finished"),
            "failed" => format!("Job {id} failed{unchanged}"),
            "cancelled" => format!("Job {id} was stopped. Nothing changed"),
            "interrupted" => format!("Job {id} was interrupted{unchanged}"),
            other => format!("Job {id} is {other}"),
        };
        let summary = text(&job["review"]["summary"]).to_owned();
        let requested = instant(display, text(&job["requested_at"]));
        let finished = job["finished_at"].as_str();
        let active = state == "queued" || state == "running";
        let mut steps = Vec::with_capacity(steps_json.len());
        for step in steps_json {
            let step_state = text(&step["state"]);
            let detail = &step["detail"];
            let mut message = text(&detail["message"]).to_owned();
            if message.is_empty() && step_state == "skipped" {
                message = "Skipped".into();
            }
            let percent = match (detail["compiled"].as_u64(), detail["total"].as_u64()) {
                (Some(done), Some(total)) if total > 0 && step_state == "running" => {
                    Some(((done * 100) / total).min(100) as u8)
                }
                _ => None,
            };
            let took = match (step["started_at"].as_str(), step["finished_at"].as_str()) {
                (Some(from), Some(to)) if step_state == "done" || step_state == "failed" => {
                    elapsed(from, to)
                }
                _ => None,
            };
            let developer = detail["details"]
                .as_str()
                .filter(|d| !d.trim().is_empty())
                .map(str::to_owned);
            steps.push(StepView {
                id: format!("software-step-{}", text(&step["key"])),
                label: text(&step["label"]).to_owned(),
                state: step_state.to_owned(),
                icon: match step_state {
                    "done" => "hero-check-circle",
                    "failed" => "hero-x-circle",
                    "running" => "hero-arrow-path",
                    "skipped" => "hero-minus-circle",
                    _ => "hero-ellipsis-horizontal-circle",
                },
                message,
                percent,
                took,
                developer,
            });
        }
        let result = &job["result"];
        let result_message = text(&result["message"]).to_owned();
        let result_kind = match state {
            "done" => Some(StatusKind::Success),
            "failed" => Some(StatusKind::Error),
            "cancelled" | "interrupted" => Some(StatusKind::Warning),
            _ => None,
        };
        let mut notes = Vec::new();
        if active && !handed_over {
            notes.push("You can close this page. The work keeps going, and the result appears here and in your notifications.".to_owned());
        }
        if awaiting_go {
            notes.push(format!(
                "Release {} is {ready} and waits for your go-ahead: Install now, or Stop to {}.",
                built.unwrap_or_default(),
                if kind == "install" {
                    "cancel the install"
                } else {
                    "throw the build away"
                }
            ));
        } else if handed_over {
            notes.push(format!(
                "The new program is in releases/{} on {}. Installing it (back up, update the database, switch, check) is the installer's part; until it is in service, the job can still be cancelled.",
                built.unwrap_or_default(),
                instance.label
            ));
        }
        let undo = &status["undo"];
        let undo_offered =
            can_manage && undo["available"] == true && undo["job"].as_i64() == Some(id);
        if state == "done" {
            match kind {
                "change" if instance.role.builds() => {
                    notes.push("Next: try it here, then install it on Staging.".to_owned());
                }
                "install" => {
                    if instance.role == bilibi_system::Role::Staging {
                        notes.push("Next: try it here.".to_owned());
                    }
                    if undo_offered {
                        notes.push(
                            "Done. Undo is available below if something is wrong.".to_owned(),
                        );
                    }
                }
                _ => {}
            }
        }
        match text(&job["cancel"]) {
            "requested" => notes.push("Asked to stop: the worker stops before the pause.".to_owned()),
            "too_late" => notes.push(
                "Past the pause, so it can no longer be stopped. If the check fails it switches back by itself."
                    .to_owned(),
            ),
            _ => {}
        }
        let mut actions = Html::EMPTY;
        if can_manage && job["cancel"] == "available" {
            let label = if (awaiting_go && kind == "change")
                || (state == "running" && kind == "change" && !handed_over)
            {
                "Stop the build"
            } else if state == "running" && (!handed_over || awaiting_go) {
                "Stop"
            } else {
                "Cancel"
            };
            actions.push(&render(Button {
                id: "software-cancel",
                button_type: "button",
                event: "request_cancel",
                body: Html::text(label),
                ..Button::default()
            })?);
        }
        if can_manage && awaiting_go {
            actions.push(&render(Button {
                id: "software-go",
                button_type: "button",
                event: "request_go",
                variant: ButtonVariant::Primary,
                body: Html::text("Install now (pauses about 1 minute)"),
                ..Button::default()
            })?);
        }
        if undo_offered {
            actions.push(&render(Button {
                id: "software-undo",
                button_type: "button",
                event: "request_undo",
                body: Html::text("Undo"),
                ..Button::default()
            })?);
        }
        let body = render(JobBody {
            id,
            heading,
            summary,
            requested,
            finished: finished.and_then(|at| instant(display, at)),
            worker: job["worker"].as_str().map(str::to_owned),
            steps,
            result_kind,
            result_message,
            notes,
            actions,
        })?;
        let title = match state {
            "queued" | "running" => "Now",
            _ => "Last job",
        };
        render(Card {
            id: "software-job",
            title,
            body,
            ..Card::default()
        })
    }

    /// Dev: the catalogue's Domains to tick and version, then Review.
    fn domains_card(
        &self,
        instance: &Instance,
        status: &Value,
        catalogue: &Value,
        busy: bool,
    ) -> Result<Html> {
        if catalogue["available"] != true {
            let message = format!(
                "The catalogue could not be read, so nothing can be chosen: {}",
                text(&catalogue["message"])
            );
            let body = render(CatalogueUnavailable {
                message: &message,
                url: text(&catalogue["url"]),
                retry: render(Button {
                    id: "software-reload-catalogue",
                    button_type: "button",
                    event: "reload_catalogue",
                    body: Html::text("Try again"),
                    ..Button::default()
                })?,
            })?;
            return render(Card {
                id: "software-domains",
                title: "Domains",
                body,
                ..Card::default()
            });
        }
        let installed: BTreeMap<&str, &str> = status["installed"]["domains"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|d| (text(&d["id"]), text(&d["version"])))
            .collect();
        let running_platform = text(&status["installed"]["platform"]["version"]);
        let mut rows = Vec::new();
        let mut changes = 0;
        let mut in_catalogue = BTreeSet::new();
        for domain in catalogue["domains"].as_array().into_iter().flatten() {
            let id = text(&domain["id"]);
            if !safe_id(id) {
                continue;
            }
            in_catalogue.insert(id);
            let ticked = self.picks.ticked.contains(id);
            let chosen = self.picks.versions.get(id).cloned().unwrap_or_default();
            let was = installed.get(id).copied();
            let action = match (was, ticked) {
                (None, true) => Some((BadgeKind::Success, "Add")),
                (Some(_), false) => Some((BadgeKind::Warning, "Remove")),
                (Some(from), true) if from != chosen => Some(match movement(from, &chosen) {
                    "downgrade" => (BadgeKind::Warning, "Go back"),
                    _ => (BadgeKind::Success, "Update"),
                }),
                _ => None,
            };
            if action.is_some() {
                changes += 1;
            }
            let options: Vec<Choice> = domain["versions"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|v| {
                    let version = text(&v["version"]);
                    let tested = v["tested_on"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|p| text(p) == running_platform);
                    let mut label = version.to_owned();
                    if Some(version) == was {
                        label.push_str(" (installed)");
                    }
                    if tested {
                        label.push_str(&format!(" · tested with Bilibi {running_platform}"));
                    }
                    Choice::new(label, version)
                })
                .collect();
            let version = Field::named(format!("version_{id}"))
                .id(format!("software-version-{}", dom(id)))
                .label("Version")
                .kind(InputKind::Select)
                .options(options)
                .value(chosen.clone())
                .wrapper_class("mb-0 w-full sm:w-72");
            let version = if busy { version.disabled() } else { version };
            let tick = Field::named(format!("domain_{id}"))
                .id(format!("software-domain-{}", dom(id)))
                .label(text(&domain["name"]))
                .kind(InputKind::Checkbox)
                .checked(ticked)
                .wrapper_class("mb-0");
            let tick = if busy { tick.disabled() } else { tick };
            let modules: Vec<&str> = domain["modules"]
                .as_array()
                .into_iter()
                .flatten()
                .map(text)
                .collect();
            let plugins: Vec<Field> = catalogue["plugins"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|p| p["domain"] == id && safe_id(text(&p["id"])))
                .map(|p| {
                    let pid = text(&p["id"]);
                    let mut label = text(&p["name"]).to_owned();
                    if let Some(publisher) = p["publisher"].as_str() {
                        label.push_str(&format!(" · by {publisher}"));
                    }
                    let field = Field::named(format!("plugin_{pid}"))
                        .id(format!("software-plugin-{}", dom(pid)))
                        .label(label)
                        .kind(InputKind::Checkbox)
                        .checked(self.picks.plugins.contains(pid))
                        .wrapper_class("mb-0");
                    if busy {
                        field.disabled()
                    } else {
                        field
                    }
                })
                .collect();
            rows.push(DomainRow {
                id: dom(id),
                tick,
                description: text(&domain["description"]).to_owned(),
                modules: modules.join(", "),
                installed: was.map_or_else(|| "Not installed".to_owned(), |v| format!("Installed {v}")),
                action: action.map(|(kind, label)| Badge::status(kind, label)),
                version,
                remove_note: (was.is_some() && !ticked).then(|| {
                    format!(
                        "{}'s pages leave the menu. Its data stays in the database and comes back if you add it again.",
                        text(&domain["name"])
                    )
                }),
                plugins,
            });
        }
        // Mounted Domains the catalogue does not list: a build from the
        // catalogue leaves them out, and the review says so as "remove".
        let outside: Vec<String> = installed
            .iter()
            .filter(|(id, _)| !in_catalogue.contains(*id))
            .map(|(id, version)| format!("{id} {version}"))
            .collect();
        changes += outside.len();
        let enabled: BTreeSet<&str> = status["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["enabled"] == true)
            .map(|p| text(&p["id"]))
            .collect();
        changes += catalogue["plugins"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| text(&p["id"]))
            .filter(|id| safe_id(id) && self.picks.plugins.contains(*id) != enabled.contains(id))
            .count();
        let switch = RadioGroup::new(
            "software-switch",
            "switch",
            vec![
                Choice::new(
                    format!("Switch {} as soon as the build is ready", instance.label),
                    "auto",
                ),
                Choice::new("Build now, and ask me before the 1-minute pause", "ask"),
            ],
        )
        .label("When the build is ready")
        .value(self.picks.switch.clone())
        .wrapper_class("mb-0");
        let switch = if busy { switch.disabled() } else { switch };
        let summary = if busy {
            "A job is waiting or running. Wait for it to finish, or stop it, before choosing a new change.".to_owned()
        } else if changes == 0 {
            "Tick or untick a Domain or plug-in, or pick another version, to change what is installed."
                .to_owned()
        } else if changes == 1 {
            "1 change. Review it before anything is built.".to_owned()
        } else {
            format!("{changes} changes. Review them before anything is built.")
        };
        let review = render(Button {
            id: "software-review",
            button_type: "button",
            variant: ButtonVariant::Primary,
            event: "review",
            disabled: busy || changes == 0,
            working: "Reviewing…",
            body: Html::text("Review changes"),
            ..Button::default()
        })?;
        let reset = render(Button {
            id: "software-reset",
            button_type: "button",
            event: "reset_picks",
            disabled: busy,
            body: Html::text("Start over"),
            ..Button::default()
        })?;
        let error = match &self.pick_error {
            Some(message) => render(Alert {
                kind: StatusKind::Error,
                id: "software-pick-error",
                class: "mb-3",
                body: Html::text(message),
            })?,
            None => Html::EMPTY,
        };
        let body = render(DomainsForm {
            rows,
            outside,
            switch,
            summary,
            review,
            reset,
            error,
        })?;
        render(Card {
            id: "software-domains",
            title: "Domains",
            body,
            ..Card::default()
        })
    }

    /// Staging and Production: install the release the source tested. Build
    /// is never offered here.
    fn install_card(&self, instance: &Instance, status: &Value, busy: bool) -> Result<Html> {
        let source = &status["instance"]["installs_from"];
        let source_text = match source {
            Value::Null => None,
            s if s["kind"] == "same_machine" => {
                Some(format!("{} on this machine", text(&s["instance"])))
            }
            s => Some(text(&s["address"]).to_owned()),
        };
        let release = Field::named("release")
            .id("software-release")
            .label("Release number tested on the source")
            .kind(InputKind::Number)
            .value(self.release.clone())
            .hint("The build number the source instance's Software page shows as finished.")
            .wrapper_class("mb-0 w-full sm:w-72")
            .attr("min", "1");
        let release = match &self.install_error {
            Some(message) => release.error(message.clone()),
            None => release,
        };
        let release = if busy || source_text.is_none() {
            release.disabled()
        } else {
            release
        };
        let review = render(Button {
            id: "software-review-install",
            button_type: "submit",
            variant: ButtonVariant::Primary,
            disabled: busy || source_text.is_none(),
            working: "Reviewing…",
            body: Html::text("Review the install"),
            ..Button::default()
        })?;
        let body = render(InstallForm {
            label: &instance.label,
            source: source_text.as_deref(),
            release,
            review,
            busy,
            production: instance.role == bilibi_system::Role::Production,
        })?;
        render(Card {
            id: "software-install",
            title: "Install a tested release",
            body,
            ..Card::default()
        })
    }

    fn history_card(&mut self, display: &DisplayContext, status: &Value) -> Result<Html> {
        let jobs = status["jobs"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default();
        if self.rows_changed {
            self.columns.load(jobs, |job| {
                job["id"].as_i64().unwrap_or_default().to_string()
            });
        }
        let mut rows = Vec::with_capacity(jobs.len());
        for job in jobs {
            let id = job["id"].as_i64().unwrap_or_default();
            let state = text(&job["state"]);
            let badge = match state {
                "done" => Badge::status(BadgeKind::Success, "Done"),
                "failed" => Badge::status(BadgeKind::Danger, "Failed"),
                "cancelled" => Badge::status(BadgeKind::Neutral, "Stopped"),
                "interrupted" => Badge::status(BadgeKind::Warning, "Interrupted"),
                "running" => Badge::status(BadgeKind::Warning, "Running"),
                "queued" => Badge::status(BadgeKind::Neutral, "Waiting"),
                other => Badge::status(BadgeKind::Neutral, other),
            };
            let kind = match text(&job["kind"]) {
                "change" => "Build",
                "install" => "Install",
                "undo" => "Undo",
                other => other,
            };
            let when = |key: &str, cell: &str| -> Result<Html> {
                match job[key].as_str().and_then(|at| instant(display, at)) {
                    Some(i) => render(InstantCell {
                        id: format!("software-job-{id}-{cell}"),
                        i,
                    }),
                    None => Ok(Html::text("—")),
                }
            };
            let mut row = FlexRow::new(id.to_string())
                .slot("id", render(NumberCell { number: id })?)
                .slot("kind", Html::text(kind))
                .slot("state", render(badge)?)
                .slot("summary", Html::text(text(&job["summary"])))
                .slot("requested_at", when("requested_at", "requested")?)
                .slot("finished_at", when("finished_at", "finished")?);
            row.dom_id = Some(format!("software-history-{id}"));
            rows.push(row);
        }
        let mut table = FlexTable::new("software-jobs", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Software jobs";
        table.framed = false;
        if jobs.is_empty() {
            table.empty = Some(TableEmpty::Content(Html::text(
                "No software change has been asked for on this instance yet.",
            )));
        }
        let body = render(table)?;
        render(Card {
            id: "software-history",
            title: "History",
            inner_class: "p-0",
            body,
            ..Card::default()
        })
    }

    /// The review the person confirms: the instance first, then what changes,
    /// the warnings, what will happen, and the code field.
    fn review_dialog(
        &self,
        instance: &Instance,
        lost: Option<u64>,
        review: &Review,
    ) -> Result<Html> {
        let facts = &review.facts;
        let label = instance.label.as_str();
        let banner = instance
            .banner("software-review-instance", "mb-3")
            .map_err(|_| Error::Internal)?;
        let mut lines: Vec<ChangeLine> = Vec::new();
        let mut checks: Vec<String> = Vec::new();
        let (title, lead, submit, variant) = match review.kind {
            Kind::Change => {
                let platform = &facts["platform"];
                let from = text(&platform["from"]);
                let to = text(&platform["to"]);
                lines.push(match text(&platform["action"]) {
                    "keep" => ChangeLine::keep(format!("Bilibi stays at {to}")),
                    "downgrade" => {
                        ChangeLine::warn(format!("Bilibi goes back from {from} to {to}"))
                    }
                    _ => ChangeLine::change(format!("Bilibi is updated from {from} to {to}")),
                });
                if let Some(because) = platform["because"].as_str() {
                    lines.last_mut().unwrap().note =
                        Some(format!("Bilibi itself changes because {because}."));
                }
                for d in facts["domains"].as_array().into_iter().flatten() {
                    let name = text(&d["name"]);
                    let version = text(&d["version"]);
                    let from = text(&d["from_version"]);
                    let mut line = match text(&d["action"]) {
                        "add" => ChangeLine::change(format!("Add {name} {version}")),
                        "remove" => ChangeLine::warn(format!("Remove {name}")),
                        "update" => ChangeLine::change(format!("Update {name} {from} → {version}")),
                        "downgrade" => {
                            ChangeLine::warn(format!("Go back: {name} {from} → {version}"))
                        }
                        _ => ChangeLine::keep(format!("Keep {name} {version}")),
                    };
                    if let Some(note) = d["note"].as_str() {
                        line.note = Some(note.to_owned());
                    } else if d["tested"] == false && d["action"] != "remove" {
                        line.note = Some("Not recorded as tested with this Bilibi.".into());
                    } else if d["source"] != "catalogue" && d["action"] != "remove" {
                        line.note = Some(format!("From {}", text(&d["source"])));
                    }
                    lines.push(line);
                }
                for p in facts["plugins"].as_array().into_iter().flatten() {
                    match text(&p["action"]) {
                        "enable" => lines.push(ChangeLine::change(format!(
                            "Switch on {}",
                            text(&p["name"])
                        ))),
                        "disable" => {
                            lines.push(ChangeLine::warn(format!("Switch off {}", text(&p["name"]))))
                        }
                        _ => {}
                    }
                }
                let estimate = facts["estimate_minutes"].as_u64().unwrap_or_default();
                let pause = facts["pause_minutes"].as_u64().unwrap_or(1);
                let lead = format!(
                    "The build takes {}; then {label} pauses for {} to switch. {}",
                    minutes_text(estimate),
                    minutes_text(pause),
                    if facts["switch"] == "ask" {
                        "You are asked before the pause."
                    } else {
                        "It switches as soon as the build is ready."
                    }
                );
                (
                    "Review the change".to_owned(),
                    lead,
                    format!("Build on {label}"),
                    ButtonVariant::Primary,
                )
            }
            Kind::Install => {
                let release = facts["release"].as_i64().unwrap_or_default();
                let source = &facts["source"];
                let source_text = if source["kind"] == "same_machine" {
                    format!("{} on this machine", text(&source["instance"]))
                } else {
                    text(&source["address"]).to_owned()
                };
                lines.push(ChangeLine::change(format!(
                    "Install release {release} from {source_text}"
                )));
                let running = &facts["running"];
                let domains: Vec<String> = running["domains"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|d| format!("{} {}", text(&d["id"]), text(&d["version"])))
                    .collect();
                lines.push(ChangeLine::keep(format!(
                    "{label} runs Bilibi {} with {} now",
                    text(&running["platform"]),
                    if domains.is_empty() {
                        "no Domains".to_owned()
                    } else {
                        domains.join(", ")
                    }
                )));
                if let Some(changes) = facts["changes"].as_str() {
                    lines.last_mut().unwrap().note = Some(changes.to_owned());
                }
                checks = facts["checks"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| text(c).to_owned())
                    .collect();
                let pause = facts["pause_minutes"].as_u64().unwrap_or(1);
                (
                    "Review the install".to_owned(),
                    format!(
                        "The release is copied and checked first. Then {label} pauses for {}.",
                        minutes_text(pause)
                    ),
                    format!("Install on {label}"),
                    ButtonVariant::Primary,
                )
            }
            Kind::Go => {
                let release = facts["release"].as_i64().unwrap_or_default();
                lines.push(ChangeLine::change(format!(
                    "Put release {release} into service on {label}"
                )));
                let pause = facts["pause_minutes"].as_u64().unwrap_or(1);
                (
                    "Go ahead with the pause".to_owned(),
                    format!(
                        "Release {release} is ready. {label} pauses for {} once you go ahead.",
                        minutes_text(pause)
                    ),
                    format!("Install now on {label}"),
                    ButtonVariant::Primary,
                )
            }
            Kind::Undo => {
                lines.push(ChangeLine::warn(format!(
                    "Undo job {}: {}",
                    facts["job"].as_i64().unwrap_or_default(),
                    text(&facts["undone"])
                )));
                lines.push(ChangeLine::change(format!(
                    "Back to release {} from release {}",
                    facts["previous_release"].as_i64().unwrap_or_default(),
                    facts["release"].as_i64().unwrap_or_default()
                )));
                let pause = facts["pause_minutes"].as_u64().unwrap_or(1);
                (
                    "Undo the last change".to_owned(),
                    format!("{label} pauses for {} to switch back.", minutes_text(pause)),
                    format!("Undo on {label}"),
                    ButtonVariant::Danger,
                )
            }
        };
        let title = if review.kind == Kind::Undo {
            text(&facts["loss"]).to_owned()
        } else {
            title
        };
        let mut warnings: Vec<(StatusKind, String)> = facts["warnings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|w| (StatusKind::Warning, text(w).to_owned()))
            .collect();
        if review.kind == Kind::Undo {
            if let (Some(count), Some(at)) = (lost, facts["backup_at"].as_str()) {
                let (changes, verb) = if count == 1 {
                    ("1 change entered", "is")
                } else {
                    ("changes entered", "are")
                };
                let counted = if count == 1 {
                    changes.to_owned()
                } else {
                    format!("{count} {changes}")
                };
                warnings.insert(
                    0,
                    (
                        StatusKind::Warning,
                        format!(
                            "About {counted} since {} {verb} lost (counted just now).",
                            crate::review::utc_text(at)
                        ),
                    ),
                );
            }
        }
        if matches!(review.kind, Kind::Install | Kind::Go)
            && instance.role == bilibi_system::Role::Production
        {
            // The review's own Production warning is the first; it is drawn as an error so it reads as the alert it is.
            if let Some(first) = warnings.first_mut() {
                first.0 = StatusKind::Error;
            }
        }
        let warnings: Vec<Html> = warnings
            .iter()
            .enumerate()
            .map(|(index, (kind, message))| {
                render(Alert {
                    kind: *kind,
                    id: &format!("software-review-warning-{index}"),
                    class: "",
                    body: Html::text(message),
                })
            })
            .collect::<Result<_>>()?;
        let steps: Vec<String> = facts["steps"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|s| text(&s["label"]).to_owned())
            .collect();
        let code = CodeInput::new(
            "software-code",
            "code",
            "6-digit code from your authenticator app",
            6,
        );
        let error = match &review.error {
            Some(message) => render(Alert {
                kind: StatusKind::Error,
                id: "software-review-error",
                class: "mb-3",
                body: Html::text(message),
            })?,
            None => Html::EMPTY,
        };
        let cancel = render(Button {
            id: "software-review-cancel",
            button_type: "button",
            event: "cancel_dialog",
            body: Html::text("Cancel"),
            ..Button::default()
        })?;
        let confirm = render(Button {
            id: "software-review-confirm",
            button_type: "submit",
            variant,
            working: "Queuing…",
            body: Html::text(&submit),
            ..Button::default()
        })?;
        let body = render(ReviewBody {
            banner,
            lead,
            lines,
            checks,
            warnings,
            what_happens: text(&facts["what_happens"]).to_owned(),
            steps,
            event: review.kind.event(),
            code,
            error,
            cancel,
            confirm,
        })?;
        render(Modal {
            id: "software-review",
            title: &title,
            cancel_event: "cancel_dialog",
            width: ModalWidth::Narrow,
            description: "Nothing changes until you confirm with your code.",
            role: "",
            flash: None,
            body,
        })
    }
}

/// `update` when `to` is not older than `from`, `downgrade` when it is; the
/// badge on a Domain row before the review says it exactly.
fn movement(from: &str, to: &str) -> &'static str {
    match (
        crate::catalogue::Version::parse(from),
        crate::catalogue::Version::parse(to),
    ) {
        (Some(from), Some(to)) if to < from => "downgrade",
        _ => "update",
    }
}
/// The newest release among version texts.
fn newest<'a>(versions: &[&'a str]) -> Option<&'a str> {
    versions
        .iter()
        .filter_map(|v| crate::catalogue::Version::parse(v).map(|parsed| (parsed, *v)))
        .max()
        .map(|(_, v)| v)
}

struct StepView {
    id: String,
    label: String,
    state: String,
    icon: &'static str,
    message: String,
    percent: Option<u8>,
    took: Option<String>,
    developer: Option<String>,
}
impl StepView {
    fn icon_class(&self) -> &'static str {
        match self.state.as_str() {
            "done" => "size-4 shrink-0 text-success-ink",
            "failed" => "size-4 shrink-0 text-danger-ink",
            "running" => "size-4 shrink-0 animate-spin text-action motion-reduce:animate-none",
            "skipped" => "size-4 shrink-0 text-ink-faint",
            _ => "size-4 shrink-0 text-ink-faint",
        }
    }
    fn label_class(&self) -> &'static str {
        match self.state.as_str() {
            "pending" | "skipped" => "text-ink-muted",
            _ => "text-ink",
        }
    }
}

struct ChangeLine {
    tone: &'static str,
    text: String,
    note: Option<String>,
}
impl ChangeLine {
    fn keep(text: String) -> Self {
        Self {
            tone: "text-ink-muted",
            text,
            note: None,
        }
    }
    fn change(text: String) -> Self {
        Self {
            tone: "text-ink font-medium",
            text,
            note: None,
        }
    }
    fn warn(text: String) -> Self {
        Self {
            tone: "text-warning-ink font-medium",
            text,
            note: None,
        }
    }
}

struct DomainRow {
    id: String,
    tick: Field,
    description: String,
    modules: String,
    installed: String,
    action: Option<Badge<'static>>,
    version: Field,
    remove_note: Option<String>,
    plugins: Vec<Field>,
}

#[derive(Template)]
#[template(path = "software.html")]
struct PageView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    banner: Html,
    instance_card: Html,
    job_card: Html,
    change_card: Html,
    history_card: Html,
    dialog: Html,
}

#[derive(Template)]
#[template(path = "software_job.html")]
struct JobBody {
    id: i64,
    heading: String,
    summary: String,
    requested: Option<Instant>,
    finished: Option<Instant>,
    worker: Option<String>,
    steps: Vec<StepView>,
    result_kind: Option<StatusKind>,
    result_message: String,
    notes: Vec<String>,
    actions: Html,
}

#[derive(Template)]
#[template(path = "software_domains.html")]
struct DomainsForm {
    rows: Vec<DomainRow>,
    outside: Vec<String>,
    switch: RadioGroup,
    summary: String,
    review: Html,
    reset: Html,
    error: Html,
}

#[derive(Template)]
#[template(path = "software_install.html")]
struct InstallForm<'a> {
    label: &'a str,
    source: Option<&'a str>,
    release: Field,
    review: Html,
    busy: bool,
    production: bool,
}

#[derive(Template)]
#[template(path = "software_review.html")]
struct ReviewBody {
    banner: Html,
    lead: String,
    lines: Vec<ChangeLine>,
    checks: Vec<String>,
    warnings: Vec<Html>,
    what_happens: String,
    steps: Vec<String>,
    event: &'static str,
    code: CodeInput,
    error: Html,
    cancel: Html,
    confirm: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "feedback.html" as feedback %}<p class="text-sm text-ink">{{ message }}</p><p class="mt-1 text-xs text-ink-muted">Address: {{ url }}. A server owner changes it under Settings › Software.</p><div class="mt-3">{{ retry|safe }}</div>"#,
    ext = "html"
)]
struct CatalogueUnavailable<'a> {
    message: &'a str,
    url: &'a str,
    retry: Html,
}

#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::instant(id=id, i=i) %}{% endcall %}"#,
    ext = "html"
)]
struct InstantCell {
    id: String,
    i: Instant,
}

#[derive(Template)]
#[template(
    source = r#"<span class="tabular-nums text-ink">{{ number }}</span>"#,
    ext = "html"
)]
struct NumberCell {
    number: i64,
}

impl LiveState<Scope, Error> for View {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            if let LiveInput::Event(value) = request.input {
                // Events act on the facts the person saw; the render after
                // them reads everything again.
                let status =
                    read(&self.pages.contracts, request.scope, "base.software.status").await?;
                let catalogue = if status["instance"]["builds"] == true {
                    self.catalogue(request.scope).await?
                } else {
                    json!({"available": false})
                };
                self.event(
                    request.scope,
                    request.credential,
                    value,
                    &status,
                    &catalogue,
                )
                .await?;
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                ..LiveBody::default()
            })
        })
    }
    fn subscriptions(&mut self) -> Vec<Subscription> {
        vec![Subscription {
            receiver: self.pages.follow.subscribe(),
        }]
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Software")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut view = View::new(self.clone(), request.query)?;
            let html = view.render(scope).await?;
            Ok(PageBody {
                html,
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
            let state: Box<dyn LiveState<Scope, Error>> =
                Box::new(View::new(self.clone(), request.query)?);
            Ok(Some(state))
        })
    }
}
