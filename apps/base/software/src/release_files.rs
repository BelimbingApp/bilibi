//! Release controls embedded in the Software page’s Releases tab and the
//! manual upload. No Bilimbi counterpart.
use crate::{web::PAGES, MANAGE, VIEW};
use askama::Template;
use bilibi_agent_api::Error as AgentError;
use bilibi_authz::authorize_operator;
use bilibi_database::{Error, Result};
use bilibi_system::Instance;
use bilibi_tenancy::Scope;
use bilibi_ui::{
    forms::{Field, InputKind, SecretInput},
    page::{LiveInput, LiveRequest, Uploads},
    BuiltinColumn, Button, ButtonVariant, Card, ColumnsOutcome, Fact, Facts, FlexRow, FlexTable,
    Fold, Form, Html, ListColumns, SectionHeading, StatusKind, TableEmpty, UploadSpec,
};
use serde_json::{json, Value};

pub const PATH: &str = "/system/software/releases";
/// The file input's name: the key the owner consumes by.
const UPLOAD: &str = "release_file";
/// A release file is a compressed program: tens of megabytes.
const UPLOAD_BYTES: u64 = bilibi_ui::upload::MAX_FILE_SIZE;

fn render(template: impl Template) -> Result<Html> {
    Html::render(&template).map_err(|_| Error::Internal)
}

/// Release-tab state: upload input, list columns, GitHub controls and feedback.
pub(super) struct ReleaseFiles {
    pages: crate::web::Pages,
    pub(super) uploads: Uploads,
    waiting: ListColumns,
    github_columns: ListColumns,
    /// Include GitHub files marked as a pre-release. Drafts stay hidden.
    github_prerelease: bool,
    /// The next render asks GitHub now, unless GitHub is limiting this server.
    github_refresh: bool,
    github_error: Option<String>,
    pub(super) flash: Option<(StatusKind, String)>,
}
fn github_columns() -> Vec<BuiltinColumn> {
    vec![
        BuiltinColumn::new("tag", "Tag"),
        BuiltinColumn::new("target", "For"),
        BuiltinColumn::new("file", "File"),
        BuiltinColumn::new("asset", "GitHub asset"),
        BuiltinColumn::new("installed", "Installed"),
        BuiltinColumn::new("digest", "Archive SHA-256"),
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
            waiting: ListColumns::mount("release-files-waiting", waiting_columns())
                .map_err(|_| Error::Internal)?,
            github_columns: ListColumns::mount("release-files-github", github_columns())
                .map_err(|_| Error::Internal)?,
            github_prerelease: false,
            github_refresh: false,
            github_error: None,
            flash: None,
        })
    }
    fn say(&mut self, kind: StatusKind, message: impl Into<String>) {
        self.flash = Some((kind, message.into()));
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
            .releases_with(scope, refresh_github, self.github_prerelease)
            .await?;
        let enrolled = self.pages.software.step_up.enrolled(scope).await?;
        let mut body = Html::EMPTY;
        body.push(&self.github_section(&facts, manages, enrolled)?);
        let mut upload = Html::EMPTY;
        if manages {
            upload.push(&self.upload_section(&instance)?);
        }
        if facts["uploads"]
            .as_array()
            .is_some_and(|uploads| !uploads.is_empty())
        {
            upload.push(&self.waiting_section(&facts, manages)?);
        }
        upload.push(&install);
        body.push(&render(Fold {
            id: "software-offline-upload",
            title: "Upload a release file",
            open: facts["uploads"]
                .as_array()
                .is_some_and(|uploads| !uploads.is_empty()),
            body: upload,
            ..Fold::default()
        })?);
        Ok(body)
    }
    /// The file input for a release file, for a server owner.
    fn upload_section(&self, instance: &Instance) -> Result<Html> {
        let input = self
            .uploads
            .input(UPLOAD, "release-files-upload")
            .label("Release file")
            .hint("Choose a tested .tar.gz release file, up to 64 MB.");
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
        let installed = &facts["installed_asset"];
        body.push(&render(Facts {
            id: "software-installed-asset",
            items: vec![
                Fact {
                    id: "software-installed-local-release",
                    title: "Local release",
                    value: Html::text(
                        &installed["local_release"]
                            .as_u64()
                            .map_or("Not recorded".into(), |n| n.to_string()),
                    ),
                },
                Fact {
                    id: "software-installed-github-asset",
                    title: "Installed GitHub asset",
                    value: Html::text(&if installed["github"].is_object() {
                        let pin = &installed["github"];
                        format!(
                            "{} · tag {} · repository {} / release {} / asset {} · {}",
                            pin["repository"].as_str().unwrap_or(""),
                            pin["tag"].as_str().unwrap_or(""),
                            pin["repository_id"],
                            pin["release_id"],
                            pin["asset_id"],
                            pin["target"].as_str().unwrap_or("")
                        )
                    } else {
                        "Not recorded".into()
                    }),
                },
                Fact {
                    id: "software-installed-archive",
                    title: "Archive SHA-256",
                    value: Html::text(
                        installed["github"]["archive_sha256"]
                            .as_str()
                            .unwrap_or("Not recorded"),
                    ),
                },
                Fact {
                    id: "software-installed-program",
                    title: "Program SHA-256",
                    value: Html::text(
                        installed["program_sha256"]
                            .as_str()
                            .unwrap_or("Not recorded"),
                    ),
                },
                Fact {
                    id: "software-installed-setup",
                    title: "Setup commit",
                    value: Html::text(
                        installed["github"]["setup_commit"]
                            .as_str()
                            .unwrap_or("Not recorded"),
                    ),
                },
            ],
        })?);
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
                .slot(
                    "asset",
                    Html::text(&format!(
                        "{repository_id}/{}/{}",
                        release["release_id"], asset
                    )),
                )
                .slot(
                    "installed",
                    Html::text(if release["installed"] == true {
                        "Yes"
                    } else {
                        ""
                    }),
                )
                .slot("tag", Html::text(&tag_text))
                .slot(
                    "target",
                    Html::text(crate::github::describe_target(
                        release["target"].as_str().unwrap_or(""),
                    )),
                )
                .slot("file", Html::text(release["name"].as_str().unwrap_or("")))
                .slot("digest", Html::text(digest))
                .slot("published", Html::text(&published));
            row.dom_id = Some(format!("release-files-github-row-{asset}"));
            if can_review && release["installed"] != true {
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
}

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
    /// Handles release controls within the one mounted Software page.
    pub(super) async fn event(&mut self, request: &LiveRequest<'_, Scope>) -> Result<bool> {
        {
            let client = self.pages.db.get().await?;
            authorize_operator(&**client, request.scope, VIEW).await?;
        }
        if let LiveInput::Event(value) = request.input {
            let values = bilibi_ui::params::string_values(value);
            let get = |key: &str| values.get(key).map(String::as_str).unwrap_or("");
            match value["type"].as_str().unwrap_or("") {
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
