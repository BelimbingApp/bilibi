//! The Imports pages (issue 133): `/imports` (the person's runs), `/imports/new`
//! (choose what the rows are and the file) and `/imports/{id}` (map the
//! columns, check, import, follow progress and read each row's result), and
//! `/exports/{id}` (one CSV export: its state and the download); `/imports`
//! lists the person's exports too. No
//! Bilimbi counterpart; the list follows the Events page, which follows
//! Bilimbi `Session.Web.IndexLive`, and the run reads first like a record.
//!
//! Every write is a `base.bulk_data.*` contract (ADR 01), as agents call them,
//! except receiving the file: Artifacts takes the bytes outside a contract,
//! so the `receive` event is a listed allowance (`Pending::Events`).
mod detail;
mod export;
mod index;
mod new;

use crate::{BulkData, Run, RunState, CREATE, VIEW};
use askama::Template;
use bilibi_agent_api::page::PageContracts;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{
        Access, Download, Method, PageWrites, Pending, Route, DOWNLOADS, PAGE_WRITES, ROUTES,
    },
    Contribution, Services,
};
use bilibi_tenancy::Scope;
use bilibi_ui::{
    page::{MountFuture, Page, PageBody, PageFuture, PageRequest},
    Badge, BadgeKind, FlashMap, Html, Instant, StatusKind, NO_FLASH,
};
use jiff::Timestamp;
use std::sync::Arc;

pub(crate) const PAGES: &str = "base/bulk_data.pages";
pub(crate) const FILES: &str = "base/bulk_data.files";
pub(crate) const EXPORT_FILES: &str = "base/bulk_data.export-files";
pub(crate) const INDEX: &str = "/imports";
const NEW: &str = "/imports/new";

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    db: Db,
    bulk: Arc<BulkData>,
    contracts: Arc<PageContracts>,
    display: Arc<DisplayPolicy>,
    locales: Arc<Locales>,
}

impl Pages {
    pub(crate) fn new(services: &Services) -> bilibi_module_registry::Result<Self> {
        Ok(Self {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            bulk: services.get(crate::SERVICE)?,
            contracts: services.get(bilibi_agent_api::page::SERVICE)?,
            display: services.get("base/datetime.display")?,
            locales: services.get("base/locale.locales")?,
        })
    }
    async fn display(
        &self,
        client: &tokio_postgres::Client,
        scope: &Scope,
    ) -> Result<DisplayContext> {
        let locale = self.locales.resolve(client, scope, None).await?.locale;
        self.display.display(client, scope, None, locale).await
    }
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}

/// One Imports page: its flash, then its parts in order.
#[derive(Template)]
#[template(path = "bulk_data_page.html")]
struct View<'a> {
    id: &'a str,
    flash: FlashMap<'a>,
    parts: Vec<Html>,
}
fn view(id: &str, flash: &Option<(StatusKind, String)>, parts: Vec<Html>) -> Result<String> {
    let mut messages = NO_FLASH;
    if let Some((kind, message)) = flash {
        match kind {
            StatusKind::Success => messages.success = message,
            StatusKind::Error => messages.error = message,
            StatusKind::Warning => messages.warning = message,
            StatusKind::Info => messages.info = message,
        }
    }
    View {
        id,
        flash: messages,
        parts,
    }
    .render()
    .map_err(|_| Error::Internal)
}

/// Clears the flash when its own dismiss control asks.
fn dismiss(flash: &mut Option<(StatusKind, String)>, key: Option<&str>) {
    if flash
        .as_ref()
        .is_some_and(|(kind, _)| Some(kind.name()) == key)
    {
        *flash = None;
    }
}

fn instant(display: &DisplayContext, at: Timestamp) -> Instant {
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    )
}

#[derive(Template)]
#[template(
    source = r#"{% import "flex_table.html" as flex %}{% call flex::instant(id=id, i=i) %}{% endcall %}"#,
    ext = "html"
)]
struct When<'a> {
    id: &'a str,
    i: Instant,
}
fn when(display: &DisplayContext, id: &str, at: Timestamp) -> Result<Html> {
    render(When {
        id,
        i: instant(display, at),
    })
}

/// The run's state as people read it.
fn status(run: &Run) -> Result<Html> {
    render(match (run.state, run.stopped) {
        (_, true) => Badge::status(BadgeKind::Danger, "Stopped"),
        (RunState::Draft, _) => Badge::status(BadgeKind::Neutral, "To map"),
        (RunState::Previewing, _) => Badge::status(BadgeKind::Plain, "Checking"),
        (RunState::Previewed, _) => Badge::status(BadgeKind::Neutral, "Checked"),
        (RunState::Running, _) => Badge::status(BadgeKind::Plain, "Importing"),
        (RunState::Completed, _) => Badge::status(BadgeKind::Success, "Imported"),
        (RunState::Cancelled, _) => Badge::status(BadgeKind::Warning, "Cancelled"),
    })
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        match operation {
            "index" => Some("Imports"),
            "new" => Some("New Import"),
            "detail" => Some("Import"),
            "export" => Some("Export"),
            _ => None,
        }
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let (html, title) = match request.operation {
                "index" => (
                    index::Index::new(self.clone(), request.query, request.framed)?
                        .render(scope)
                        .await?,
                    None,
                ),
                "new" => (new::Upload::new(self.clone()).render(scope).await?, None),
                "detail" => {
                    let mut detail =
                        detail::Detail::new(self.clone(), request.path, request.query)?;
                    let html = detail.render(scope).await?;
                    (html, detail.title())
                }
                "export" => (
                    export::ExportPage::new(self.clone(), request.path)?
                        .render(scope)
                        .await?,
                    None,
                ),
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
            Ok(Some(match request.operation {
                "index" => Box::new(index::Index::new(
                    self.clone(),
                    request.query,
                    request.framed,
                )?) as Box<dyn bilibi_ui::page::LiveState<Scope, Error>>,
                "new" => Box::new(new::Upload::new(self.clone())),
                "detail" => Box::new(detail::Detail::new(
                    self.clone(),
                    request.path,
                    request.query,
                )?),
                "export" => Box::new(export::ExportPage::new(self.clone(), request.path)?),
                _ => return Ok(None),
            }))
        })
    }
}

macro_rules! provenance {
    () => {
        crate::provenance!()
    };
}
inventory::submit! { Contribution::new(ROUTES, "base/bulk_data.index", provenance!(), &Route {
    method: Method::Get, path: INDEX, service: PAGES, operation: "index",
    access: Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "base/bulk_data.new", provenance!(), &Route {
    method: Method::Get, path: NEW, service: PAGES, operation: "new",
    access: Access::Capability(CREATE), framed: true,
}) }
inventory::submit! { Contribution::new(ROUTES, "base/bulk_data.detail", provenance!(), &Route {
    method: Method::Get, path: "/imports/{id}", service: PAGES, operation: "detail",
    access: Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "base/bulk_data.failed-rows", provenance!(), &Download {
    path: "/imports/{id}/failed-rows", service: FILES, operation: "failed-rows", access: Access::Capability(VIEW),
}) }
inventory::submit! { Contribution::new(ROUTES, "base/bulk_data.export", provenance!(), &Route {
    method: Method::Get, path: "/exports/{id}", service: PAGES, operation: "export",
    access: Access::Capability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "base/bulk_data.export-file", provenance!(), &Download {
    path: "/exports/{id}/file", service: EXPORT_FILES, operation: "export-file", access: Access::Capability(VIEW),
}) }
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
    service: PAGES,
    contracts: &[
        ("preview", "base.bulk_data.import.preview"),
        ("start", "base.bulk_data.import.start"),
        ("cancel", "base.bulk_data.import.cancel"),
        ("save_mapping", "base.bulk_data.mapping.save"),
    ],
    pending: Pending::Events { events: &["receive"], reason: "Receiving a CSV stores it as a private artifact and opens its run: Artifacts takes bytes outside a contract, whose input is bounded at 1 MiB. Agent-supplied files come later (issue 133)." },
}) }
