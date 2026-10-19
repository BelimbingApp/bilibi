//! The Release files page (`/system/software/releases`) and the release
//! file download: the small UI of release transport (`README.md`, "Release
//! transport"). It shows the releases kept on this instance with a download
//! link for each, the pairing key, what the source offers, and takes an
//! uploaded release file. Installing is the Software page's and the
//! `base.software.install` contract's; this page only moves files. No
//! Bilimbi counterpart: the Software page is Bilibi's own.
use crate::{transport, Software, MANAGE, VIEW};
use askama::Template;
use bilibi_agent_api::{page::PageContracts, Error as AgentError};
use bilibi_authz::authorize_operator;
use bilibi_database::{Db, Error, Result};
use bilibi_settings::Settings;
use bilibi_system::Instance;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    download::{Download, DownloadFuture, DownloadRequest, FileBody},
    forms::SecretInput,
    page::{
        LiveBody, LiveFuture, LiveInput, LiveRequest, LiveState, MountFuture, Page, PageBody,
        PageFuture, PageRequest, Uploads,
    },
    ActionLink, BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome, FlashMap, FlexRow,
    FlexTable, Header, Html, ListColumns, PageVariant, StatusKind, TableEmpty, UploadSpec,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub const PATH: &str = "/system/software/releases";
pub const DOWNLOAD_PATH: &str = "/system/software/releases/{release}/download";
pub(crate) const PAGES: &str = "base/software.release_pages";
pub(crate) const FILES: &str = "base/software.release_files";
/// The file input's name: the key the owner consumes by.
const UPLOAD: &str = "release_file";
/// A release file is a compressed program: tens of megabytes.
const UPLOAD_BYTES: u64 = bilibi_ui::upload::MAX_FILE_SIZE;

fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}

#[derive(Clone)]
pub(crate) struct Pages {
    pub db: Db,
    pub settings: Arc<Settings>,
    pub contracts: Arc<PageContracts>,
    pub software: Arc<Software>,
}
impl Page<Scope, Error> for Pages {
    fn title(&self, _operation: &str) -> Option<&'static str> {
        Some("Release files")
    }
    fn render<'a>(&'a self, request: PageRequest<'a, Scope>) -> PageFuture<'a, Error> {
        Box::pin(async move {
            let scope = request.scope.ok_or(Error::Unauthorized)?;
            let view = View::new(self.clone())?;
            Ok(PageBody {
                html: view.body(scope).await?,
                live: true,
                title: None,
            })
        })
    }
    fn mount<'a>(&'a self, _request: PageRequest<'a, Scope>) -> MountFuture<'a, Scope, Error> {
        Box::pin(async move {
            let view = View::new(self.clone())?;
            Ok(Some(Box::new(view) as Box<dyn LiveState<Scope, Error>>))
        })
    }
}

/// The release file download: `/system/software/releases/{release}/download`.
pub(crate) struct Files {
    pub software: Arc<Software>,
}
impl Download<Scope, Error> for Files {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let release: u64 = request
                .path
                .get("release")
                .and_then(|n| n.parse().ok())
                .filter(|n| *n >= 1)
                .ok_or(Error::NotFound)?;
            let (bytes, filename) = self.software.bundle(request.scope, release).await?;
            Ok(FileBody::attachment(
                bytes,
                transport::BUNDLE_TYPE,
                &filename,
            ))
        })
    }
}

/// The mounted page: the file input's state, the two tables' columns and
/// the last message.
struct View {
    pages: Pages,
    uploads: Uploads,
    here: ListColumns,
    source: ListColumns,
    flash: Option<(StatusKind, String)>,
    pair_error: Option<String>,
}
fn release_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("release", "Release").right(),
        BuiltinColumn::new("in_service", "In service"),
        BuiltinColumn::new("platform", "Bilibi"),
        BuiltinColumn::new("domains", "Domains"),
        BuiltinColumn::new("checksum", "Checksum"),
        BuiltinColumn::new("file", "File"),
    ]
}
impl View {
    fn new(pages: Pages) -> Result<Self> {
        Ok(Self {
            pages,
            uploads: Uploads::new([UploadSpec::new(UPLOAD, UPLOAD_BYTES).accept([
                ".tar.gz",
                ".tgz",
                "application/gzip",
                "application/x-gzip",
            ])]),
            here: ListColumns::mount("release-files-here", release_columns())
                .map_err(|_| Error::Internal)?,
            source: ListColumns::mount("release-files-source", release_columns())
                .map_err(|_| Error::Internal)?,
            flash: None,
            pair_error: None,
        })
    }
    fn say(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    /// The page's own answer to a refused contract call.
    fn refused(&mut self, error: Error, field: &str) {
        let message = match error {
            Error::Forbidden => "You do not have permission to change the pairing key.".to_owned(),
            Error::Invalid => "The pairing key was not accepted.".to_owned(),
            other => format!("The pairing key could not be saved ({other:?})."),
        };
        self.pair_error = Some(message.clone());
        let _ = field;
        self.say(StatusKind::Error, message);
    }
    async fn pair(&mut self, request: &LiveRequest<'_, Scope>, input: Value) {
        let call = bilibi_agent_api::page::Call {
            scope: request.scope,
            credential: request.credential,
            service: PAGES,
            event: "pair",
            contract: "base.software.pair",
        };
        self.pair_error = None;
        match self.pages.contracts.submit(&call, input.clone()).await {
            Ok(_) => {
                let message = if input["clear"].as_bool() == Some(true) {
                    "Pairing key cleared. This instance shares no releases until a key is set again."
                } else {
                    "Pairing key saved. Paste the same key on the instance this one shares releases with."
                };
                self.say(StatusKind::Success, message);
            }
            Err(AgentError::InvalidInput(fields)) => {
                let message = fields
                    .values()
                    .flatten()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "The pairing key was not accepted.".into());
                self.pair_error = Some(message.clone());
                self.say(StatusKind::Error, message);
            }
            Err(AgentError::Forbidden) => self.refused(Error::Forbidden, "key"),
            Err(AgentError::Backend(error)) => self.refused(error, "key"),
            Err(other) => self.refused(Error::Internal, &other.to_string()),
        }
    }
    async fn upload(&mut self, scope: &Scope) -> Result<()> {
        let files = self.uploads.consume(UPLOAD);
        let Some(file) = files.into_iter().next() else {
            self.say(StatusKind::Info, "Choose a release file first.");
            return Ok(());
        };
        match self
            .pages
            .software
            .take_upload(scope, &file.name, file.bytes)
            .await?
        {
            Ok(upload) => {
                let facts = &upload.facts;
                let from = facts["from"]["label"]
                    .as_str()
                    .map_or(String::new(), |label| format!(" from {label}"));
                let release = facts["release"]
                    .as_u64()
                    .map_or("a release".to_owned(), |n| format!("release {n}"));
                self.say(
                    StatusKind::Success,
                    format!(
                        "Received {release}{from} as {}. Install it from the Software page or through an assistant (base.software.install with bundle {}).",
                        upload.bundle, upload.bundle
                    ),
                );
            }
            Err(why) => self.say(StatusKind::Error, why),
        }
        Ok(())
    }
    async fn discard(&mut self, scope: &Scope, bundle: &str) -> Result<()> {
        match self.pages.software.discard_upload(scope, bundle).await? {
            Ok(()) => self.say(StatusKind::Success, format!("Removed {bundle}.")),
            Err(why) => self.say(StatusKind::Error, why),
        }
        Ok(())
    }

    async fn body(&self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let manages = authorize_operator(&**client, scope, MANAGE).await.is_ok();
        let instance = Instance::resolve(&self.pages.settings, &**client).await?;
        drop(client);
        let facts = self.pages.software.releases(scope).await?;
        let banner = instance
            .banner("release-files-instance", "mb-4")
            .map_err(|_| Error::Internal)?;
        let mut cards = Html::default();
        cards.push(&self.here_card(&instance, &facts, manages)?);
        if !instance.role.builds() {
            cards.push(&self.source_card(&instance, &facts)?);
            cards.push(&self.upload_card(&instance, &facts, manages)?);
        }
        cards.push(&self.pairing_card(&instance, &facts, manages)?);
        let message = |kind| {
            self.flash
                .as_ref()
                .filter(|(current, _)| *current == kind)
                .map_or("", |(_, message)| message.as_str())
        };
        Ok(render(ReleaseFilesPage {
            header: render(Header {
                title: Html::text("Release files"),
                subtitle: "Move a tested release between instances: download its file here, upload it on the instance that installs it.",
                ..Header::default()
            })?,
            banner,
            cards,
            flash: FlashMap {
                info: message(StatusKind::Info),
                success: message(StatusKind::Success),
                error: message(StatusKind::Error),
                warning: message(StatusKind::Warning),
            },
            variant: PageVariant::Form,
        })?
        .to_string())
    }
    /// The releases kept on this instance, each with its download link.
    fn here_card(&self, instance: &Instance, facts: &Value, manages: bool) -> Result<Html> {
        let here = &facts["here"];
        let mut table = release_table(
            &self.here,
            "release-files-here-table",
            "grid_here",
            &here["releases"],
            |n| {
                if manages {
                    Some(DOWNLOAD_PATH.replace("{release}", &n.to_string()))
                } else {
                    None
                }
            },
        )?;
        table.caption = "Releases kept here";
        if here["available"].as_bool() == Some(false) {
            table.empty = Some(TableEmpty::Content(Html::text(
                here["message"]
                    .as_str()
                    .unwrap_or("No releases are kept here."),
            )));
        } else if here["releases"].as_array().is_none_or(Vec::is_empty) {
            table.empty = Some(TableEmpty::Content(Html::text(&format!(
                "No release is kept on {} yet. The first one is put in with scripts/install-server.sh release.",
                instance.label
            ))));
        }
        let body = render(HereBody {
            label: &instance.label,
            manages,
            table: render(table)?,
        })?;
        render(Card {
            id: "release-files-here",
            title: "Releases kept here",
            inner_class: "p-0",
            body,
            ..Card::default()
        })
    }
    /// What the source offers, when the instance installs from one.
    fn source_card(&self, instance: &Instance, facts: &Value) -> Result<Html> {
        let source = &facts["source"];
        let mut table = release_table(
            &self.source,
            "release-files-source-table",
            "grid_source",
            &source["releases"],
            |_| None,
        )?;
        table.caption = "Releases on the source";
        if source["available"].as_bool() != Some(true) {
            table.empty = Some(TableEmpty::Content(Html::text(
                source["message"]
                    .as_str()
                    .unwrap_or("The source could not be asked."),
            )));
        } else if source["releases"].as_array().is_none_or(Vec::is_empty) {
            table.empty = Some(TableEmpty::Content(Html::text(
                "The source keeps no release yet.",
            )));
        }
        let name = match &instance.installs_from {
            Some(from) => crate::review::source_name(from),
            None => "no source".to_owned(),
        };
        let healthy = source["available"].as_bool() == Some(true);
        let current = source["current"].as_u64();
        let body = render(SourceBody {
            label: &instance.label,
            name: &name,
            healthy,
            answers: source["healthy"].as_bool() == Some(true),
            current,
            table: render(table)?,
        })?;
        render(Card {
            id: "release-files-source",
            title: "Install from the source",
            inner_class: "p-0",
            body,
            ..Card::default()
        })
    }
    /// The file input and the uploaded files waiting.
    fn upload_card(&self, instance: &Instance, facts: &Value, manages: bool) -> Result<Html> {
        let input = self
            .uploads
            .input(UPLOAD, "release-files-upload")
            .label("Release file")
            .hint("A bilibi-release-<n>.tar.gz downloaded from the instance that tested it, up to 64 MB.");
        let waiting: Vec<Waiting> = facts["uploads"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|upload| {
                let bundle = upload["bundle"].as_str().unwrap_or("").to_owned();
                let release = upload["release"].as_u64();
                let from = upload["from"]["label"].as_str().map(str::to_owned);
                let sha = upload["sha256"].as_str().unwrap_or("").to_owned();
                Waiting {
                    id: format!("release-files-waiting-{}", bundle.replace('_', "-")),
                    heading: match (release, &from) {
                        (Some(n), Some(from)) => format!("{bundle}: release {n} from {from}"),
                        (Some(n), None) => format!("{bundle}: release {n}"),
                        _ => format!("{bundle}: a release file"),
                    },
                    detail: format!(
                        "{}{} · checksum {}…",
                        describe_platform(&upload["platform"]),
                        describe_domains(&upload["domains"]),
                        sha.chars().take(12).collect::<String>()
                    ),
                    bundle,
                }
            })
            .collect();
        let body = render(UploadBody {
            label: &instance.label,
            manages,
            input: render(input)?,
            submit: render(Button {
                id: "release-files-upload-submit",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Receiving…",
                disabled: !manages,
                body: Html::text("Receive the file"),
                ..Button::default()
            })?,
            waiting,
        })?;
        render(Card {
            id: "release-files-upload-card",
            title: "Upload a release file",
            body,
            ..Card::default()
        })
    }
    /// The pairing key: set, cleared, never shown.
    fn pairing_card(&self, instance: &Instance, facts: &Value, manages: bool) -> Result<Html> {
        let pairing = &facts["pairing"];
        let set = pairing["set"].as_bool() == Some(true);
        let mut secret = SecretInput::in_form("pair", "key", "pairing key")
            .id("release-files-pairing-key")
            .label("Pairing key")
            .hint("The same key on every instance that shares releases: 16 to 128 characters, no spaces. Make one with: openssl rand -hex 24");
        if set {
            secret = secret.stored();
        }
        if let Some(error) = &self.pair_error {
            secret = secret.errors(vec![error.clone()]);
        }
        let status = match (set, pairing["set_at"].as_str()) {
            (true, Some(at)) => format!(
                "A pairing key is set (since {}).",
                crate::review::utc_text(at)
            ),
            (true, None) => "A pairing key is set.".to_owned(),
            (false, _) => format!(
                "No pairing key is set: {} shares no releases and downloads none.",
                instance.label
            ),
        };
        let body = render(PairingBody {
            label: &instance.label,
            status: &status,
            set,
            manages,
            secret: render(secret)?,
            save: render(Button {
                id: "release-files-pairing-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Saving…",
                disabled: !manages,
                body: Html::text("Save pairing key"),
                ..Button::default()
            })?,
            clear: render(Button {
                id: "release-files-pairing-clear",
                button_type: "button",
                event: "clear_pairing",
                working: "Clearing…",
                disabled: !manages,
                body: Html::text("Clear the key"),
                ..Button::default()
            })?,
        })?;
        render(Card {
            id: "release-files-pairing",
            title: "Pairing key",
            body,
            ..Card::default()
        })
    }
}
impl LiveState<Scope, Error> for View {
    fn update<'a>(&'a mut self, request: LiveRequest<'a, Scope>) -> LiveFuture<'a, Error> {
        Box::pin(async move {
            {
                let client = self.pages.db.get().await?;
                authorize_operator(&**client, request.scope, VIEW).await?;
            }
            let mut notice = String::new();
            if let LiveInput::Event(value) = request.input {
                let values = bilibi_ui::params::string_values(value);
                let get = |key: &str| values.get(key).map(String::as_str).unwrap_or("");
                match value["type"].as_str().unwrap_or("") {
                    "pair" => {
                        let key = get("pair[key]").trim().to_owned();
                        self.pair(&request, json!({"key": key})).await;
                    }
                    "clear_pairing" => self.pair(&request, json!({"clear": true})).await,
                    "upload_release" => self.upload(request.scope).await?,
                    "discard_upload" => {
                        let bundle = get("value").to_owned();
                        self.discard(request.scope, &bundle).await?;
                    }
                    "grid_here" => {
                        if let ColumnsOutcome::Sort(_) = self.here.handle(&values) {
                            return Err(Error::Invalid);
                        }
                    }
                    "grid_source" => {
                        if let ColumnsOutcome::Sort(_) = self.source.handle(&values) {
                            return Err(Error::Invalid);
                        }
                    }
                    "clear-flash"
                        if self
                            .flash
                            .as_ref()
                            .is_some_and(|(kind, _)| kind.name() == get("key")) =>
                    {
                        self.flash = None;
                    }
                    _ => {}
                }
                if let Some((_, message)) = &self.flash {
                    notice = message.clone();
                }
            }
            Ok(LiveBody {
                html: self.body(request.scope).await?,
                notice,
                ..LiveBody::default()
            })
        })
    }
    fn uploads(&mut self) -> Option<&mut Uploads> {
        Some(&mut self.uploads)
    }
}

/// One table of releases, from a listing's `releases` array.
fn release_table<'a>(
    columns: &'a ListColumns,
    id: &'a str,
    event: &'a str,
    releases: &Value,
    download: impl Fn(u64) -> Option<String>,
) -> Result<FlexTable<'a>> {
    let mut rows = Vec::new();
    for release in releases.as_array().into_iter().flatten() {
        let n = release["release"].as_u64().unwrap_or(0);
        let in_service = release["in_service"].as_bool() == Some(true);
        let sha = release["sha256"].as_str().unwrap_or("").to_owned();
        let file = match download(n) {
            Some(href) => render(ActionLink {
                id: &format!("release-files-download-{n}"),
                icon: "export",
                title: "Download the release file",
                href: &href,
                method: "",
                body: Html::text("Download"),
            })?,
            None => Html::text(&format!(
                "{} MB",
                release["bytes"].as_u64().unwrap_or(0) / 1_048_576
            )),
        };
        let mut row = FlexRow::new(n.to_string())
            .slot("release", Html::text(&n.to_string()))
            .slot(
                "in_service",
                Html::text(if in_service { "Yes" } else { "" }),
            )
            .slot(
                "platform",
                Html::text(&describe_platform(&release["platform"])),
            )
            .slot(
                "domains",
                Html::text(describe_domains(&release["domains"]).trim_start_matches(" with ")),
            )
            .slot(
                "checksum",
                Html::text(&sha.chars().take(12).collect::<String>()),
            )
            .slot("file", file);
        row.dom_id = Some(format!("{id}-{n}"));
        rows.push(row);
    }
    let mut table = FlexTable::new(id, columns.flex_columns(), rows);
    table.event = event;
    table.zoom = columns.zoom;
    table.suggestions = columns.flex_suggestions();
    table.add_query = &columns.add_query;
    table.framed = false;
    Ok(table)
}
fn describe_platform(platform: &Value) -> String {
    match platform["version"].as_str() {
        Some(version) => format!("Bilibi {version}"),
        None => "Not recorded".to_owned(),
    }
}
fn describe_domains(domains: &Value) -> String {
    let list: Vec<String> = domains
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            Some(format!(
                "{} {}",
                d["id"].as_str()?,
                d["version"].as_str().unwrap_or("?")
            ))
        })
        .collect();
    if list.is_empty() {
        String::new()
    } else {
        format!(" with {}", list.join(", "))
    }
}

#[derive(Template)]
#[template(path = "release_files.html")]
struct ReleaseFilesPage<'a> {
    header: Html,
    banner: Html,
    cards: Html,
    flash: FlashMap<'a>,
    variant: PageVariant,
}
#[derive(Template)]
#[template(path = "release_files_here.html")]
struct HereBody<'a> {
    label: &'a str,
    manages: bool,
    table: Html,
}
#[derive(Template)]
#[template(path = "release_files_source.html")]
struct SourceBody<'a> {
    label: &'a str,
    name: &'a str,
    healthy: bool,
    answers: bool,
    current: Option<u64>,
    table: Html,
}
struct Waiting {
    id: String,
    bundle: String,
    heading: String,
    detail: String,
}
#[derive(Template)]
#[template(path = "release_files_upload.html")]
struct UploadBody<'a> {
    label: &'a str,
    manages: bool,
    input: Html,
    submit: Html,
    waiting: Vec<Waiting>,
}
#[derive(Template)]
#[template(path = "release_files_pairing.html")]
struct PairingBody<'a> {
    label: &'a str,
    status: &'a str,
    set: bool,
    manages: bool,
    secret: Html,
    save: Html,
    clear: Html,
}
