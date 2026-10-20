//! Release controls embedded in the Software page’s Releases tab and the
//! authenticated release download. No Bilimbi counterpart.
use crate::{transport, web::PAGES, Software, MANAGE, VIEW};
use askama::Template;
use bilibi_agent_api::Error as AgentError;
use bilibi_authz::authorize_operator;
use bilibi_database::{Error, Result};
use bilibi_system::Instance;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    download::{Download, DownloadFuture, DownloadRequest, FileBody},
    forms::{Field, InputKind, SecretInput},
    page::{LiveInput, LiveRequest, Uploads},
    ActionLink, BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome, Fact, Facts, FlexRow,
    FlexTable, Fold, Form, Html, ListColumns, SectionHeading, StatusKind, TableEmpty, UploadSpec,
};
use serde_json::{json, Value};
use std::sync::Arc;

pub const PATH: &str = "/system/software/releases";
pub const DOWNLOAD_PATH: &str = "/system/software/releases/{release}/download";
pub(crate) const FILES: &str = "base/software.release_files";
/// The file input's name: the key the owner consumes by.
const UPLOAD: &str = "release_file";
/// A release file is a compressed program: tens of megabytes.
const UPLOAD_BYTES: u64 = bilibi_ui::upload::MAX_FILE_SIZE;

fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
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
pub(super) struct ReleaseFiles {
    pages: crate::web::Pages,
    pub(super) uploads: Uploads,
    here: ListColumns,
    source: ListColumns,
    waiting: ListColumns,
    /// What the source reported when this page was mounted. Showing the page
    /// again does not ask it again (the source limits how often one sender
    /// may); the person asks again with "Refresh releases", and saving
    /// the pairing key forgets it, since the answer then differs.
    asked: Option<Value>,
    github_columns: ListColumns,
    /// Include GitHub files marked as a pre-release. Drafts stay hidden.
    github_prerelease: bool,
    /// The next render asks GitHub now, unless GitHub is limiting this server.
    github_refresh: bool,
    github_error: Option<String>,
    pub(super) flash: Option<(StatusKind, String)>,
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
fn github_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("tag", "Tag"),
        BuiltinColumn::new("target", "For"),
        BuiltinColumn::new("file", "File"),
        BuiltinColumn::new("digest", "Check code"),
        BuiltinColumn::new("published", "Published"),
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
impl ReleaseFiles {
    pub(super) fn new(pages: crate::web::Pages) -> Result<Self> {
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
            github_columns: ListColumns::mount("release-files-github", github_columns())
                .map_err(|_| Error::Internal)?,
            github_prerelease: false,
            github_refresh: false,
            github_error: None,
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
    /// Saves the GitHub repository, token or refresh interval. A step-up the
    /// owner asks for ends the event: the host takes the person to confirm
    /// and back, and nothing was changed.
    async fn github_save(
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
            contract: "base.software.github",
        };
        self.github_error = None;
        match self.pages.contracts.submit(&call, input.clone()).await {
            Ok(_) => {
                self.github_refresh = true;
                let message = if input["accept"].as_bool() == Some(true) {
                    "GitHub repository accepted. You can review a release below."
                } else if input["clear_token"].as_bool() == Some(true) {
                    "GitHub read token cleared."
                } else if input.get("token").and_then(Value::as_str).is_some() {
                    "GitHub read token saved. It is not shown again."
                } else {
                    "GitHub settings saved."
                };
                self.say(StatusKind::Success, message);
            }
            Err(AgentError::InvalidInput(fields)) => {
                let message = fields
                    .values()
                    .flatten()
                    .next()
                    .cloned()
                    .unwrap_or_else(|| "The GitHub settings were not accepted.".into());
                self.github_error = Some(message.clone());
                self.say(StatusKind::Error, message);
            }
            Err(AgentError::Backend(error @ Error::StepUpRequired { .. })) => return Err(error),
            Err(AgentError::Forbidden) => {
                let message = "You do not have permission to change the GitHub repository.";
                self.github_error = Some(message.into());
                self.say(StatusKind::Error, message);
            }
            Err(AgentError::Backend(error)) => {
                let message = format!("The GitHub settings could not be saved ({error:?}).");
                self.github_error = Some(message.clone());
                self.say(StatusKind::Error, message);
            }
            Err(_) => {
                let message = "The GitHub settings could not be saved.";
                self.github_error = Some(message.into());
                self.say(StatusKind::Error, message);
            }
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
                        "Received {release}{from} as {}. Choose it below to review and install it.",
                        upload.bundle
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

    pub(super) async fn body(&mut self, scope: &Scope, install: Html) -> Result<Html> {
        let client = self.pages.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let manages = authorize_operator(&**client, scope, MANAGE).await.is_ok();
        let instance = Instance::resolve(&self.pages.settings, &**client).await?;
        drop(client);
        let refresh_github = std::mem::take(&mut self.github_refresh);
        let facts = self
            .pages
            .software
            .releases_with(
                scope,
                self.asked.clone(),
                refresh_github,
                self.github_prerelease,
            )
            .await?;
        if self.asked.is_none() {
            self.asked = Some(facts["source"].clone());
        }
        let enrolled = self.pages.software.step_up.enrolled(scope).await?;
        let mut body = Html::EMPTY;
        body.push(&self.github_section(&facts, manages, enrolled)?);
        if manages {
            body.push(&self.upload_section(&instance)?);
        }
        if facts["uploads"]
            .as_array()
            .is_some_and(|uploads| !uploads.is_empty())
        {
            body.push(&self.waiting_section(&facts, manages)?);
        }
        body.push(&install);
        body.push(&self.here_section(&instance, &facts, manages)?);
        let mut source = self.source_section(&instance, &facts)?;
        if manages {
            source.push(&render(ActionLink {
                id: "software-update-server-settings",
                href: "/system/settings#setting-instance-installs_from",
                title: "Choose the update server in Instance settings",
                body: Html::text("Update server settings"),
                ..ActionLink::default()
            })?);
        }
        source.push(&self.pairing_section(&instance, &facts, manages, enrolled)?);
        body.push(&render(Fold {
            id: "software-update-server",
            title: "Update server and pairing key",
            open: self.pair_error.is_some(),
            body: source,
            ..Fold::default()
        })?);
        Ok(body)
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
        let mut body = Html::EMPTY;
        if manages {
            if let Some(current) = here["current"].as_u64() {
                body.push(&render(ActionLink {
                    id: "software-download-current",
                    icon: "export",
                    title: "Download this server’s release file for another Bilibi server",
                    href: &DOWNLOAD_PATH.replace("{release}", &current.to_string()),
                    body: Html::text("Download this server’s release file"),
                    ..ActionLink::default()
                })?);
            }
        }
        if here["releases"]
            .as_array()
            .is_some_and(|releases| !releases.is_empty())
        {
            body.push(&render(Fold {
                id: "software-stored-releases",
                title: "Stored releases",
                body: render(table)?,
                ..Fold::default()
            })?);
        } else {
            body.push(&Html::text(
                here["message"]
                    .as_str()
                    .unwrap_or("No release file is stored on this server yet."),
            ));
        }
        render(Card {
            id: "release-files-here",
            title: "Release download",
            body,
            ..Card::default()
        })
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
            None => format!("{} has no update server chosen. Choose one in Settings › Instance › Install releases tested on, or upload a tested release file.", instance.label),
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
        let description = format!(
            "Upload a tested update for {}, then review and install it below.",
            instance.label
        );
        section(
            "release-files-upload-card",
            "Release upload",
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
            "Choose a file below to review and install it.",
            Html::default(),
            true,
            render(table)?,
        )
    }
    /// Setup files from the GitHub repository the owner named.
    fn github_section(&self, facts: &Value, manages: bool, enrolled: bool) -> Result<Html> {
        let github = &facts["github"];
        let repository = github["repository"].as_str().unwrap_or("");
        let accepted = github["accepted"].as_bool() == Some(true);
        let identity_changed = github["identity_changed"].as_bool() == Some(true);
        let token_set = github["token_set"].as_bool() == Some(true);
        let minutes = github["refresh_minutes"].as_i64().unwrap_or(15);
        let mut body = render(Facts {
            id: "release-files-github-facts",
            items: vec![
                Fact {
                    title: "Repository",
                    id: "release-files-github-repository-fact",
                    value: Html::text(if repository.is_empty() {
                        "Not set"
                    } else {
                        repository
                    }),
                },
                Fact {
                    title: "Accepted",
                    id: "release-files-github-accepted",
                    value: Html::text(if accepted { "Yes" } else { "No" }),
                },
                Fact {
                    title: "Read token",
                    id: "release-files-github-token-fact",
                    value: Html::text(if token_set { "Saved" } else { "Not set" }),
                },
                Fact {
                    title: "Check every",
                    id: "release-files-github-refresh-fact",
                    value: Html::text(&format!(
                        "{minutes} minute{}",
                        if minutes == 1 { "" } else { "s" }
                    )),
                },
            ],
        })?;
        let mut notice = github["message"].as_str().unwrap_or("").to_owned();
        if github["stale"].as_bool() == Some(true) {
            if !notice.is_empty() {
                notice.push(' ');
            }
            notice.push_str("This list was saved earlier. GitHub could not be asked just now.");
        }
        if !notice.is_empty() {
            body.push(&Html::text(&notice));
        }
        if manages {
            let mut repository_field = Field::in_form("github", "repository")
                .id("release-files-github-repository")
                .label("GitHub repository")
                .value(repository)
                .hint("owner/name, like BelimbingApp/bilibi-setup-template. Saving or accepting asks for your authenticator code.")
                .wrapper_class("mb-0 w-full sm:w-96");
            if let Some(error) = &self.github_error {
                repository_field = repository_field.error(error.clone());
            }
            let mut secret = SecretInput::in_form("github", "token", "GitHub read token")
                .id("release-files-github-token")
                .label("GitHub read token")
                .hint("Leave empty for a public repository. For a private one, use a fine-grained token that can read this repository only.");
            if token_set {
                secret = secret.stored();
            }
            let minutes_field = Field::in_form("github", "refresh_minutes")
                .id("release-files-github-minutes")
                .label("Check GitHub every this many minutes")
                .kind(InputKind::Number)
                .value(minutes.to_string())
                .hint("15 means the list is refreshed at most every 15 minutes, and again when you choose Refresh.")
                .wrapper_class("mb-0 w-full sm:w-72")
                .attr("min", "1")
                .attr("max", "1440");
            let mut fields = render(repository_field)?;
            fields.push(&render(secret)?);
            fields.push(&render(minutes_field)?);
            let mut actions = render(Button {
                id: "release-files-github-save",
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Saving…",
                body: Html::text("Save"),
                ..Button::default()
            })?;
            if token_set {
                actions.push(&render(Button {
                    id: "release-files-github-clear-token",
                    button_type: "button",
                    event: "clear_github_token",
                    working: "Clearing…",
                    body: Html::text("Clear read token"),
                    ..Button::default()
                })?);
            }
            let mut step = bilibi_ui::StepUp::new(enrolled, "/system/software?tab=releases");
            step.body = fields;
            step.actions = actions;
            body.push(&render(Form {
                id: "release-files-github-form",
                event: "github_source",
                class: "mt-3",
                body: render(step)?,
                ..Form::default()
            })?);
            if (!accepted || identity_changed) && github["repository_id"].as_i64().is_some() {
                let repository_id = github["repository_id"].as_i64().unwrap_or(0).to_string();
                let mut hidden = render(
                    Field::named("repository")
                        .id("release-files-github-accept-repository")
                        .kind(InputKind::Hidden)
                        .value(repository),
                )?;
                hidden.push(&render(
                    Field::named("repository_id")
                        .id("release-files-github-accept-id")
                        .kind(InputKind::Hidden)
                        .value(repository_id),
                )?);
                hidden.push(&render(
                    Field::named("accept")
                        .id("release-files-github-accept-flag")
                        .kind(InputKind::Hidden)
                        .value("true"),
                )?);
                let accept_step = {
                    let mut step =
                        bilibi_ui::StepUp::new(enrolled, "/system/software?tab=releases");
                    step.body = hidden;
                    step.actions = render(Button {
                        id: "release-files-github-accept",
                        button_type: "submit",
                        variant: ButtonVariant::Primary,
                        working: "Accepting…",
                        body: Html::text("Accept this repository"),
                        ..Button::default()
                    })?;
                    step
                };
                body.push(&render(Form {
                    id: "release-files-github-accept-form",
                    event: "github_accept",
                    class: "mt-3",
                    body: render(accept_step)?,
                    ..Form::default()
                })?);
            }
        }
        body.push(&self.github_table(
            github,
            manages && accepted && !identity_changed,
            github["repository_id"].as_i64().unwrap_or(0),
        )?);
        let mut actions = render(Button {
            id: "release-files-github-refresh",
            button_type: "button",
            event: "refresh_github",
            working: "Checking…",
            body: Html::text("Refresh"),
            ..Button::default()
        })?;
        actions.push(&render(Button {
            id: "release-files-github-prerelease",
            button_type: "button",
            event: "github_prerelease",
            body: Html::text(if self.github_prerelease {
                "Show stable releases"
            } else {
                "Show pre-releases"
            }),
            ..Button::default()
        })?);
        section(
            "release-files-github",
            "GitHub releases",
            "Install a setup file from the GitHub repository you name. Uploading a file still works when GitHub cannot be reached.",
            actions,
            false,
            body,
        )
    }
    fn github_table(&self, github: &Value, can_review: bool, repository_id: i64) -> Result<Html> {
        let mut rows = Vec::new();
        for release in github["releases"].as_array().into_iter().flatten() {
            let asset = release["asset_id"].as_i64().unwrap_or(0);
            let digest = release["digest"].as_str().unwrap_or("");
            let published = release["published_at"]
                .as_str()
                .unwrap_or("")
                .chars()
                .take(10)
                .collect::<String>();
            let tag = release["tag"].as_str().unwrap_or("");
            let tag_text = if release["prerelease"].as_bool() == Some(true) {
                format!("{tag} (pre-release)")
            } else {
                tag.to_owned()
            };
            let mut row = FlexRow::new(asset.to_string())
                .slot("tag", Html::text(&tag_text))
                .slot(
                    "target",
                    Html::text(crate::github::describe_target(
                        release["target"].as_str().unwrap_or(""),
                    )),
                )
                .slot("file", Html::text(release["name"].as_str().unwrap_or("")))
                .slot(
                    "digest",
                    Html::text(&digest.chars().take(12).collect::<String>()),
                )
                .slot("published", Html::text(&published));
            row.dom_id = Some(format!("release-files-github-row-{asset}"));
            if can_review {
                row.actions = self.github_review_form(release, repository_id)?;
            }
            rows.push(row);
        }
        let empty_list = rows.is_empty();
        let mut table = FlexTable::new(
            "release-files-github-table",
            self.github_columns.flex_columns(),
            rows,
        );
        table.event = "grid_github";
        table.zoom = self.github_columns.zoom;
        table.suggestions = self.github_columns.flex_suggestions();
        table.add_query = &self.github_columns.add_query;
        table.framed = false;
        table.actions = can_review;
        table.caption = "GitHub releases";
        if empty_list {
            let empty = github["message"].as_str().unwrap_or(
                if github["repository"]
                    .as_str()
                    .is_some_and(|name| !name.is_empty())
                {
                    "No setup file for this server is in the list."
                } else {
                    "No GitHub repository is set."
                },
            );
            table.empty = Some(TableEmpty::Content(Html::text(empty)));
        }
        render(table)
    }
    fn github_review_form(&self, release: &Value, repository_id: i64) -> Result<Html> {
        let asset = release["asset_id"].as_i64().unwrap_or(0);
        let pairs = [
            ("asset", asset.to_string()),
            ("repository_id", repository_id.to_string()),
            (
                "release_id",
                release["release_id"].as_i64().unwrap_or(0).to_string(),
            ),
            ("tag", release["tag"].as_str().unwrap_or("").to_owned()),
            (
                "target",
                release["target"].as_str().unwrap_or("").to_owned(),
            ),
            (
                "digest",
                release["digest"].as_str().unwrap_or("").to_owned(),
            ),
        ];
        let mut body = Html::EMPTY;
        for (name, value) in pairs {
            body.push(&render(
                Field::named(name)
                    .id(format!("release-files-github-{asset}-{name}"))
                    .kind(InputKind::Hidden)
                    .value(value),
            )?);
        }
        let form_id = format!("release-files-github-review-form-{asset}");
        let button_id = format!("release-files-github-review-{asset}");
        render(Form {
            id: &form_id,
            event: "request_install",
            body,
            actions: render(Button {
                id: &button_id,
                button_type: "submit",
                variant: ButtonVariant::Primary,
                working: "Reviewing…",
                body: Html::text("Review"),
                ..Button::default()
            })?,
            ..Form::default()
        })
    }
    /// The pairing key: set, cleared, never shown.
    fn pairing_section(
        &self,
        instance: &Instance,
        facts: &Value,
        manages: bool,
        enrolled: bool,
    ) -> Result<Html> {
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
            let mut step = bilibi_ui::StepUp::new(enrolled, "/system/software?tab=releases");
            step.body = render(secret)?;
            step.actions = actions;
            body.push(&render(Form {
                id: "release-files-pairing-form",
                event: "pair",
                class: "mt-3",
                body: render(step)?,
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

impl ReleaseFiles {
    /// Handles transport controls within the one mounted Software page.
    pub(super) async fn event(&mut self, request: &LiveRequest<'_, Scope>) -> Result<bool> {
        {
            let client = self.pages.db.get().await?;
            authorize_operator(&**client, request.scope, VIEW).await?;
        }
        if let LiveInput::Event(value) = request.input {
            let values = bilibi_ui::params::string_values(value);
            let get = |key: &str| values.get(key).map(String::as_str).unwrap_or("");
            match value["type"].as_str().unwrap_or("") {
                "pair" => {
                    let key = get("pair[key]").trim().to_owned();
                    self.pair(request, "pair", json!({"key": key})).await?;
                }
                "clear_pairing" => {
                    self.pair(request, "clear_pairing", json!({"clear": true}))
                        .await?
                }
                "refresh_source" => self.asked = None,
                "github_source" => {
                    let repository = get("github[repository]").trim().to_owned();
                    let mut input = json!({"repository": repository});
                    if let Ok(minutes) = get("github[refresh_minutes]").trim().parse::<i64>() {
                        input["refresh_minutes"] = json!(minutes);
                    } else if !get("github[refresh_minutes]").trim().is_empty() {
                        self.github_error = Some(
                            "Enter how many minutes to wait between GitHub checks, from 1 to 1440. 15 checks about four times an hour."
                                .into(),
                        );
                        self.say(
                            StatusKind::Error,
                            "Enter how many minutes to wait between GitHub checks, from 1 to 1440. 15 checks about four times an hour.",
                        );
                        return Ok(true);
                    }
                    let token = get("github[token]").trim().to_owned();
                    if !token.is_empty() {
                        input["token"] = json!(token);
                    }
                    self.github_save(request, "github_source", input).await?;
                }
                "github_accept" => {
                    let mut input = json!({
                        "accept": true,
                        "repository": get("repository").trim(),
                    });
                    if let Ok(id) = get("repository_id").trim().parse::<i64>() {
                        input["repository_id"] = json!(id);
                    }
                    self.github_save(request, "github_accept", input).await?;
                }
                "clear_github_token" => {
                    self.github_save(request, "clear_github_token", json!({"clear_token": true}))
                        .await?;
                }
                "refresh_github" => self.github_refresh = true,
                "github_prerelease" => self.github_prerelease = !self.github_prerelease,
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
                "grid_github" => {
                    if let ColumnsOutcome::Sort(_) = self.github_columns.handle(&values) {
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
                _ => return Ok(false),
            }
        }
        Ok(true)
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
