//! `/system/media/quarantine` (`base/media.pages`, operation `index`): the
//! tenant's stored files a malware scan held back, with rescan and purge (issue
//! 141). No Bilimbi counterpart (Bilimbi does not scan uploads); the list
//! follows the Events page, which follows Bilimbi `Session.Web.IndexLive`.
//!
//! Rescan and purge are the `base.media.quarantine.*` contracts (ADR 01), as
//! agents call them. Purge confirms through the shared dialog, with the
//! instance banner (`bilibi_system::Instance::banner`): the request
//! holds the row's file id, and `purge` acts on that held request, never on a
//! client value. Media rechecks `admin.media.quarantine.manage` in its own
//! transaction; a refusal is a flash.
use crate::{agent, Media, QuarantinePage, QuarantineQuery, ScanState, QUARANTINE_MANAGE};
use askama::Template;
use bilibi_agent_api::{
    page::{Call, PageContracts},
    Error as ContractError,
};
use bilibi_authz::effective_capabilities;
use bilibi_database::{Db, Error, Result};
use bilibi_datetime::{DisplayContext, DisplayPolicy, Format, Precision};
use bilibi_locale::Locales;
use bilibi_module_registry::{
    contributions::{Access, Method, PageWrites, Pending, Route, PAGE_WRITES, ROUTES},
    Contribution, Provenance,
};
use bilibi_system::Instance;
use bilibi_tenancy::{ReadClient, Scope};
use bilibi_ui::{
    choices,
    list_state::{Filter, FILTERS_FORM},
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest,
    },
    params, Alert, Badge, BadgeKind, BuiltinColumn, Card, ColumnType, ColumnsOutcome,
    FilterToolbar, FlashMap, FlexRow, FlexTable, Header, Html, IconButton, IconContext, IconKind,
    Instant, ListColumns, ListSpec, ListState, PageCounts, Pagination, RowMode, StatusKind,
    TableEmpty, ToolbarControl, NO_FLASH,
};
use jiff::Timestamp;
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashMap},
    sync::{Arc, LazyLock},
};

pub const PATH: &str = "/system/media/quarantine";
pub(crate) const PAGES: &str = "base/media.pages";
const PAGE_SIZES: [u64; 3] = [25, 50, 100];
const DENIED: &str = "You do not have permission to manage quarantined files.";
const STATES: [&str; 6] = [
    "attention",
    "infected",
    "failed",
    "pending",
    "unscanned",
    "all",
];

static SPEC: LazyLock<Arc<ListSpec>> = LazyLock::new(|| {
    ListSpec {
        page_sizes: PAGE_SIZES.to_vec(),
        default_page_size: 25,
        page_size_param: "per_page".into(),
        filters: BTreeMap::from([("state".into(), Filter::one_of(&STATES, "attention"))]),
        omit_defaults: true,
        ..ListSpec::default()
    }
    .shared()
    .expect("quarantined file list spec")
});

fn builtins() -> Vec<BuiltinColumn> {
    let mut stored = BuiltinColumn::new("stored", "Stored");
    stored.kind = ColumnType::Datetime;
    let mut scanned = BuiltinColumn::new("scanned", "Scanned");
    scanned.kind = ColumnType::Datetime;
    vec![
        BuiltinColumn::new("file", "File"),
        BuiltinColumn::new("state", "Status"),
        BuiltinColumn::new("finding", "Finding"),
        BuiltinColumn::new("record", "Attached to"),
        stored,
        scanned,
    ]
}

/// The module's page adapter; every field is shared by mounted states.
#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub media: Arc<Media>,
    pub contracts: Arc<PageContracts>,
    pub display: Arc<DisplayPolicy>,
    pub locales: Arc<Locales>,
}

fn render(fragment: impl Template) -> Result<Html> {
    Html::render(&fragment).map_err(|_| Error::Internal)
}
fn parse(query: &HashMap<String, String>) -> Result<ListState> {
    ListState::parse(query, SPEC.clone()).map_err(|_| Error::Internal)
}

#[derive(Template)]
#[template(
    source = r#"<span class="block text-ink">{{ name }}</span>{% if let Some(detail) = detail %}<span class="block text-xs text-ink-muted">{{ detail }}</span>{% endif %}"#,
    ext = "html"
)]
struct TwoLines<'a> {
    name: &'a str,
    detail: Option<String>,
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

/// The purge confirmation, opening with the installation it purges on.
#[derive(Template)]
#[template(
    source = r#"{% import "dialog.html" as dialog %}{% call dialog::confirm_dialog(id="media-quarantine-purge-confirm", consequence=consequence, detail="The file's contents are removed for good. Its record, its scan verdict and the audit trail stay as evidence.", confirm="Purge", working="Purging…", confirm_event="purge", cancel_event="cancel_purge", body=banner) %}{% endcall %}"#,
    ext = "html"
)]
struct PurgeConfirm<'a> {
    consequence: &'a str,
    banner: Html,
}

#[derive(Template)]
#[template(path = "media_quarantine.html")]
struct IndexView<'a> {
    flash: FlashMap<'a>,
    header: Html,
    notice: Html,
    toolbar: Html,
    card: Html,
    dialog: Html,
}

/// A purge the person asked for and has not yet confirmed.
struct Request {
    id: i64,
    name: String,
}

struct Index {
    pages: Pages,
    state: ListState,
    columns: ListColumns,
    rows_changed: bool,
    /// The files last shown: a request names one of these.
    rows: Vec<crate::QuarantinedFile>,
    request: Option<Request>,
    flash: Option<(StatusKind, String)>,
}

fn size(bytes: Option<i64>) -> Option<String> {
    let bytes = bytes? as f64;
    Some(if bytes < 1024.0 {
        format!("{bytes} B")
    } else if bytes < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes / 1024.0)
    } else {
        format!("{:.1} MB", bytes / (1024.0 * 1024.0))
    })
}
fn reason(code: Option<&str>) -> Option<&'static str> {
    match code? {
        "too_large" => Some("Larger than the scanner accepts"),
        "scanner_unavailable" => Some("The scanner did not answer"),
        "file_unavailable" => Some("The stored file could not be read"),
        "scan_error" => Some("The check could not be recorded"),
        _ => None,
    }
}

impl Index {
    fn new(pages: Pages, query: &HashMap<String, String>) -> Result<Self> {
        Ok(Self {
            pages,
            state: parse(query)?,
            columns: ListColumns::mount("media-quarantine", builtins())
                .map_err(|_| Error::Internal)?,
            rows_changed: true,
            rows: Vec::new(),
            request: None,
            flash: None,
        })
    }
    fn flash(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    fn shown(&self, value: &Value) -> Option<&crate::QuarantinedFile> {
        let id = value
            .get("key")
            .and_then(Value::as_str)?
            .parse::<i64>()
            .ok()?;
        self.rows.iter().find(|row| row.id == id)
    }
    /// The page of files the list asks for. Search and paging run in SQL,
    /// so every matching file is within reach however many there are.
    async fn window(
        &self,
        client: &dyn ReadClient,
        scope: &Scope,
        states: &[ScanState],
        search: &str,
    ) -> Result<QuarantinePage> {
        let query = QuarantineQuery {
            states,
            search,
            offset: i64::try_from(self.state.offset()).map_err(|_| Error::Invalid)?,
            limit: i64::try_from(self.state.page_size).map_err(|_| Error::Invalid)?,
        };
        self.pages
            .media
            .quarantine(client, scope, query)
            .await
            .map_err(|error| match error {
                crate::Error::Database(error) => error,
                _ => Error::Internal,
            })
    }
    fn counts(&self, found: &QuarantinePage) -> PageCounts {
        let total = u64::try_from(found.total).unwrap_or(0);
        PageCounts::new(self.state.page, self.state.page_size, total)
    }

    /// Run one quarantine contract as the page's person and say how it went.
    async fn act(
        &mut self,
        scope: &Scope,
        credential: &str,
        event: &'static str,
        id: i64,
        name: &str,
    ) -> Result<()> {
        let call = Call {
            scope,
            credential,
            service: PAGES,
            event,
            contract: if event == "rescan" {
                "base.media.quarantine.rescan"
            } else {
                "base.media.quarantine.purge"
            },
        };
        match self.pages.contracts.submit(&call, json!({"file_id": id})).await {
            Ok(_) if event == "rescan" => self.flash(
                StatusKind::Success,
                format!("{name} is being scanned again. It stays unavailable until the scanner calls it clean."),
            ),
            Ok(_) => self.flash(
                StatusKind::Success,
                format!("{name}'s contents are being removed for good. Its record stays as evidence."),
            ),
            Err(ContractError::Backend(error @ (Error::StepUpRequired { .. } | Error::Unauthorized))) => {
                return Err(error)
            }
            Err(ContractError::Forbidden | ContractError::Backend(Error::Forbidden)) => {
                self.flash(StatusKind::Error, DENIED)
            }
            Err(ContractError::InvalidInput(fields)) => {
                let reason = fields
                    .get("file_id")
                    .and_then(|messages| messages.first())
                    .cloned()
                    .unwrap_or_else(|| "The request was refused".into());
                self.flash(StatusKind::Error, format!("{name}: {reason}."))
            }
            Err(_) => self.flash(
                StatusKind::Error,
                "File storage is unavailable; nothing was changed.",
            ),
        }
        Ok(())
    }

    async fn event(&mut self, scope: &Scope, credential: &str, value: &Value) -> Result<()> {
        let get = |key: &str| value.get(key).and_then(Value::as_str);
        match get("type").ok_or(Error::Invalid)? {
            "filters" => self
                .state
                .apply_filters(&params::form_values(value, FILTERS_FORM)),
            "page" => self.state.put_page(get("page")),
            "grid" => match self.columns.handle(&params::string_values(value)) {
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
            "rescan" => {
                // Gone since the list was drawn: the redraw shows that.
                if let Some(row) = self.shown(value) {
                    let (id, name) = (row.id, display_name(row).to_owned());
                    self.act(scope, credential, "rescan", id, &name).await?;
                }
            }
            "request_purge" => {
                if let Some(request) = self.shown(value).map(|row| Request {
                    id: row.id,
                    name: display_name(row).to_owned(),
                }) {
                    self.flash = None;
                    self.request = Some(request);
                }
            }
            "cancel_purge" => self.request = None,
            "purge" => {
                if let Some(request) = self.request.take() {
                    self.act(scope, credential, "purge", request.id, &request.name)
                        .await?;
                }
            }
            _ => return Err(Error::Invalid),
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
        let can_manage = effective_capabilities(&**client, scope)
            .await?
            .contains(QUARANTINE_MANAGE);
        let scanning = pages.media.scanning();
        let filter = self.state.filters["state"].clone();
        let states = agent::states(&filter).ok_or(Error::Invalid)?;
        let search = self.state.search.trim().to_owned();
        let mut found = self.window(&**client, scope, &states, &search).await?;
        let mut counts = self.counts(&found);
        if self.state.clamp_to_last_page(counts.total_pages, true) {
            found = self.window(&**client, scope, &states, &search).await?;
            counts = self.counts(&found);
        }
        self.rows = found.files;
        if self.rows_changed {
            self.columns.load(&self.rows, |row| row.id.to_string());
        }
        let header = render(Header {
            title: Html::text("Quarantined Files"),
            subtitle: "Files a malware scan held back. Nobody can open them until a scan calls them clean.",
            title_actions: render(IconButton {
                context: IconContext::Inline,
                id: "media-quarantine-pin",
                nav_pin: "nav-admin-system-media-quarantine",
                ..IconButton::new("bilimbi-pin", "Pin Quarantined Files to sidebar")
            })?,
            ..Header::default()
        })?;
        let notice = if scanning {
            Html::EMPTY
        } else {
            render(Alert {
                kind: StatusKind::Info,
                id: "media-quarantine-no-scanner",
                class: "",
                body: Html::text("No malware scanner is installed, so new files are not scanned. Rescanning needs a scanner."),
            })?
        };
        let form = self.state.filters_form();
        let toolbar = render(FilterToolbar::new(
            "media-quarantine-filters",
            vec![
                ToolbarControl::search(
                    "media-quarantine-search",
                    "search",
                    "Search files",
                    &form["search"],
                    "Search by file name...",
                ),
                ToolbarControl::select(
                    "media-quarantine-state",
                    "state",
                    "Filter by status",
                    &form["state"],
                    choices(&[
                        ("Needs attention", "attention"),
                        ("Quarantined", "infected"),
                        ("Scan failed", "failed"),
                        ("Being checked", "pending"),
                        ("Not scanned", "unscanned"),
                        ("All not clean", "all"),
                    ]),
                ),
            ],
        ))?;
        let compact = self.columns.mode() == RowMode::Compact;
        let mut rows = Vec::with_capacity(self.rows.len());
        for file in &self.rows {
            let key = file.id.to_string();
            let requested = file.purge_requested_at.is_some();
            let purged = file.purged_at.is_some();
            let status = render(match (file.state, requested, purged) {
                (_, _, true) => Badge::status(BadgeKind::Neutral, "Purged"),
                (_, true, _) => Badge::status(BadgeKind::Plain, "Purging"),
                (ScanState::Infected, ..) => Badge::status(BadgeKind::Danger, "Quarantined"),
                (ScanState::Failed, ..) => Badge::status(BadgeKind::Warning, "Scan failed"),
                (ScanState::Pending, ..) => Badge::status(BadgeKind::Plain, "Being checked"),
                (ScanState::Unscanned, ..) => Badge::status(BadgeKind::Neutral, "Not scanned"),
                (ScanState::Clean, ..) => Badge::status(BadgeKind::Success, "Clean"),
            })?;
            let detail = (!compact).then(|| {
                [file.mime_type.clone(), size(file.file_size)]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
            });
            let scanner = match (&file.scanner, &file.scanner_version) {
                (Some(name), Some(version)) => Some(format!("{name} {version}")),
                (Some(name), None) => Some(name.clone()),
                _ => None,
            };
            let finding = match (&file.signature, reason(file.reason.as_deref())) {
                (Some(found), _) => render(TwoLines {
                    name: found,
                    detail: scanner.filter(|_| !compact),
                })?,
                (None, Some(reason)) => render(TwoLines {
                    name: reason,
                    detail: scanner.filter(|_| !compact),
                })?,
                (None, None) => Html::text("—"),
            };
            let record = match &file.subject {
                Some((kind, id)) => Html::text(&format!("{kind} {id}")),
                None => Html::text("—"),
            };
            let stored_id = format!("media-quarantine-stored-{key}");
            let stored = render(When {
                id: &stored_id,
                i: instant(&display, file.created_at),
            })?;
            let scanned = match file.scanned_at {
                Some(at) => {
                    let id = format!("media-quarantine-scanned-{key}");
                    render(When {
                        id: &id,
                        i: instant(&display, at),
                    })?
                }
                None => Html::text("—"),
            };
            let mut actions = Html::EMPTY;
            if can_manage && scanning && !requested && file.state != ScanState::Clean {
                let id = format!("media-quarantine-rescan-{key}");
                actions.push(&render(IconButton {
                    id: &id,
                    kind: IconKind::Neutral,
                    event: "rescan",
                    name: "key",
                    value: &key,
                    ..IconButton::new("refresh", "Scan again")
                })?);
            }
            if can_manage
                && !purged
                && matches!(file.state, ScanState::Infected | ScanState::Failed)
            {
                let id = format!("media-quarantine-purge-{key}");
                actions.push(&render(IconButton {
                    id: &id,
                    kind: IconKind::Danger,
                    event: "request_purge",
                    name: "key",
                    value: &key,
                    ..IconButton::new("trash", "Purge contents")
                })?);
            }
            let mut row = FlexRow::new(key.clone())
                .slot(
                    "file",
                    render(TwoLines {
                        name: display_name(file),
                        detail: detail.filter(|text| !text.is_empty()),
                    })?,
                )
                .slot("state", status)
                .slot("finding", finding)
                .slot("record", record)
                .slot("stored", stored)
                .slot("scanned", scanned);
            row.dom_id = Some(format!("media-quarantine-{key}"));
            row.actions = actions;
            rows.push(row);
        }
        let mut table = FlexTable::new("media-quarantine", self.columns.flex_columns(), rows);
        table.zoom = self.columns.zoom;
        table.suggestions = self.columns.flex_suggestions();
        table.add_query = &self.columns.add_query;
        table.caption = "Quarantined files";
        table.framed = false;
        table.actions = can_manage;
        if counts.total_entries == 0 {
            table.empty = Some(TableEmpty::Content(Html::text(if search.is_empty() {
                "No files with this status."
            } else {
                "No files match this search."
            })));
        }
        let mut body = render(table)?;
        body.push(&render(Pagination::new(
            "media-quarantine-pagination",
            counts,
            &PAGE_SIZES,
        ))?);
        let card = render(Card {
            id: "media-quarantine-card",
            inner_class: "p-0",
            body,
            ..Card::default()
        })?;
        let dialog = match &self.request {
            Some(request) => {
                let consequence = format!("Purge the contents of {}?", request.name);
                let instance = Instance::resolve(&pages.media.settings, &**client).await?;
                render(PurgeConfirm {
                    consequence: &consequence,
                    banner: instance
                        .banner("media-quarantine-purge-instance", "mt-3")
                        .map_err(|_| Error::Internal)?,
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
        IndexView {
            flash,
            header,
            notice,
            toolbar,
            card,
            dialog,
        }
        .render()
        .map_err(|_| Error::Internal)
    }
}

fn display_name(file: &crate::QuarantinedFile) -> &str {
    file.original_filename.as_deref().unwrap_or("Unnamed file")
}

fn instant(display: &DisplayContext, at: i64) -> Instant {
    let at = Timestamp::from_second(at).unwrap_or(Timestamp::UNIX_EPOCH);
    let rendered = display.render(at, Format::DateTime, Precision::Minute);
    Instant::new(
        &rendered.iso,
        &rendered.text,
        &rendered.company,
        &rendered.utc,
        rendered.mode.as_str(),
    )
}

impl LiveState<Scope, Error> for Index {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            self.rows_changed = true;
            match request.input {
                LiveInput::Refresh => {}
                LiveInput::Params(values) => self.state = parse(values)?,
                LiveInput::Event(value) => {
                    self.event(request.scope, request.credential, value).await?
                }
            }
            let html = self.render(request.scope).await?;
            Ok(LiveBody {
                html,
                query: self.state.to_params(),
                rows: self.rows.len(),
                ..LiveBody::default()
            })
        })
    }
}

impl Page<Scope, Error> for Pages {
    fn title(&self, operation: &str) -> Option<&'static str> {
        (operation == "index").then_some("Quarantined Files")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            if request.operation != "index" {
                return Err(Error::NotFound);
            }
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let mut index = Index::new(self.clone(), request.query)?;
            let html = index.render(scope).await?;
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
                Box::new(Index::new(self.clone(), request.query)?);
            Ok(Some(state))
        })
    }
}

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/media",
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! { Contribution::new(ROUTES, "base/media.quarantine", provenance!(), &Route {
    method: Method::Get, path: PATH, service: PAGES, operation: "index",
    access: Access::Capability(crate::QUARANTINE_VIEW), framed: true,
}) }
bilibi_menu::contribute!(
    "base/media",
    bilibi_menu::MenuItem::page("admin.system.media-quarantine", "Quarantined Files", PATH)
        .icon("shield-exclamation")
        .under("admin.system")
        .requires(bilibi_menu::Requirement::Capability(crate::QUARANTINE_VIEW))
        .order(25)
);
inventory::submit! { Contribution::new(PAGE_WRITES, PAGES, provenance!(), &PageWrites {
    service: PAGES,
    contracts: &[("rescan", "base.media.quarantine.rescan"), ("purge", "base.media.quarantine.purge")],
    pending: Pending::None,
}) }
