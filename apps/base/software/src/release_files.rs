//! The Release files page (`/system/software/releases`) and the release
//! file download: the small UI of release transport (`README.md`, "Release
//! transport"). It shows the releases kept on this instance with a download
//! link for each, the pairing key, what the source offers, and takes an
//! uploaded release file. Installing is the Software page's (`web.rs`) and the
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
    ActionLink, BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome, Fact, Facts,
    FlashGroup, FlashMap, FlexRow, FlexTable, Form, Header, Html, ListColumns, PageVariant,
    SectionHeading, StatusKind, TableEmpty, UploadSpec,
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
            let mut view = View::new(self.clone())?;
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

/// The mounted page: the file input's state, the three tables' columns, the
/// source's answer and the last message.
struct View {
    pages: Pages,
    uploads: Uploads,
    here: ListColumns,
    source: ListColumns,
    waiting: ListColumns,
    /// What the source reported when this page was mounted. Showing the page
    /// again does not ask it again (the source limits how often one sender
    /// may); the person asks again with "Refresh releases", and saving
    /// the pairing key forgets it, since the answer then differs.
    asked: Option<Value>,
    flash: Option<(StatusKind, String)>,
    pair_error: Option<String>,
}
fn release_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("release", "Release").right(),
        BuiltinColumn::new("in_service", "Running now"),
        BuiltinColumn::new("platform", "Bilibi"),
        BuiltinColumn::new("domains", "Domains"),
        BuiltinColumn::new("checksum", "File checksum"),
        BuiltinColumn::new("file", "File"),
    ]
}
fn waiting_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("bundle", "File"),
        BuiltinColumn::new("release", "Release").right(),
        BuiltinColumn::new("from", "Made on"),
        BuiltinColumn::new("platform", "Bilibi"),
        BuiltinColumn::new("domains", "Domains"),
        BuiltinColumn::new("checksum", "File checksum"),
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
            waiting: ListColumns::mount("release-files-waiting", waiting_columns())
                .map_err(|_| Error::Internal)?,
            asked: None,
            flash: None,
            pair_error: None,
        })
    }
    fn say(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
    }
    /// The page's own answer to a refused contract call.
    fn refused(&mut self, error: Error) {
        let message = match error {
            Error::Forbidden => "You do not have permission to change the pairing key.".to_owned(),
            Error::Invalid => "The pairing key was not accepted.".to_owned(),
            other => format!("The pairing key could not be saved ({other:?})."),
        };
        self.pair_error = Some(message.clone());
        self.say(StatusKind::Error, message);
    }
    /// Sets or clears the pairing key. A step-up the owner asks for ends the
    /// event: the host takes the person to confirm and back, and nothing was
    /// changed.
    async fn pair(
        &mut self,
        request: &LiveRequest<'_, Scope>,
        event: &str,
        input: Value,
    ) -> Result<()> {
        let call = bilibi_agent_api::page::Call {
            scope: request.scope,
            credential: request.credential,
            service: PAGES,
            event,
            contract: "base.software.pair",
        };
        self.pair_error = None;
        match self.pages.contracts.submit(&call, input.clone()).await {
            Ok(_) => {
                self.asked = None;
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
            Err(AgentError::Backend(error @ Error::StepUpRequired { .. })) => return Err(error),
            Err(AgentError::Forbidden) => self.refused(Error::Forbidden),
            Err(AgentError::Backend(error)) => self.refused(error),
            Err(_) => self.refused(Error::Internal),
        }
        Ok(())
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

    async fn body(&mut self, scope: &Scope) -> Result<String> {
        let client = self.pages.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let manages = authorize_operator(&**client, scope, MANAGE).await.is_ok();
        let instance = Instance::resolve(&self.pages.settings, &**client).await?;
        drop(client);
        let facts = self
            .pages
            .software
            .releases_with(scope, self.asked.clone())
            .await?;
        if self.asked.is_none() {
            self.asked = Some(facts["source"].clone());
        }
        let mut body = render(Header {
            title: Html::text("Release files"),
            subtitle:
                "Download a tested Bilibi update or upload one to install from the Software page.",
            ..Header::default()
        })?;
        body.push(
            &instance
                .banner("release-files-instance", "mb-2")
                .map_err(|_| Error::Internal)?,
        );
        body.push(&self.here_section(&instance, &facts, manages)?);
        body.push(&self.source_section(&instance, &facts)?);
        if manages {
            body.push(&self.upload_section(&instance)?);
        }
        body.push(&self.waiting_section(&facts, manages)?);
        body.push(&self.pairing_section(&instance, &facts, manages)?);
        let message = |kind| {
            self.flash
                .as_ref()
                .filter(|(current, _)| *current == kind)
                .map_or("", |(_, message)| message.as_str())
        };
        let mut page = render(FlashGroup {
            id: "flash-group",
            flash: FlashMap {
                info: message(StatusKind::Info),
                success: message(StatusKind::Success),
                error: message(StatusKind::Error),
                warning: message(StatusKind::Warning),
            },
        })?;
        page.push(&render(bilibi_ui::Page {
            variant: PageVariant::Form,
            body,
            ..bilibi_ui::Page::default()
        })?);
        Ok(page.to_string())
    }
    /// The releases kept on this instance, each with its download link.
    fn here_section(&self, instance: &Instance, facts: &Value, manages: bool) -> Result<Html> {
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
        table.caption = "Stored releases";
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
        let description = if manages {
            format!("{} keeps these updates. Download the one running now to install it on another Bilibi server.", instance.label)
        } else {
            format!("{} keeps these releases.", instance.label)
        };
        section(
            "release-files-here",
            "Stored releases",
            &description,
            Html::default(),
            true,
            render(table)?,
        )
    }
    /// What the source offers, when the instance installs from one.
    fn source_section(&self, instance: &Instance, facts: &Value) -> Result<Html> {
        let source = &facts["source"];
        let mut table = release_table(
            &self.source,
            "release-files-source-table",
            "grid_source",
            &source["releases"],
            |_| None,
        )?;
        table.caption = "Updates on the update server";
        if source["available"].as_bool() != Some(true) {
            table.empty = Some(TableEmpty::Content(Html::text(
                source["message"]
                    .as_str()
                    .unwrap_or("Bilibi could not contact the update server."),
            )));
        } else if source["releases"].as_array().is_none_or(Vec::is_empty) {
            table.empty = Some(TableEmpty::Content(Html::text(
                "The update server has no release yet.",
            )));
        }
        let name = match &instance.installs_from {
            Some(from) => crate::review::source_name(from),
            None => "no update server".to_owned(),
        };
        let mut description = match &instance.installs_from {
            Some(_) => format!("{} gets tested updates from {name}.", instance.label),
            None => format!("{} has no update server chosen. Choose one in Settings › Instance › Install releases tested on, or upload a tested release file below.", instance.label),
        };
        if source["available"].as_bool() == Some(true) {
            match source["current"].as_u64() {
                Some(n) => description.push_str(&format!(
                    " It runs release {n} now{}.",
                    if source["healthy"].as_bool() == Some(true) {
                        " and is responding"
                    } else {
                        ", but its program does not answer"
                    }
                )),
                None => description.push_str(" It has no release running."),
            }
            description.push_str(" Install the running release from the Software page.");
        }
        let ask = render(Button {
            id: "release-files-source-refresh",
            button_type: "button",
            event: "refresh_source",
            working: "Asking…",
            body: Html::text("Refresh releases"),
            ..Button::default()
        })?;
        section(
            "release-files-source",
            "Update server",
            &description,
            ask,
            true,
            render(table)?,
        )
    }
    /// The file input for a release file, for a server owner.
    fn upload_section(&self, instance: &Instance) -> Result<Html> {
        let input = self
            .uploads
            .input(UPLOAD, "release-files-upload")
            .label("Release file")
            .hint("Choose a tested bilibi-release-<n>.tar.gz file, up to 64 MB.");
        let form = render(Form {
            id: "release-files-upload-form",
            event: "upload_release",
            body: render(input)?,
            actions: render(Button {
                id: "release-files-upload-submit",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Receiving…",
                body: Html::text("Upload file"),
                ..Button::default()
            })?,
            ..Form::default()
        })?;
        let description = format!("Upload a tested update for {}. Bilibi checks the file; you install it separately from the Software page.", instance.label);
        section(
            "release-files-upload-card",
            "Upload a release file",
            &description,
            Html::default(),
            false,
            form,
        )
    }
    /// The uploaded release files waiting for an install.
    fn waiting_section(&self, facts: &Value, manages: bool) -> Result<Html> {
        let mut rows = Vec::new();
        for upload in facts["uploads"].as_array().into_iter().flatten() {
            let bundle = upload["bundle"].as_str().unwrap_or("").to_owned();
            let sha = upload["sha256"].as_str().unwrap_or("");
            let id = format!("release-files-waiting-{}", bundle.replace('_', "-"));
            let remove = if manages {
                render(Button {
                    id: &format!("{id}-remove"),
                    button_type: "button",
                    event: "discard_upload",
                    name: "value",
                    value: &bundle,
                    working: "Removing…",
                    body: Html::text("Remove"),
                    ..Button::default()
                })?
            } else {
                Html::default()
            };
            let mut row = FlexRow::new(bundle.clone())
                .slot("bundle", Html::text(&bundle))
                .slot(
                    "release",
                    Html::text(
                        &upload["release"]
                            .as_u64()
                            .map_or(String::new(), |n| n.to_string()),
                    ),
                )
                .slot(
                    "from",
                    Html::text(upload["from"]["label"].as_str().unwrap_or("")),
                )
                .slot(
                    "platform",
                    Html::text(&describe_platform(&upload["platform"])),
                )
                .slot(
                    "domains",
                    Html::text(describe_domains(&upload["domains"]).trim_start_matches(" with ")),
                )
                .slot(
                    "checksum",
                    Html::text(&sha.chars().take(12).collect::<String>()),
                );
            row.dom_id = Some(id);
            row.actions = remove;
            rows.push(row);
        }
        let mut table = FlexTable::new(
            "release-files-waiting-table",
            self.waiting.flex_columns(),
            rows,
        );
        table.event = "grid_waiting";
        table.zoom = self.waiting.zoom;
        table.suggestions = self.waiting.flex_suggestions();
        table.add_query = &self.waiting.add_query;
        table.framed = false;
        table.actions = manages;
        table.caption = "Uploaded releases";
        table.empty = Some(TableEmpty::Content(Html::text(
            "No release file is waiting here.",
        )));
        section(
            "release-files-waiting-card",
            "Uploaded releases",
            "Choose one of these files on the Software page to review and install it.",
            Html::default(),
            true,
            render(table)?,
        )
    }
    /// The pairing key: set, cleared, never shown.
    fn pairing_section(&self, instance: &Instance, facts: &Value, manages: bool) -> Result<Html> {
        let pairing = &facts["pairing"];
        let set = pairing["set"].as_bool() == Some(true);
        let status = if set {
            "Saved securely. You can replace or clear it below.".to_owned()
        } else {
            "Not set".to_owned()
        };
        let mut body = render(Facts {
            id: "release-files-pairing-facts",
            items: vec![Fact {
                title: "Status",
                id: "release-files-pairing-status",
                value: Html::text(&status),
            }],
        })?;
        if manages {
            let mut secret = SecretInput::in_form("pair", "key", "pairing key")
                .id("release-files-pairing-key")
                .label("Pairing key")
                .hint("Use the same secret on both Bilibi servers: 16 to 128 characters, no spaces. Saving or clearing it requires your authenticator code.");
            if set {
                secret = secret.stored();
            }
            if let Some(error) = &self.pair_error {
                secret = secret.errors(vec![error.clone()]);
            }
            let mut actions = render(Button {
                id: "release-files-pairing-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Saving…",
                body: Html::text("Save pairing key"),
                ..Button::default()
            })?;
            if set {
                actions.push(&render(Button {
                    id: "release-files-pairing-clear",
                    button_type: "button",
                    event: "clear_pairing",
                    working: "Clearing…",
                    body: Html::text("Clear the key"),
                    ..Button::default()
                })?);
            }
            body.push(&render(Form {
                id: "release-files-pairing-form",
                event: "pair",
                class: "mt-3",
                body: render(secret)?,
                actions,
                ..Form::default()
            })?);
        }
        let mut description = format!("This secret lets {} and its update server share release files. Save the same secret on both servers.", instance.label);
        if !set {
            description.push_str(&format!(
                " Without a key, {} cannot share release files automatically. You can still upload a file.",
                instance.label
            ));
        }
        section(
            "release-files-pairing",
            "Pairing key",
            &description,
            Html::default(),
            false,
            body,
        )
    }
}
/// One section: its heading with a short description, then its card.
fn section(
    id: &str,
    title: &str,
    description: &str,
    actions: Html,
    flat: bool,
    body: Html,
) -> Result<Html> {
    let heading = format!("{id}-heading");
    let mut section = render(SectionHeading {
        title,
        id: &heading,
        class: "mt-6",
        description,
        actions,
        ..SectionHeading::default()
    })?;
    section.push(&render(Card {
        id,
        inner_class: if flat { "p-0" } else { "" },
        body,
        ..Card::default()
    })?);
    Ok(section)
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
                        self.pair(&request, "pair", json!({"key": key})).await?;
                    }
                    "clear_pairing" => {
                        self.pair(&request, "clear_pairing", json!({"clear": true}))
                            .await?
                    }
                    "refresh_source" => self.asked = None,
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
                    "grid_waiting" => {
                        if let ColumnsOutcome::Sort(_) = self.waiting.handle(&values) {
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
