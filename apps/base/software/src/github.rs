//! GitHub as a source of setup releases. The server reads one repository the
//! owner named. It does not give GitHub a way into the server.
//!
//! The repository name, the remembered GitHub repository number, the optional
//! read token and the refresh interval are settings (`lib.rs`). The token is
//! encrypted. Listings are cached on disk so a live page does not call GitHub
//! on every refresh. Downloads go through `base/integration` and drop the
//! token when GitHub redirects to the file host (`redirect.rs`).
use crate::{review::Review, transport, Software, SoftwareError, SoftwareResult, STEP_UP};
use bilibi_authz::authorize_operator;
use bilibi_database::{MutationTransaction, Result};
use bilibi_integration::{Call, Failure, Origin, Outgoing, RedirectPolicy, Reply};
use bilibi_settings::{Settings, Target};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{fs, path::Path, sync::Mutex, time::Duration};
use tokio_postgres::GenericClient;

pub const REPOSITORY: &str = "software.github_repository";
pub const REPOSITORY_ID: &str = "software.github_repository_id";
pub const REPOSITORY_BOUND: &str = "software.github_repository_bound";
pub const TOKEN: &str = "software.github_token";
pub const REFRESH_MINUTES: &str = "software.github_refresh_minutes";
pub const DEFAULT_REFRESH_MINUTES: i64 = 15;
pub const STEP_UP_TOKEN: &str = "base.software.github_token.change";

const API: &str = "https://api.github.com";
const API_VERSION: &str = "2022-11-28";
const ACCEPT: &str = "application/vnd.github+json";
const USER_AGENT: &str = "bilibi";
const SYSTEM: &str = "github";
const LISTING_TIMEOUT: Duration = Duration::from_secs(30);
const LISTING_BYTES: u64 = 2 << 20;
/// Hosts that may receive a release-asset download after GitHub redirects.
/// The API token is removed when the host is no longer `api.github.com`.
const DOWNLOAD_HOSTS: &[&str] = &[
    "api.github.com",
    "release-assets.githubusercontent.com",
    "objects.githubusercontent.com",
    "github-releases.githubusercontent.com",
];

static MEMORY: Mutex<Option<Value>> = Mutex::new(None);

/// `owner/name`, the shape GitHub uses in a repository address.
pub fn parse_repository(raw: &str) -> std::result::Result<(String, String), String> {
    let raw = raw.trim();
    let Some((owner, name)) = raw.split_once('/') else {
        return Err(
            "Enter the repository as owner/name, like BelimbingApp/bilibi-setup-template.".into(),
        );
    };
    if raw.matches('/').count() != 1 || !github_owner(owner) || !github_name(name) {
        return Err("Enter the repository as owner/name, like BelimbingApp/bilibi-setup-template. Do not paste a file address.".into());
    }
    Ok((owner.to_owned(), name.to_owned()))
}
fn github_owner(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=39).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'-')
        && !value.starts_with('-')
        && !value.ends_with('-')
}
fn github_name(value: &str) -> bool {
    let bytes = value.as_bytes();
    (1..=100).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'-' | b'_' | b'.'))
        && !value.starts_with('.')
        && !value.ends_with('.')
}
pub fn accepts_repository(value: &Value) -> bool {
    value.is_null()
        || value
            .as_str()
            .is_some_and(|name| parse_repository(name).is_ok())
}
pub fn accepts_token(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(token_ok)
}
fn token_ok(token: &str) -> bool {
    (20..=255).contains(&token.chars().count()) && token.chars().all(|c| c.is_ascii_graphic())
}
pub fn accepts_id(value: &Value) -> bool {
    value.is_null() || value.as_i64().is_some_and(|id| id > 0)
}
pub fn accepts_bound(value: &Value) -> bool {
    value.is_null()
        || value
            .as_str()
            .is_some_and(|name| parse_repository(name).is_ok())
}

/// The Rust target this process was built for, when it is one the setup
/// archives are named with.
pub fn this_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-gnu"),
        _ => None,
    }
}
pub fn describe_target(target: &str) -> &'static str {
    match target {
        "x86_64-unknown-linux-gnu" => "Linux x86-64",
        "aarch64-unknown-linux-gnu" => "Linux ARM 64-bit",
        _ => "this kind of computer",
    }
}

struct Saved {
    repository: Option<String>,
    repository_id: Option<i64>,
    bound: Option<String>,
    refresh_minutes: i64,
    token_set: bool,
}
impl Software {
    async fn github_saved(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
    ) -> Result<Saved> {
        let repository = text_setting(self.settings.get_global(client, REPOSITORY).await?.as_str());
        let bound = text_setting(
            self.settings
                .get_global(client, REPOSITORY_BOUND)
                .await?
                .as_str(),
        );
        let repository_id = self
            .settings
            .get_global(client, REPOSITORY_ID)
            .await?
            .as_i64()
            .filter(|id| *id > 0);
        let refresh_minutes = self
            .settings
            .get_global(client, REFRESH_MINUTES)
            .await?
            .as_i64()
            .filter(|minutes| (1..=1440).contains(minutes))
            .unwrap_or(DEFAULT_REFRESH_MINUTES);
        let token_set = self
            .settings
            .resolve_masked(client, scope, Target::Global, &[TOKEN])
            .await?
            .get(TOKEN)
            .is_some_and(|resolved| resolved.source.is_some());
        Ok(Saved {
            repository,
            repository_id,
            bound,
            refresh_minutes,
            token_set,
        })
    }

    /// The cached GitHub release list, refreshed when it is older than the
    /// owner's interval or when `force` is set. `prerelease` includes files
    /// GitHub marks as a pre-release. Drafts are never listed.
    pub(crate) async fn github_listing(
        &self,
        scope: &Scope,
        force: bool,
        prerelease: bool,
    ) -> Result<Value> {
        let client = self.db.get().await?;
        let saved = self.github_saved(&**client, scope).await?;
        drop(client);
        let Some(repository) = saved.repository.clone() else {
            return Ok(json!({
                "available": false,
                "token_set": saved.token_set,
                "refresh_minutes": saved.refresh_minutes,
                "message": "No GitHub repository is set. A server owner sets Software › GitHub repository, like BelimbingApp/bilibi-setup-template.",
            }));
        };
        let Some(target) = this_target() else {
            return Ok(json!({
                "available": false,
                "repository": repository,
                "token_set": saved.token_set,
                "refresh_minutes": saved.refresh_minutes,
                "message": "This server is not a Linux x86-64 or ARM 64-bit computer, so it cannot choose a GitHub setup file.",
            }));
        };
        let cache_path = self
            .paths
            .as_ref()
            .map(|paths| paths.dir.join("data").join("github-listing.json"));
        if !force {
            if let Some(cached) = read_cache(cache_path.as_deref(), &repository) {
                if fresh(&cached, saved.refresh_minutes) || limited(&cached) {
                    return Ok(present(&cached, &saved, target, prerelease, false));
                }
            }
        } else if let Some(cached) = read_cache(cache_path.as_deref(), &repository) {
            if limited(&cached) {
                return Ok(present(&cached, &saved, target, prerelease, false));
            }
        }
        let token = self.github_token(scope).await?;
        let fetched = fetch_listing(
            &self.integration,
            scope,
            &repository,
            token.as_deref(),
            target,
            cache_path.as_deref(),
        )
        .await;
        let cached = match fetched {
            Ok(value) => value,
            Err(message) => {
                // Remember the failed attempt so every page render does not
                // ask a GitHub that just refused or did not answer. Refresh
                // still asks again.
                let mut kept = read_cache(cache_path.as_deref(), &repository).unwrap_or_else(
                    || json!({"available": false, "repository": repository, "releases": []}),
                );
                kept["attempt_at"] = json!(jiff::Timestamp::now().to_string());
                kept["message"] = json!(message);
                write_cache(cache_path.as_deref(), &kept);
                let mut shown = present(&kept, &saved, target, prerelease, true);
                shown["message"] = json!(message);
                return Ok(shown);
            }
        };
        Ok(present(&cached, &saved, target, prerelease, false))
    }

    pub(crate) async fn github_token(&self, scope: &Scope) -> Result<Option<String>> {
        let client = self.db.get().await?;
        let resolved = self
            .settings
            .get(&**client, scope, Target::Global, TOKEN)
            .await?;
        Ok(resolved.value.as_str().map(str::to_owned))
    }

    /// What saving the GitHub source would do. The token is never copied
    /// into the review.
    pub(crate) async fn review_github(
        &self,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, crate::MANAGE).await?;
        let saved = self.github_saved(&**client, scope).await?;
        drop(client);
        github_review(self, scope, input, &saved).await
    }

    pub(crate) async fn github_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, crate::MANAGE).await?;
        let saved = self.github_saved(tx.sql(), scope).await?;
        let review = github_review(self, scope, input, &saved).await?;
        self.step_up.require(scope, STEP_UP, &review.target).await?;
        if (input.get("token").and_then(Value::as_str).is_some()
            || input["clear_token"].as_bool() == Some(true))
            && !self.settings.encryption_available()
        {
            return Err(SoftwareError::field(
                "token",
                "This server has no settings key (BILIBI_SETTINGS_KEY), so it cannot keep a GitHub token. Set one in the instance's env file first.",
            ));
        }
        apply_github(tx, &self.settings, scope, input, &review).await?;
        Ok(
            json!({"review": review.facts, "github": {"repository": review.facts["repository"], "repository_id": review.facts["repository_id"], "token_set": review.facts["token_set"]}}),
        )
    }
}

fn text_setting(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

async fn github_review(
    software: &Software,
    scope: &Scope,
    input: &Value,
    saved: &Saved,
) -> SoftwareResult<Review> {
    let repository = match input.get("repository") {
        Some(Value::String(name)) => {
            parse_repository(name)
                .map_err(|message| SoftwareError::field("repository", &message))?;
            Some(name.trim().to_owned())
        }
        Some(Value::Null) | None => saved.repository.clone(),
        Some(_) => {
            return Err(SoftwareError::field(
                "repository",
                "Enter the repository as owner/name, like BelimbingApp/bilibi-setup-template.",
            ))
        }
    };
    if let Some(token) = input.get("token").and_then(Value::as_str) {
        if !token_ok(token) {
            return Err(SoftwareError::field(
                "token",
                "The GitHub token must be 20 to 255 characters with no spaces. Create a fine-grained token that can only read this repository.",
            ));
        }
    }
    let refresh_minutes = match input.get("refresh_minutes") {
        Some(value) => value
            .as_i64()
            .filter(|minutes| (1..=1440).contains(minutes))
            .ok_or_else(|| {
                SoftwareError::field(
                    "refresh_minutes",
                    "Enter how many minutes to wait between GitHub checks, from 1 to 1440. 15 checks about four times an hour.",
                )
            })?,
        None => saved.refresh_minutes,
    };
    let clear_token = input["clear_token"].as_bool() == Some(true);
    let accept = input["accept"].as_bool() == Some(true);
    let name_changes = repository.as_deref() != saved.repository.as_deref()
        && input.get("repository").and_then(Value::as_str).is_some();
    let mut looked_up: Option<(i64, bool)> = None;
    if accept || name_changes {
        let Some(repository) = repository.clone() else {
            return Err(SoftwareError::field(
                "repository",
                "Enter the repository as owner/name before accepting it.",
            ));
        };
        let token = if clear_token {
            None
        } else if let Some(token) = input.get("token").and_then(Value::as_str) {
            Some(token.to_owned())
        } else {
            software.github_token(scope).await?
        };
        let repo = fetch_repo(&software.integration, scope, &repository, token.as_deref())
            .await
            .map_err(|message| SoftwareError::field("repository", &message))?;
        if let Some(expected) = input.get("repository_id").and_then(Value::as_i64) {
            if expected != repo.0 {
                return Err(SoftwareError::field(
                    "repository_id",
                    &format!("GitHub now says {repository} is repository {0}, not {expected}. Look at the list again before saving.", repo.0),
                ));
            }
        }
        looked_up = Some(repo);
    }
    let repository_id = looked_up.map(|(id, _)| id).or(saved.repository_id);
    let token_set = if clear_token {
        false
    } else if input.get("token").and_then(Value::as_str).is_some() {
        true
    } else {
        saved.token_set
    };
    let repository_text = repository.clone().unwrap_or_else(|| "no repository".into());
    let summary = match &looked_up {
        Some((id, _)) => format!("Remember GitHub repository {repository_text} as number {id}"),
        None if clear_token => format!("Clear the GitHub read token for {repository_text}"),
        None if input.get("token").is_some() => {
            format!("Save a GitHub read token for {repository_text}")
        }
        None => format!("Check GitHub for {repository_text} every {refresh_minutes} minutes"),
    };
    let mut notes = Vec::new();
    if let Some((id, private)) = looked_up {
        notes.push(format!(
            "GitHub repository {repository_text} is number {id}{}.",
            if private { ", and it is private" } else { "" }
        ));
        if saved.repository_id.is_some_and(|old| old != id) {
            notes.push(format!(
                "That replaces remembered repository number {}.",
                saved.repository_id.unwrap_or(0)
            ));
        }
    }
    if clear_token {
        notes.push("The saved read token will be removed.".into());
    } else if input.get("token").and_then(Value::as_str).is_some() {
        notes.push("A new read token will be saved. It is not shown here.".into());
    }
    notes.push(format!(
        "The release list is checked at most every {refresh_minutes} minutes, and when you choose Refresh."
    ));
    let request = json!({
        "repository": repository,
        "repository_id": repository_id,
        "refresh_minutes": refresh_minutes,
        "clear_token": clear_token,
        "accept": accept || name_changes,
        "token_set": token_set,
    });
    Ok(Review {
        target: summary.clone(),
        request: request.clone(),
        facts: json!({
            "summary": summary,
            "repository": repository,
            "repository_id": repository_id,
            "refresh_minutes": refresh_minutes,
            "clear_token": clear_token,
            "token_set": token_set,
            "accept": accept || name_changes,
            "notes": notes,
            "step_up": [STEP_UP],
        }),
        switch: "auto".into(),
    })
}

async fn apply_github(
    tx: &mut MutationTransaction<'_>,
    settings: &Settings,
    scope: &Scope,
    input: &Value,
    review: &Review,
) -> SoftwareResult<()> {
    if let Some(repository) = review.facts["repository"].as_str() {
        if input.get("repository").and_then(Value::as_str).is_some() {
            settings
                .put(tx, scope, Target::Global, REPOSITORY, json!(repository))
                .await?;
        }
    }
    if review.facts["accept"].as_bool() == Some(true) {
        let id = review.facts["repository_id"].as_i64().ok_or_else(|| {
            SoftwareError::field("repository", "GitHub did not return a repository number.")
        })?;
        let repository = review.facts["repository"].as_str().ok_or_else(|| {
            SoftwareError::field("repository", "Enter the repository as owner/name.")
        })?;
        settings
            .put(tx, scope, Target::Global, REPOSITORY_ID, json!(id))
            .await?;
        settings
            .put(
                tx,
                scope,
                Target::Global,
                REPOSITORY_BOUND,
                json!(repository),
            )
            .await?;
        settings
            .put(tx, scope, Target::Global, REPOSITORY, json!(repository))
            .await?;
    }
    if let Some(minutes) = input.get("refresh_minutes").and_then(Value::as_i64) {
        settings
            .put(tx, scope, Target::Global, REFRESH_MINUTES, json!(minutes))
            .await?;
    }
    if review.facts["clear_token"].as_bool() == Some(true) {
        settings.delete(tx, scope, Target::Global, TOKEN).await?;
    } else if let Some(token) = input.get("token").and_then(Value::as_str) {
        settings
            .put(tx, scope, Target::Global, TOKEN, json!(token))
            .await?;
    }
    Ok(())
}

/// The identity an install review binds, and the download checks again.
#[derive(Clone, Debug)]
pub(crate) struct Pin {
    pub repository: String,
    pub repository_id: i64,
    pub release_id: i64,
    pub asset_id: i64,
    pub tag: String,
    pub target: String,
    pub digest: String,
    pub name: String,
    pub bytes: u64,
    pub prerelease: bool,
    pub setup_commit: Option<String>,
}
impl Pin {
    pub(crate) fn from_request(source: &Value) -> std::result::Result<Self, String> {
        let repository = source["repository"]
            .as_str()
            .ok_or("the request names no GitHub repository")?
            .to_owned();
        parse_repository(&repository).map_err(|_| {
            "the request names a GitHub repository this server cannot call".to_owned()
        })?;
        let digest = normalize_digest(source["digest"].as_str().unwrap_or(""))
            .ok_or("the request has no check code for the GitHub file")?;
        Ok(Self {
            repository,
            repository_id: source["repository_id"]
                .as_i64()
                .ok_or("the request names no GitHub repository number")?,
            release_id: source["release_id"]
                .as_i64()
                .ok_or("the request names no GitHub release")?,
            asset_id: source["asset_id"]
                .as_i64()
                .ok_or("the request names no GitHub file")?,
            tag: source["tag"]
                .as_str()
                .ok_or("the request names no setup tag")?
                .to_owned(),
            target: source["target"]
                .as_str()
                .ok_or("the request names no target")?
                .to_owned(),
            digest,
            name: source["name"]
                .as_str()
                .unwrap_or("the setup file")
                .to_owned(),
            bytes: source["bytes"].as_u64().unwrap_or(0),
            prerelease: source["prerelease"].as_bool().unwrap_or(false),
            setup_commit: source["setup_commit"].as_str().map(str::to_owned),
        })
    }
}

/// Read the setup tag's commit only for a Staging comparison. A missing
/// answer leaves the comparison unknown and never blocks installation.
pub(crate) async fn setup_commit(software: &Software, scope: &Scope, pin: &Pin) -> Option<String> {
    let token = software.github_token(scope).await.ok()?;
    let mut url = url::Url::parse(&format!("{API}/repos/{}/commits", pin.repository)).ok()?;
    url.path_segments_mut()
        .ok()?
        .push(&format!("refs/tags/{}", pin.tag));
    let commit = get_json(
        &software.integration,
        scope,
        url.as_str(),
        token.as_deref(),
        "commits.get",
    )
    .await
    .ok()?;
    commit["sha"]
        .as_str()
        .filter(|sha| sha.len() == 40 && sha.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(|sha| sha.to_ascii_lowercase())
}

/// Read the release GitHub has now and refuse when it is not the reviewed file.
pub(crate) async fn recheck(
    software: &Software,
    scope: &Scope,
    pin: &Pin,
) -> std::result::Result<(), String> {
    let token = software
        .github_token(scope)
        .await
        .map_err(|_| "The GitHub token could not be read. Ask again.".to_owned())?;
    let repo = fetch_repo(
        &software.integration,
        scope,
        &pin.repository,
        token.as_deref(),
    )
    .await
    .map_err(|message| format!("Could not check the GitHub repository: {message}"))?;
    if repo.0 != pin.repository_id {
        return Err(format!(
            "GitHub repository {} is number {} now, not {}. Nothing was downloaded. Accept the repository again, then review the release.",
            pin.repository, repo.0, pin.repository_id
        ));
    }
    let release = get_json(
        &software.integration,
        scope,
        &format!("{API}/repos/{}/releases/{}", pin.repository, pin.release_id),
        token.as_deref(),
        "releases.get",
    )
    .await
    .map_err(|message| format!("Could not check the GitHub release: {message}"))?;
    if release["id"].as_i64() != Some(pin.release_id) {
        return Err("GitHub returned a different release from the one that was reviewed. Nothing was downloaded. Review the release again.".into());
    }
    if release["tag_name"].as_str() != Some(pin.tag.as_str()) {
        return Err(format!(
            "GitHub release {} is tagged {}, not {}. Nothing was downloaded. Review the release again.",
            pin.release_id,
            release["tag_name"].as_str().unwrap_or("without a tag"),
            pin.tag
        ));
    }
    if release["draft"].as_bool() == Some(true) {
        return Err("That GitHub release is still a draft. Publish it, or choose a release that is not a draft.".into());
    }
    let asset = release["assets"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|asset| asset["id"].as_i64() == Some(pin.asset_id))
        .cloned()
        .ok_or_else(|| format!("GitHub release {} no longer has the file that was reviewed. Nothing was downloaded. Review the release again.", pin.tag))?;
    let digest = normalize_digest(asset["digest"].as_str().unwrap_or("")).ok_or_else(|| {
        format!(
            "GitHub has no check code for {} now. Nothing was downloaded.",
            pin.name
        )
    })?;
    if digest != pin.digest {
        return Err(format!(
            "The GitHub file {} changed after it was reviewed (its check code no longer matches). Nothing was downloaded. Review the release again.",
            pin.name
        ));
    }
    Ok(())
}

pub(crate) fn asset_request(repository: &str, asset_id: i64, token: Option<&str>) -> Outgoing {
    let mut outgoing = Outgoing::get(format!(
        "{API}/repos/{repository}/releases/assets/{asset_id}"
    ))
    .header("accept", "application/octet-stream")
    .header("user-agent", USER_AGENT)
    .header("x-github-api-version", API_VERSION)
    .redirects(RedirectPolicy {
        max_hops: 3,
        allowed_hosts: DOWNLOAD_HOSTS,
    });
    if let Some(token) = token {
        outgoing = outgoing.header("authorization", format!("Bearer {token}"));
    }
    outgoing
}

fn present(cache: &Value, saved: &Saved, target: &str, prerelease: bool, stale: bool) -> Value {
    let repository = cache["repository"].as_str().unwrap_or("");
    let live_id = cache["repository_id"].as_i64();
    let accepted = saved.repository.as_deref() == Some(repository)
        && saved.bound.as_deref() == Some(repository)
        && saved.repository_id.is_some()
        && (live_id.is_none() || saved.repository_id == live_id);
    let identity_changed = saved.repository_id.is_some()
        && live_id.is_some()
        && saved.repository.as_deref() == Some(repository)
        && saved.repository_id != live_id;
    let mut releases: Vec<Value> = cache["releases"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|row| prerelease || row["prerelease"].as_bool() != Some(true))
        .collect();
    releases.retain(|row| row["target"].as_str() == Some(target));
    let mut message = cache["message"].as_str().unwrap_or("").to_owned();
    if identity_changed {
        message = format!(
            "GitHub now says {repository} is repository {}, not the remembered number {}. Accept the new repository before installing.",
            live_id.unwrap_or(0),
            saved.repository_id.unwrap_or(0)
        );
    } else if !accepted && live_id.is_some() {
        message = format!(
            "GitHub repository {repository} is number {}. Accept it before installing a release.",
            live_id.unwrap_or(0)
        );
    } else if releases.is_empty() && message.is_empty() {
        message = if prerelease {
            format!(
                "No setup file for {} is published in {repository}.",
                describe_target(target)
            )
        } else {
            format!(
                "No stable setup file for {} is published in {repository}. Choose Show pre-releases if the file is marked as a pre-release.",
                describe_target(target)
            )
        };
    }
    json!({
        "available": cache["available"].as_bool().unwrap_or(false),
        "repository": repository,
        "repository_id": live_id,
        "remembered_id": saved.repository_id,
        "accepted": accepted,
        "identity_changed": identity_changed,
        "target": target,
        "prerelease": prerelease,
        "fetched_at": cache["fetched_at"],
        "stale": stale || cache["stale"].as_bool().unwrap_or(false),
        "token_set": saved.token_set,
        "refresh_minutes": saved.refresh_minutes,
        "limited_until": cache["limited_until"],
        "message": if message.is_empty() { Value::Null } else { json!(message) },
        "releases": releases,
    })
}

/// Whether the last look at GitHub, answered or not, is newer than `minutes`.
fn fresh(cache: &Value, minutes: i64) -> bool {
    let newest = ["fetched_at", "attempt_at"]
        .iter()
        .filter_map(|key| cache[*key].as_str())
        .filter_map(|text| text.parse::<jiff::Timestamp>().ok())
        .map(|at| at.as_second())
        .max();
    newest.is_some_and(|at| jiff::Timestamp::now().as_second() - at < minutes.saturating_mul(60))
}
fn limited(cache: &Value) -> bool {
    let Some(until) = cache["limited_until"].as_str() else {
        return false;
    };
    let Ok(until) = until.parse::<jiff::Timestamp>() else {
        return false;
    };
    jiff::Timestamp::now() < until
}

/// The saved list: the file in the instance's `data/` folder (the one place
/// the program may write besides `incoming/`), else the copy in memory.
fn read_cache(path: Option<&Path>, repository: &str) -> Option<Value> {
    let from_file = path.and_then(|path| {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    });
    let value = from_file.or_else(|| MEMORY.lock().ok().and_then(|guard| guard.clone()))?;
    (value["repository"].as_str() == Some(repository)).then_some(value)
}
fn write_cache(path: Option<&Path>, value: &Value) {
    if let Ok(mut guard) = MEMORY.lock() {
        *guard = Some(value.clone());
    }
    let Some(path) = path else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    if let Ok(bytes) = serde_json::to_vec(value) {
        let _ = fs::write(path, bytes);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
        }
    }
}

async fn fetch_listing(
    integration: &bilibi_integration::Integration,
    scope: &Scope,
    repository: &str,
    token: Option<&str>,
    target: &str,
    cache_path: Option<&Path>,
) -> std::result::Result<Value, String> {
    let (id, private) = fetch_repo(integration, scope, repository, token).await?;
    let previous = read_cache(cache_path, repository);
    let etag = previous
        .as_ref()
        .and_then(|cache| cache["etag"].as_str())
        .map(str::to_owned);
    let url = format!("{API}/repos/{repository}/releases?per_page=30");
    let first = get_raw(
        integration,
        scope,
        &url,
        token,
        etag.as_deref(),
        "releases.list",
    )
    .await?;
    if first.status == 304 {
        let mut cache = previous.unwrap_or_else(|| json!({}));
        cache["fetched_at"] = json!(jiff::Timestamp::now().to_string());
        cache["repository_id"] = json!(id);
        if let Some(fields) = cache.as_object_mut() {
            for stale in ["message", "attempt_at", "limited_until"] {
                fields.remove(stale);
            }
        }
        apply_rate(&mut cache, &first);
        write_cache(cache_path, &cache);
        return Ok(cache);
    }
    if !(200..300).contains(&first.status) {
        return Err(github_status(
            repository,
            first.status,
            token.is_some(),
            &first,
        ));
    }
    let mut body = json_body(&first.body)?;
    let mut rated = &first;
    let mut page_keep = None;
    if let Some(next) = next_link(&header_text(&first, "link").unwrap_or_default()) {
        if next.starts_with(API) && rate_headers_remaining(&first) > 10 {
            if let Ok(page) = get_raw(integration, scope, &next, token, None, "releases.list").await
            {
                if (200..300).contains(&page.status) {
                    if let Ok(mut more) = json_body(&page.body) {
                        if let (Some(left), Some(right)) =
                            (body.as_array_mut(), more.as_array_mut())
                        {
                            left.append(right);
                        }
                    }
                    page_keep = Some(page);
                }
            }
        }
    }
    if let Some(page) = page_keep.as_ref() {
        rated = page;
    }
    let releases = body
        .as_array()
        .into_iter()
        .flatten()
        .filter(|release| release["draft"].as_bool() != Some(true))
        .filter_map(|release| release_rows(release, target))
        .flatten()
        .collect::<Vec<_>>();
    let mut cache = json!({
        "available": true,
        "repository": repository,
        "repository_id": id,
        "private": private,
        "etag": header_text(&first, "etag"),
        "fetched_at": jiff::Timestamp::now().to_string(),
        "releases": releases,
    });
    apply_rate(&mut cache, rated);
    write_cache(cache_path, &cache);
    Ok(cache)
}

fn release_rows(release: &Value, target: &str) -> Option<Vec<Value>> {
    let tag = release["tag_name"].as_str()?.to_owned();
    let release_id = release["id"].as_i64()?;
    let prerelease = release["prerelease"].as_bool().unwrap_or(false);
    let published_at = release["published_at"].as_str().unwrap_or("").to_owned();
    let rows = release["assets"]
        .as_array()?
        .iter()
        .filter_map(|asset| asset_row(asset, &tag, release_id, prerelease, &published_at, target))
        .collect::<Vec<_>>();
    Some(rows)
}
fn asset_row(
    asset: &Value,
    tag: &str,
    release_id: i64,
    prerelease: bool,
    published_at: &str,
    target: &str,
) -> Option<Value> {
    if asset["state"]
        .as_str()
        .is_some_and(|state| state != "uploaded")
    {
        return None;
    }
    let name = asset["name"].as_str()?.to_owned();
    let suffix = format!("-{target}.tar.gz");
    if !name.starts_with("bilibi-setup-") || !name.ends_with(&suffix) {
        return None;
    }
    let digest = normalize_digest(asset["digest"].as_str().unwrap_or(""))?;
    Some(json!({
        "tag": tag,
        "release_id": release_id,
        "asset_id": asset["id"].as_i64()?,
        "name": name,
        "target": target,
        "digest": digest,
        "bytes": asset["size"].as_u64().unwrap_or(0),
        "prerelease": prerelease,
        "published_at": published_at,
    }))
}

async fn fetch_repo(
    integration: &bilibi_integration::Integration,
    scope: &Scope,
    repository: &str,
    token: Option<&str>,
) -> std::result::Result<(i64, bool), String> {
    let reply = get_raw(
        integration,
        scope,
        &format!("{API}/repos/{repository}"),
        token,
        None,
        "repos.get",
    )
    .await?;
    if reply.status != 200 {
        return Err(github_status(
            repository,
            reply.status,
            token.is_some(),
            &reply,
        ));
    }
    let body = json_body(&reply.body)?;
    let id = body["id"].as_i64().ok_or_else(|| {
        format!("GitHub answered for {repository}, but not with a repository number.")
    })?;
    Ok((id, body["private"].as_bool().unwrap_or(false)))
}

async fn get_json(
    integration: &bilibi_integration::Integration,
    scope: &Scope,
    url: &str,
    token: Option<&str>,
    operation: &str,
) -> std::result::Result<Value, String> {
    let reply = get_raw(integration, scope, url, token, None, operation).await?;
    if reply.status != 200 {
        return Err(github_status(
            "GitHub",
            reply.status,
            token.is_some(),
            &reply,
        ));
    }
    json_body(&reply.body)
}

async fn get_raw(
    integration: &bilibi_integration::Integration,
    scope: &Scope,
    url: &str,
    token: Option<&str>,
    etag: Option<&str>,
    operation: &str,
) -> std::result::Result<Reply, String> {
    let mut outgoing = Outgoing::get(url)
        .header("accept", ACCEPT)
        .header("user-agent", USER_AGENT)
        .header("x-github-api-version", API_VERSION)
        .timeout(LISTING_TIMEOUT)
        .max_body_bytes(LISTING_BYTES)
        .no_redirects();
    if let Some(token) = token {
        outgoing = outgoing.header("authorization", format!("Bearer {token}"));
    }
    if let Some(etag) = etag {
        outgoing = outgoing.header("if-none-match", etag);
    }
    integration
        .send(
            Origin::User(scope),
            &Call {
                system: SYSTEM.into(),
                operation: operation.into(),
                ..Call::default()
            },
            outgoing,
        )
        .await
        .map_err(failure_sentence)
}

fn json_body(bytes: &[u8]) -> std::result::Result<Value, String> {
    serde_json::from_slice(bytes)
        .map_err(|_| "GitHub answered, but not with a release list.".into())
}
fn header_text(reply: &Reply, name: &str) -> Option<String> {
    reply
        .headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}
fn rate_headers_remaining(reply: &Reply) -> i64 {
    header_text(reply, "x-ratelimit-remaining")
        .and_then(|value| value.parse().ok())
        .unwrap_or(1)
}
fn apply_rate(cache: &mut Value, reply: &Reply) {
    let remaining = rate_headers_remaining(reply);
    let reset = header_text(reply, "x-ratelimit-reset").and_then(|value| value.parse::<i64>().ok());
    let retry = header_text(reply, "retry-after").and_then(|value| value.parse::<i64>().ok());
    if remaining == 0 || retry.is_some() {
        let until = retry
            .map(|seconds| jiff::Timestamp::now() + jiff::SignedDuration::from_secs(seconds))
            .or_else(|| reset.and_then(|seconds| jiff::Timestamp::from_second(seconds).ok()));
        if let Some(until) = until {
            cache["limited_until"] = json!(until.to_string());
            cache["message"] = json!(format!(
                "GitHub is limiting requests until {until}. The list saved earlier is shown."
            ));
        }
    }
}
fn next_link(header: &str) -> Option<String> {
    header.split(',').find_map(|part| {
        let (url, rel) = part.split_once(';')?;
        rel.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_owned()
        })
    })
}
fn json_i64(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
        .filter(|number| *number > 0)
}
fn normalize_digest(value: &str) -> Option<String> {
    let hex = value
        .strip_prefix("sha256:")
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    (hex.len() == 64 && hex.chars().all(|c| c.is_ascii_hexdigit())).then_some(hex)
}
/// The sentence for a download GitHub answered with something other than
/// the file. A rate limit looks like a refusal, so it is named as one.
/// `limits` holds the `retry-after`, `x-ratelimit-remaining` and
/// `x-ratelimit-reset` header values, when GitHub sent them.
pub(crate) fn download_refusal(status: u16, limits: [Option<&str>; 3], token: bool) -> String {
    let [retry_after, remaining, reset] = limits;
    if (status == 403 || status == 429) && (retry_after.is_some() || remaining == Some("0")) {
        let until = reset
            .and_then(|value| value.parse::<i64>().ok())
            .and_then(|seconds| jiff::Timestamp::from_second(seconds).ok());
        return match until {
            Some(until) => format!(
                "GitHub is limiting requests from this server until {until}. Nothing was saved. Ask again after that time{}.",
                if token { "" } else { ", or save a read token to raise the limit" }
            ),
            None => "GitHub is limiting requests from this server. Nothing was saved. Ask again in a few minutes.".into(),
        };
    }
    match status {
        401 => "GitHub refused the read token. Save a new read-only token, or clear it when the repository is public. Nothing was saved.".into(),
        403 | 404 => "GitHub would not send the file. Check that the read token can read this repository. Nothing was saved.".into(),
        status => format!("GitHub answered with status {status} instead of the file. Nothing was saved."),
    }
}
fn github_status(repository: &str, status: u16, token: bool, reply: &Reply) -> String {
    if status == 304 {
        return format!("{repository} has not changed.");
    }
    if status == 401 {
        return "GitHub refused the read token. Save a new read-only token, or clear it when the repository is public.".into();
    }
    if status == 403 || status == 429 {
        if header_text(reply, "retry-after").is_some() || rate_headers_remaining(reply) == 0 {
            return "GitHub is limiting requests from this server. The last saved list is shown, when there is one. Try Refresh later.".into();
        }
        return format!("GitHub refused the request for {repository}. Check the repository name and the read token.");
    }
    if status == 404 {
        return if token {
            format!("GitHub has no repository {repository} that this token can read. Check the name, or use a token that can read that repository.")
        } else {
            format!("GitHub has no public repository {repository}. If it is private, save a read-only token.")
        };
    }
    if (300..400).contains(&status) {
        return "GitHub sent the request to another address. Nothing was read.".into();
    }
    format!("GitHub answered with status {status} for {repository}. Try again in a few minutes.")
}
fn failure_sentence(failure: Failure) -> String {
    match failure {
        Failure::Transport { class, .. } => match class {
            "timeout" => {
                "GitHub did not answer in time. The last saved list is shown, when there is one."
                    .into()
            }
            "connect" => {
                "GitHub could not be reached. The last saved list is shown, when there is one."
                    .into()
            }
            "redirect" => {
                "GitHub sent the request to an address this server does not call. Nothing was read."
                    .into()
            }
            "body_limit" => {
                "GitHub sent more than this server will read for a release list.".into()
            }
            other => format!("GitHub could not be read ({other}). Try again in a few minutes."),
        },
        Failure::Invalid => "The GitHub address could not be called.".into(),
        Failure::Unrecorded => {
            "GitHub may have answered, but the exchange could not be recorded. Try again.".into()
        }
    }
}

/// Find the reviewed asset in a listing the page or `base.software.releases` returned.
pub(crate) fn pin_from_listing(listing: &Value, input: &Value) -> SoftwareResult<Pin> {
    let asset_id = json_i64(&input["asset"]).ok_or_else(|| {
        SoftwareError::field(
            "asset",
            "Give the GitHub file number from base.software.releases.",
        )
    })?;
    if listing["accepted"].as_bool() != Some(true) {
        let message = listing["message"].as_str().unwrap_or(
            "Accept the GitHub repository on Software › Releases before installing from it.",
        );
        return Err(SoftwareError::field("asset", message));
    }
    if listing["identity_changed"].as_bool() == Some(true) {
        return Err(SoftwareError::field(
            "asset",
            listing["message"]
                .as_str()
                .unwrap_or("Accept the changed GitHub repository before installing."),
        ));
    }
    let row = listing["releases"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|row| row["asset_id"].as_i64() == Some(asset_id))
        .cloned()
        .ok_or_else(|| {
            SoftwareError::field(
                "asset",
                "That file is not in the current GitHub list. Refresh the list and choose a file for this server.",
            )
        })?;
    let digest = normalize_digest(input["digest"].as_str().unwrap_or("")).ok_or_else(|| {
        SoftwareError::field(
            "digest",
            "Give the check code from base.software.releases, 64 hex characters.",
        )
    })?;
    let listed = row["digest"].as_str().unwrap_or("");
    if digest != listed {
        return Err(SoftwareError::field(
            "digest",
            "The GitHub file's check code changed after you chose it. Refresh the list and review the release again.",
        ));
    }
    let repository_id = json_i64(&input["repository_id"]).ok_or_else(|| {
        SoftwareError::field(
            "repository_id",
            "Give the GitHub repository number from base.software.releases.",
        )
    })?;
    if listing["repository_id"].as_i64() != Some(repository_id) {
        return Err(SoftwareError::field(
            "repository_id",
            "That repository number is not the one GitHub reports now. Refresh the list and review the release again.",
        ));
    }
    let release_id = json_i64(&input["release_id"]).ok_or_else(|| {
        SoftwareError::field(
            "release_id",
            "Give the GitHub release number from base.software.releases.",
        )
    })?;
    if row["release_id"].as_i64() != Some(release_id) {
        return Err(SoftwareError::field(
            "release_id",
            "That release number does not match the file. Refresh the list and review the release again.",
        ));
    }
    let tag = input["tag"].as_str().unwrap_or("");
    if tag.is_empty() || row["tag"].as_str() != Some(tag) {
        return Err(SoftwareError::field(
            "tag",
            "That setup tag does not match the file. Refresh the list and review the release again.",
        ));
    }
    let target = input["target"].as_str().unwrap_or("");
    if row["target"].as_str() != Some(target) {
        return Err(SoftwareError::field(
            "target",
            &format!(
                "That file is not for {}. Choose the file for this server.",
                this_target().map(describe_target).unwrap_or("this server")
            ),
        ));
    }
    Ok(Pin {
        repository: listing["repository"].as_str().unwrap_or("").to_owned(),
        repository_id,
        release_id,
        asset_id,
        tag: tag.to_owned(),
        target: target.to_owned(),
        digest,
        name: row["name"].as_str().unwrap_or("setup").to_owned(),
        bytes: row["bytes"].as_u64().unwrap_or(0),
        prerelease: row["prerelease"].as_bool().unwrap_or(false),
        setup_commit: row["setup_commit"].as_str().map(str::to_owned),
    })
}

/// The archive check code, lowercase hex without a `sha256:` prefix.
pub(crate) fn digest_of(path: &std::path::Path) -> std::io::Result<String> {
    transport::sha256_of(path)
}

/// Compare exact published assets across instances. Local release numbers
/// and tags alone cannot establish equal bytes.
pub(crate) fn same_asset(a: &Value, b: &Value) -> bool {
    let digest = |value: &Value| {
        value["digest"]
            .as_str()
            .or_else(|| value["archive_sha256"].as_str())
            .and_then(normalize_digest)
    };
    ["repository_id", "release_id", "asset_id"]
        .iter()
        .all(|key| a[key].as_i64().is_some_and(|id| id > 0) && a[key] == b[key])
        && ["tag", "target"]
            .iter()
            .all(|key| a[key].as_str().is_some_and(|text| !text.is_empty()) && a[key] == b[key])
        && digest(a).is_some()
        && digest(a) == digest(b)
}
