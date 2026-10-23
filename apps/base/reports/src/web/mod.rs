//! The report pages (`base/reports.pages`): `/reports`, the list with its
//! New report dialog (`index`), and `/reports/{id}`, one report read first
//! with its result, definition and sharing as tabs (`show`). No Bilimbi
//! counterpart; the list follows Bilimbi `Session.Web.IndexLive` as Events
//! does, the create dialog follows Contacts. Every write is an operation
//! contract (ADR 01), declared in `PAGE_WRITES`.
mod builder;
pub(crate) mod download;
mod index;
pub(crate) mod panel;
mod result;
mod show;

use crate::{ReportError, Reports, AUTHOR, EXPORT, SHARE};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{LiveState, MountFuture, Page, PageBody, PageFuture, PageRequest},
    FlashMap, Html, Instant, StatusKind, NO_FLASH,
};
use serde_json::Value;
use std::{collections::BTreeMap, sync::Arc};

pub const SERVICE: &str = "base/reports.pages";
pub const INDEX: &str = "/reports";
pub const SHOW: &str = "/reports/{id}";
pub const EXPORT_PATH: &str = "/reports/{id}/export";

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub reports: Arc<Reports>,
    pub contracts: Arc<PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

/// What the reader may do here, read once per render.
#[derive(Clone, Copy, Default)]
pub(crate) struct Can {
    pub author: bool,
    pub share: bool,
    pub export: bool,
}

impl Pages {
    pub async fn can(&self, scope: &Scope) -> Result<Can> {
        let client = self.db.get().await?;
        let held = effective_capabilities(&**client, scope).await?;
        Ok(Can {
            author: held.contains(AUTHOR),
            share: held.contains(SHARE),
            export: held.contains(EXPORT),
        })
    }
    pub async fn display(&self, scope: &Scope) -> Result<DisplayContext> {
        let client = self.db.get().await?;
        let locale = self.locales.resolve(&**client, scope, None).await?.locale;
        self.display.display(&**client, scope, None, locale).await
    }
    /// A declared page write, as the agent doors call it.
    pub async fn submit(
        &self,
        scope: &Scope,
        credential: &str,
        event: &'static str,
        contract: &'static str,
        input: Value,
    ) -> std::result::Result<Value, ContractError> {
        let call = Call {
            scope,
            credential,
            service: SERVICE,
            event,
            contract,
        };
        self.contracts
            .submit(&call, input)
            .await
            .map(|result| result["result"].clone())
    }
}

/// A contract refusal as the page's own words, or the transport error a
/// page must not swallow (the host's sign-in and step-up flows).
pub(crate) enum Refusal {
    Fields(BTreeMap<String, Vec<String>>),
    Message(String),
}
pub(crate) fn refusal(error: ContractError, denied: &str) -> Result<Refusal> {
    match error {
        ContractError::Backend(error @ (Error::Unauthorized | Error::StepUpRequired { .. })) => {
            Err(error)
        }
        ContractError::InvalidInput(fields) => {
            // The facade's own sentences arrive under "report".
            match fields.get("report") {
                Some(messages) if fields.len() == 1 => Ok(Refusal::Message(messages.join(" "))),
                _ => Ok(Refusal::Fields(fields)),
            }
        }
        ContractError::Forbidden | ContractError::Backend(Error::Forbidden) => {
            Ok(Refusal::Message(denied.into()))
        }
        ContractError::NotFound => Ok(Refusal::Message(ReportError::NotFound.to_string())),
        ContractError::StalePlan | ContractError::Backend(Error::Conflict) => {
            Ok(Refusal::Message(ReportError::Conflict.to_string()))
        }
        _ => Ok(Refusal::Message(
            "Reports are unavailable right now; nothing was saved.".into(),
        )),
    }
}

pub(crate) fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// The page's one flash, as the shared group takes it.
pub(crate) fn flash_map(flash: &Option<(StatusKind, String)>) -> FlashMap<'_> {
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

/// The shared dismiss: clears the flash it names, whatever page holds it.
pub(crate) fn clear_flash(flash: &mut Option<(StatusKind, String)>, value: &Value) {
    let key = value.get("key").and_then(Value::as_str);
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| Some(kind.name()) == key)
    {
        *flash = None;
    }
}

/// A stored UTC time (`YYYY-MM-DDTHH:MM:SSZ`) in the reader's display.
pub(crate) fn instant(display: &DisplayContext, at: &str) -> Option<Instant> {
    let at: jiff::Timestamp = at.parse().ok()?;
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Some(Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    ))
}

pub(crate) fn report_path(id: i64) -> String {
    format!("/reports/{id}")
}

/// A facade refusal while rendering: a report gone or no longer shared is
/// the page's 404, a lost capability its 403.
pub(crate) fn page_error(error: ReportError) -> Error {
    match error {
        ReportError::NotFound | ReportError::Unreadable => Error::NotFound,
        ReportError::Forbidden => Error::Forbidden,
        ReportError::Database(error) => error,
        _ => Error::Internal,
    }
}

fn report_id(request: &PageRequest<'_, Scope>) -> Result<i64> {
    bilibi_ui::params::positive_id(request.path.get("id").map(String::as_str))
        .ok_or(Error::NotFound)
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Reports")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let (html, title) = match request.operation {
                "index" => {
                    let mut index = index::Index::new(self.clone(), request.query)?;
                    (index.render(scope).await?, None)
                }
                "show" => {
                    let mut show =
                        show::Show::new(self.clone(), report_id(&request)?, request.query);
                    let html = show.render(scope).await?;
                    (html, show.title())
                }
                _ => return Err(Error::NotFound),
            };
            Ok(PageBody {
                html,
                live: true,
                title,
            })
        })
    }
    fn mount<'a>(&'a self, request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let state: Box<dyn LiveState<Scope, Error>> = match request.operation {
                "index" => Box::new(index::Index::new(self.clone(), request.query)?),
                "panel" => Box::new(panel::Panel::new(
                    self.clone(),
                    bilibi_dashboard::PanelArgs::from_request(&request)?,
                )?),
                "show" => Box::new(show::Show::new(
                    self.clone(),
                    report_id(&request)?,
                    request.query,
                )),
                _ => return Ok(None),
            };
            Ok(Some(state))
        })
    }
}
