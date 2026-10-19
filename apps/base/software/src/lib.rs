//! Software discovery, installed versions, and safe install and undo jobs.
//! Operators build in their local Bilibi copy, then install a tested release
//! through the separate installer. No Bilimbi counterpart.
//!
//! Facts come from one place each: the running program for the Domains and
//! versions (`bilibi_module_registry::Composition`, handed in by the host as
//! `bootstrap.composition`), `bilibi_system::Instance` for which instance
//! this is, the published catalogue for what can be chosen, and the job
//! tables here for history and plug-ins.
mod agent;
mod catalogue;
mod discovery;
mod follow;
mod jobs;
mod pairing;
mod release_files;
mod review;
mod transfer;
mod transport;
mod web;

use bilibi_authz::{
    authorize_operator, CapabilityDefinition, SystemPrincipalDefinition, VocabularyContribution,
    VOCABULARY,
};
use bilibi_database::{Context, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_integration::Integration;
use bilibi_module_registry::{
    contributions::{
        Access, Download, Method, PageWrites, Pending, Route, DOWNLOADS, PAGE_WRITES, ROUTES,
    },
    Composition, Contribution, Provenance,
};
use bilibi_notifications::{Notice, NotificationBoundary};
use bilibi_settings::{
    Definition, Editor, ScopeKind, Setting, Settings, Validator, ValueType, DEFINITIONS,
};
use bilibi_step_up::StepUp;
use bilibi_system::Instance;
use bilibi_tenancy::{Authentication, Scope};
use serde_json::{json, Value};
use std::sync::{Arc, OnceLock};

pub use catalogue::{Catalogue, Version, DEFAULT_URL as DEFAULT_CATALOGUE_URL};
pub use jobs::{CHANNEL, RECORD};
pub use review::Review;
pub use transfer::TransportRefusal;
pub use transport::{
    bundle_filename, Paths as InstancePaths, BUNDLE_TYPE, LISTING_PATH, PAIRING_HEADER,
};

/// The MCP App card (`card/software-v1.html`): the Software page as one
/// self-contained document a chat host shows inline. `open_software` opens it.
pub const CARD_URI: &str = "ui://bilibi/software-v1.html";

/// A refusal that names the field and says what to do, or a backend error.
#[derive(Debug)]
pub enum SoftwareError {
    Field { field: String, message: String },
    Backend(Error),
}
impl SoftwareError {
    pub fn field(field: &str, message: &str) -> Self {
        Self::Field {
            field: field.into(),
            message: message.into(),
        }
    }
}
impl<E: Into<Error>> From<E> for SoftwareError {
    fn from(error: E) -> Self {
        Self::Backend(error.into())
    }
}
pub type SoftwareResult<T> = std::result::Result<T, SoftwareError>;

pub const SERVICE: &str = "base/software.software";
/// Seeing the installed software, the catalogue and the jobs (platform).
pub const VIEW: &str = "admin.system.software.view";
/// Changing the server's software (platform): a server owner's power, since
/// one install changes every company on the server.
pub const MANAGE: &str = "admin.system.software.manage";
/// The step-up operation every change asks for: a fresh code every time, on
/// every instance, Dev included, because an install changes the server itself.
pub const STEP_UP: &str = "base.software.change";
/// Where the published catalogue is read from.
pub const CATALOGUE_URL_KEY: &str = "software.catalogue_url";
pub const ACTION_INSTALL: &str = "software.install";
pub const ACTION_UNDO: &str = "software.undo";
pub const ACTION_CANCEL: &str = "software.cancel";
pub const ACTION_PAIR: &str = "software.pair";
/// The action a worker's outcome is settled under (`Software::settle`).
pub const ACTION_FINISHED: &str = "software.finished";
/// The action a notification of a settled outcome is sent under.
const ACTION_NOTIFIED: &str = "software.notified";
/// The most of a worker's message a notification carries; a tool's error can
/// be far longer than a notice may be.
const NOTICE_BODY_CHARS: usize = 900;
/// How often the server records the outcomes workers wrote.
const SETTLE_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

bilibi_step_up::declare!(
    "base/software",
    bilibi_step_up::Operation::new(
        STEP_UP,
        "Change the server's software (install, undo, pairing key)"
    )
    .always()
);
/// Saving the pairing key in the settings editor: whoever holds the key can
/// download the whole program, so it asks for a fresh code like `pair` does.
pub const PAIRING_KEY_STEP_UP: &str = "base.software.pairing_key.change";
bilibi_step_up::declare!(
    "base/software",
    bilibi_step_up::Operation::new(
        PAIRING_KEY_STEP_UP,
        "Change the pairing key in the settings"
    )
    .always()
    .guards_setting(pairing::KEY)
);

/// The facade: reviews a request without writing, and records it inside the
/// caller's audited transaction (an agent plan's commit, or a page through
/// `PageContracts`).
pub struct Software {
    db: Db,
    mutations: Arc<MutationBoundary>,
    settings: Arc<Settings>,
    step_up: Arc<StepUp>,
    notifications: Arc<NotificationBoundary>,
    authentication: Arc<Authentication>,
    composition: Arc<Composition>,
    catalogue: catalogue::Reader,
    integration: Arc<Integration>,
    /// The instance folder (`BILIBI_INSTANCE_DIR`), when this program runs
    /// in one: where releases are kept and received (`transport.rs`).
    paths: Option<transport::Paths>,
    /// `APP_ORIGIN`: what a release file made here says it came from.
    origin: String,
}
static BOUND: OnceLock<Arc<Software>> = OnceLock::new();
/// The installed facade, for the agent contracts of this module.
fn bound() -> Result<&'static Arc<Software>> {
    BOUND.get().ok_or(Error::Internal)
}

impl Software {
    /// What this instance announces itself as, what is
    /// installed, the plug-ins, the job waiting or running (or the last
    /// one), whether Undo is available, and the recent jobs.
    pub async fn status(&self, scope: &Scope) -> Result<Value> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let tenant = scope.tenant();
        let instance = Instance::resolve(&self.settings, &**client).await?;
        let active = jobs::active(&**client, tenant).await?;
        let recent = jobs::recent(&**client, tenant, 10).await?;
        let shown = match &active {
            Some(job) => Some(job.clone()),
            None => recent.first().cloned(),
        };
        let job = match shown {
            Some(job) => {
                let mut value = job.json();
                value["result"] = json!(jobs::with_loss(job.result.clone()));
                value["steps"] = Value::Array(jobs::steps_json(&**client, tenant, job.id).await?);
                value["cancel"] = json!(cancel_state(&**client, tenant, &job).await?);
                value
            }
            None => Value::Null,
        };
        let undo = match jobs::last_done(&**client, tenant).await? {
            Some(done) if active.is_none() => match review::undo_facts(&done) {
                Ok(facts) => {
                    let backup_at = if facts.restores_backup {
                        jobs::backup_time(&**client, tenant, &done).await?
                    } else {
                        None
                    };
                    json!({"available": true, "job": done.id, "restores_backup": facts.restores_backup,
                                    "release": facts.release, "previous_release": facts.previous_release,
                                    "backup_at": backup_at})
                }
                Err(reason) => json!({"available": false, "job": done.id, "reason": reason}),
            },
            Some(done) => {
                json!({"available": false, "job": done.id, "reason": "A job is waiting or running."})
            }
            None => {
                json!({"available": false, "reason": "Nothing to undo: no change has finished on this instance."})
            }
        };
        Ok(json!({
            "instance": review::instance_json(&instance),
            "installed": review::installed_json(&self.composition),
            "plugins": jobs::plugins_json(&**client, tenant).await?,
            "uploads": self.paths.as_ref().map(transport::Paths::uploads).unwrap_or_default().into_iter()
                .map(|(bundle, facts)| json!({"bundle": bundle, "release": facts["release"], "from": facts["from"]["label"]}))
                .collect::<Vec<_>>(),
            "pairing": pairing::state_json(pairing::is_set(&**client, &self.settings, scope).await?),
            "backups": jobs::backups_json(&**client, tenant).await?,
            "backup_policy": jobs::BACKUP_POLICY,
            "backup_undo": jobs::BACKUP_UNDO,
            "job": job,
            "undo": undo,
            "jobs": recent.iter().map(|job| json!({"id": job.id, "kind": job.kind, "state": job.state,
                "summary": job.review["summary"], "requested_at": job.requested_at, "finished_at": job.finished_at})).collect::<Vec<_>>(),
        }))
    }

    /// Discovery and local build instructions on every instance. Proposals
    /// highlight catalogue entries; they never create a server job.
    pub async fn open(&self, scope: &Scope, input: &Value) -> Result<Value> {
        let mut value = self.status(scope).await?;
        let catalogue = self.catalogue(scope).await?;
        let proposed = &input["propose"];
        let wanted = |kind: &str| -> Vec<String> {
            proposed[kind]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        let mut unknown = Vec::new();
        let mut proposal = json!({});
        for kind in ["domains", "extensions", "plugins"] {
            let (known, missing): (Vec<_>, Vec<_>) = wanted(kind).into_iter().partition(|id| {
                catalogue[kind]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|item| item["id"] == *id)
            });
            proposal[kind] = json!(known);
            unknown.extend(missing);
        }
        proposal["unknown"] = json!(unknown);
        value["proposal"] = proposal;
        value["catalogue"] = catalogue;
        value["next_step"] = json!(discovery::NEXT_STEP);
        value["card"] = json!({"resource": CARD_URI});
        Ok(value)
    }

    /// Puts the outcomes the installer wrote into the audit
    /// trail: each finished job becomes one audited change, from the job as
    /// queued to the job as the worker left it, attributed to that worker as
    /// a system principal. Returns how many were settled.
    pub async fn settle(&self) -> Result<usize> {
        let mut client = self.db.get().await?;
        let pending = jobs::unsettled(&**client).await?;
        let mut settled = 0;
        for (tenant, job) in pending {
            let worker = job.worker.clone().unwrap_or_else(|| "worker".into());
            let mut tx = self
                .mutations
                .begin(
                    &mut client,
                    Context::system(tenant, &worker, ACTION_FINISHED),
                )
                .await?;
            let recorded = jobs::settle(&mut tx, tenant, &job).await?;
            tx.commit().await?;
            if recorded {
                settled += 1;
                if let Err(error) = self.announce(&mut client, tenant, &job, &worker).await {
                    eprintln!(
                        "warning: software job {} was recorded but its notification was not sent: {error:?}",
                        job.id
                    );
                }
            }
        }
        Ok(settled)
    }
    /// Tells the person who asked how their job ended after the outcome is
    /// recorded, so a notification that
    /// cannot be sent never keeps a job unsettled. Their account is
    /// re-proved live for the notification's scope; an account that is gone
    /// gets none.
    async fn announce(
        &self,
        client: &mut tokio_postgres::Client,
        tenant: i64,
        job: &jobs::Job,
        worker: &str,
    ) -> Result<()> {
        let mut tx = self
            .mutations
            .begin(client, Context::system(tenant, worker, ACTION_NOTIFIED))
            .await?;
        let title = match job.state.as_str() {
            "done" => format!("Software job {} finished", job.id),
            "failed" => format!("Software job {} failed", job.id),
            "interrupted" => format!("Software job {} was interrupted", job.id),
            _ => format!("Software job {} stopped", job.id),
        };
        let body = brief(
            job.result
                .as_ref()
                .and_then(|result| result["message"].as_str())
                .unwrap_or_default(),
        );
        let user = job.requested_by;
        match self
            .authentication
            .authenticate(
                tx.sql(),
                tenant,
                user,
                format!("software-settle-{tenant}-{user}"),
            )
            .await
        {
            Ok(scope) => {
                self.notifications
                    .notify(
                        &mut tx,
                        &scope,
                        &[user],
                        &Notice::new(title).kind("software.job").body(&body),
                    )
                    .await?;
            }
            Err(_) => {
                eprintln!(
                    "software: account {user} of tenant {tenant} cannot be notified (not active)"
                );
            }
        }
        tx.commit().await
    }
    /// Runs beside the server for as long as it serves (`bilibi-web run`
    /// starts it like the queue executor): settles outcomes every few
    /// seconds, so a job the installer finished while this program was
    /// restarting is recorded as soon as the new program is up.
    pub async fn run_settling(
        self: Arc<Self>,
        mut stop: tokio::sync::watch::Receiver<bool>,
    ) -> Result<()> {
        loop {
            if *stop.borrow() {
                return Ok(());
            }
            if let Err(error) = self.settle().await {
                eprintln!("warning: software outcomes were not settled this round: {error:?}");
            }
            tokio::select! {
                _ = stop.changed() => {}
                _ = tokio::time::sleep(SETTLE_EVERY) => {}
            }
        }
    }

    /// What restoring the backup of finished job `job` would lose: the time
    /// of that backup and how many audited changes were entered since,
    /// counted now. `count` is null when the job restores no backup or its
    /// backup time is unknown. A separate read, because the count changes
    /// with every write and is not part of what a confirmation is bound to.
    pub async fn undo_loss(&self, scope: &Scope, job: i64) -> Result<Value> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let tenant = scope.tenant();
        let Some(job) = jobs::get(&**client, tenant, job).await? else {
            return Ok(json!({"job": job, "backup_at": null, "count": null}));
        };
        let restores = review::undo_facts(&job).is_ok_and(|facts| facts.restores_backup);
        let backup_at = if restores {
            jobs::backup_time(&**client, tenant, &job).await?
        } else {
            None
        };
        let count = match &backup_at {
            Some(at) => jobs::changes_since(&**client, at).await?["changes"].as_i64(),
            None => None,
        };
        Ok(json!({"job": job.id, "backup_at": backup_at, "count": count}))
    }

    /// The published catalogue, or why it could not be read.
    pub async fn catalogue(&self, scope: &Scope) -> Result<Value> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let url = self.catalogue_url(&**client).await?;
        Ok(match self.catalogue.read(scope, &url).await {
            Ok(catalogue) => {
                let mut value = discovery::catalogue(&catalogue, &self.composition);
                value["available"] = json!(true);
                value["url"] = json!(url);
                value
            }
            Err(message) => json!({"available": false, "url": url, "message": message}),
        })
    }

    /// What installing a release tested on the source instance would do.
    pub async fn review_install(&self, scope: &Scope, input: &Value) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        self.install_review(&**client, scope, input).await
    }
    pub async fn install_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        let review = self.install_review(tx.sql(), scope, input).await?;
        self.step_up.require(scope, STEP_UP, &review.target).await?;
        let downloads = review.request["source"]["kind"].as_str() == Some("https");
        let new = jobs::NewJob {
            kind: "install",
            request: review.request.clone(),
            review: review.facts.clone(),
            undo_of: None,
            switch: review.switch.clone(),
            steps: jobs::install_steps(downloads),
        };
        self.queue(tx, scope, new, &review).await
    }

    /// What taking back the last finished change would do, and what it costs.
    pub async fn review_undo(&self, scope: &Scope, input: &Value) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        self.undo_review(&**client, scope, input).await
    }
    pub async fn undo_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        let review = self.undo_review(tx.sql(), scope, input).await?;
        self.step_up.require(scope, STEP_UP, &review.target).await?;
        let restores_backup = review.facts["restores_backup"].as_bool().unwrap_or(false);
        let mut recorded = review.facts.clone();
        if let Some(at) = review.facts["backup_at"].as_str() {
            recorded["lost_at_commit"] = json!(jobs::changes_since(tx.sql(), at).await?);
        }
        let new = jobs::NewJob {
            kind: "undo",
            request: review.request.clone(),
            review: recorded,
            undo_of: review.facts["job"].as_i64(),
            switch: "auto".into(),
            steps: jobs::undo_steps(restores_backup),
        };
        self.queue(tx, scope, new, &review).await
    }

    /// Whether the job waiting or running can still be stopped, and what
    /// stopping it means.
    pub async fn review_cancel(&self, scope: &Scope, input: &Value) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        self.cancel_review(&**client, scope, input).await
    }
    pub async fn cancel_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        let review = self.cancel_review(tx.sql(), scope, input).await?;
        let id = review.facts["job"].as_i64().ok_or(Error::Internal)?;
        let job = jobs::cancel(tx, scope.tenant(), scope.user(), id).await?;
        let title = if job.state == "cancelled" {
            format!("Software job {} cancelled", job.id)
        } else {
            format!("Software job {} asked to stop", job.id)
        };
        self.notifications
            .notify(
                tx,
                scope,
                &[scope.user()],
                &Notice::new(title)
                    .kind("software.job")
                    .body(review.facts["summary"].as_str().unwrap_or_default()),
            )
            .await?;
        if job.state == "cancelled" {
            self.drop_download(&job);
        }
        let mut value = job.json();
        value["review"] = review.facts;
        Ok(value)
    }

    /// What giving a copied install its go-ahead would do.
    pub async fn review_go(&self, scope: &Scope, input: &Value) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        self.go_review(&**client, scope, input).await
    }
    /// Turns a parked job's `switch` to `auto` inside the caller's
    /// transaction, after the owner's fresh 2FA check.
    pub async fn go_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        let review = self.go_review(tx.sql(), scope, input).await?;
        self.step_up.require(scope, STEP_UP, &review.target).await?;
        let id = review.facts["job"].as_i64().ok_or(Error::Internal)?;
        let job = jobs::go(tx, scope.tenant(), id).await?;
        self.notifications
            .notify(
                tx,
                scope,
                &[scope.user()],
                &Notice::new(format!("Software job {}: go-ahead given", job.id))
                    .kind("software.job")
                    .body(review.facts["summary"].as_str().unwrap_or_default()),
            )
            .await?;
        let mut value = job.json();
        value["review"] = review.facts;
        Ok(value)
    }

    async fn queue(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        new: jobs::NewJob,
        review: &Review,
    ) -> SoftwareResult<Value> {
        let kind = new.kind;
        let job = jobs::queue(tx, scope.tenant(), scope.user(), new).await?;
        let title = match kind {
            "install" => format!("Software install queued as job {}", job.id),
            _ => format!("Software undo queued as job {}", job.id),
        };
        self.notifications
            .notify(
                tx,
                scope,
                &[scope.user()],
                &Notice::new(title)
                    .kind("software.job")
                    .body(review.facts["summary"].as_str().unwrap_or_default()),
            )
            .await?;
        let mut value = job.json();
        value["steps"] = json!(review.facts["steps"]);
        Ok(value)
    }

    async fn catalogue_url(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
    ) -> Result<String> {
        Ok(self
            .settings
            .get_global(client, CATALOGUE_URL_KEY)
            .await?
            .as_str()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(DEFAULT_CATALOGUE_URL)
            .to_owned())
    }

    async fn install_review(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let instance = Instance::resolve(&self.settings, client).await?;
        let active = jobs::active(client, scope.tenant()).await?;
        // The source is asked only for a release of its own; an uploaded
        // file needs no source.
        let source = match (&instance.installs_from, input["bundle"].as_str()) {
            (Some(from), None) => Some(review::SourceFacts {
                listing: self.source_listing(scope, from).await?,
            }),
            _ => None,
        };
        let uploads = self
            .paths
            .as_ref()
            .map(transport::Paths::uploads)
            .unwrap_or_default();
        let running_sha256 = self
            .paths
            .as_ref()
            .and_then(|paths| paths.current().map(|n| paths.release(n)))
            .and_then(|dir| transport::expected_checksum(&dir).ok());
        review::install(
            &instance,
            &self.composition,
            active.as_ref(),
            input,
            source.as_ref(),
            &review::Held {
                uploads: &uploads,
                running_sha256: running_sha256.as_deref(),
                enabled_plugins: &jobs::enabled_plugins(client, scope.tenant()).await?,
            },
        )
    }
    async fn undo_review(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let tenant = scope.tenant();
        let instance = Instance::resolve(&self.settings, client).await?;
        let active = jobs::active(client, tenant).await?;
        let last = jobs::last_done(client, tenant).await?;
        let named = match input["job"].as_i64() {
            Some(id) => jobs::get(client, tenant, id).await?,
            None => None,
        };
        let backup_at = match last.as_ref().map(|job| (job, review::undo_facts(job))) {
            Some((job, Ok(facts))) if facts.restores_backup => {
                jobs::backup_time(client, tenant, job).await?
            }
            _ => None,
        };
        review::undo(
            &instance,
            active.as_ref(),
            last.as_ref(),
            named.as_ref(),
            backup_at.as_deref(),
            input,
        )
    }
    async fn go_review(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let tenant = scope.tenant();
        let instance = Instance::resolve(&self.settings, client).await?;
        let active = jobs::active(client, tenant).await?;
        let named = match input["job"].as_i64() {
            Some(id) => jobs::get(client, tenant, id).await?,
            None => None,
        };
        let job = named.or(active).ok_or_else(|| {
            SoftwareError::field("job", "No job is waiting or running on this instance.")
        })?;
        let built = jobs::parked_release(client, tenant, &job).await?;
        review::go(&instance, &job, built)
    }
    async fn cancel_review(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let tenant = scope.tenant();
        let active = jobs::active(client, tenant).await?;
        let named = match input["job"].as_i64() {
            Some(id) => jobs::get(client, tenant, id).await?,
            None => None,
        };
        let job = named.or(active).ok_or_else(|| {
            SoftwareError::field("job", "No job is waiting or running on this instance.")
        })?;
        let past_pause = jobs::past_pause(client, tenant, job.id).await?;
        review::cancel(&job, past_pause)
    }
}

/// `text` cut at a word boundary to fit a notification, with an ellipsis.
fn brief(text: &str) -> String {
    if text.chars().count() <= NOTICE_BODY_CHARS {
        return text.to_owned();
    }
    let cut: String = text.chars().take(NOTICE_BODY_CHARS - 1).collect();
    let words = cut
        .rsplit_once(char::is_whitespace)
        .map_or(cut.as_str(), |(head, _)| head);
    format!("{}…", words.trim_end())
}

/// `"available"`, `"requested"`, `"too_late"` or `"not_running"`.
async fn cancel_state(
    client: &impl tokio_postgres::GenericClient,
    tenant: i64,
    job: &jobs::Job,
) -> Result<&'static str> {
    Ok(if !job.active() {
        "not_running"
    } else if job.cancel_requested_at.is_some() {
        "requested"
    } else if job.state == "running" && jobs::past_pause(client, tenant, job.id).await? {
        "too_late"
    } else {
        "available"
    })
}

macro_rules! provenance {
    () => {
        Provenance {
            module: "base/software",
            source: file!(),
            line: line!(),
        }
    };
}
inventory::submit! { Contribution::new(VOCABULARY, "base/software", provenance!(), &VocabularyContribution {
    domains: &[], verbs: &[],
    capabilities: &[CapabilityDefinition::platform(VIEW), CapabilityDefinition::platform(MANAGE)],
    roles: &[],
    // The source reads its own pairing key as this principal on a request from
    // another instance, which has no session (`pairing.rs`).
    system_principals: &[SystemPrincipalDefinition { name: pairing::PRINCIPAL, capabilities: &[] }],
}) }
inventory::submit! { Contribution::new(DEFINITIONS, CATALOGUE_URL_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Global], default: || json!(DEFAULT_CATALOGUE_URL), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.as_str().is_some_and(|url| { let url = url.trim(); url.starts_with("https://") && url.len() <= 2048 && !url.chars().any(char::is_whitespace) }), message: "The catalogue address must start with https://, like https://raw.githubusercontent.com/BelimbingApp/bilibi/catalogue/catalogue.json." }),
    editor: Some(Editor { group: "Software", label: "Catalogue address: where the list of Domains and releases is read from",
        help: "The published catalogue.json that lists the Platform and Domain releases and which were tested together. Change it only to read a catalogue you publish yourself (server owner only).",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, pairing::KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Global], default: || json!(null), nullable: true, encrypted: true,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: pairing::accepts_value, message: "The pairing key must be 16 to 128 characters with no spaces, like the output of: openssl rand -hex 24" }),
    editor: Some(Editor { group: "Software", label: "Pairing key other instances use to download releases from this one",
        help: "Set the same key on every instance that shares releases: the one that installs presents it, the one it installs from checks it. Stored encrypted and never shown back. Changing or clearing it asks for your authenticator code every time (server owner only).",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
// The Software page: server owners' view of what runs and the way to change
// it. Seeing it needs the platform `view` capability; the owner's facade
// rechecks `manage` on every write.
inventory::submit! { Contribution::new(ROUTES, "base/software.index", provenance!(), &Route {
    method: Method::Get, path: web::PATH, service: web::PAGES, operation: "index",
    access: Access::OperatorCapability(VIEW), framed: true,
}) }
// Bilimbi's menu has "admin.system.software" as an empty container; here it
// is the page itself.
bilibi_menu::contribute!(
    "base/software",
    bilibi_menu::MenuItem::page("admin.system.software", "Software", web::PATH)
        .under("admin.system")
        .icon("hero-cube")
        .requires(bilibi_menu::Requirement::Capability(VIEW))
        .order(40)
);
// The page's writes are this module's own contracts (ADR 01): a reviewed
// plan for an install, undo or go-ahead, and a plain submit for cancel.
inventory::submit! { Contribution::new(PAGE_WRITES, web::PAGES, provenance!(), &PageWrites {
    service: web::PAGES,
    contracts: &[("install", "base.software.install"), ("undo", "base.software.undo"), ("go", "base.software.go"), ("cancel_job", "base.software.cancel")],
    pending: Pending::None,
}) }
// The Release files page (`release_files.rs`): the releases kept here with their
// download links, the pairing key, and the upload of a release file. Seeing
// it needs the platform `view` capability; the facade rechecks `manage` on
// every write. The Software page (`web.rs`) is the place to install.
inventory::submit! { Contribution::new(ROUTES, "base/software.releases", provenance!(), &Route {
    method: Method::Get, path: release_files::PATH, service: release_files::PAGES, operation: "releases",
    access: Access::OperatorCapability(VIEW), framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "base/software.release_file", provenance!(), &Download {
    path: release_files::DOWNLOAD_PATH, service: release_files::FILES, operation: "release_file",
    access: Access::OperatorCapability(MANAGE),
}) }
bilibi_menu::contribute!(
    "base/software",
    bilibi_menu::MenuItem::page(
        "admin.system.release_files",
        "Release files",
        release_files::PATH
    )
    .under("admin.system")
    .icon("hero-archive-box-arrow-down")
    .requires(bilibi_menu::Requirement::Capability(VIEW))
    .order(41)
);
// The page's only business write is the pairing key, through its contract
// (ADR 01). Receiving an uploaded file writes the instance folder, not the
// database.
inventory::submit! { Contribution::new(PAGE_WRITES, release_files::PAGES, provenance!(), &PageWrites {
    service: release_files::PAGES,
    contracts: &[
        ("pair", "base.software.pair"),
        ("clear_pairing", "base.software.pair"),
    ],
    pending: Pending::None,
}) }
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/software", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/agent_api", "base/settings", "base/step_up", "base/system", "base/notifications", "base/integration", "base/audit", "base/ui", "base/menu", "base/datetime", "base/locale"],
        migrations: &[("base-software-001-jobs", include_str!("../migrations/001-jobs.sql")), ("base-software-002-settled", include_str!("../migrations/002-settled.sql")), ("base-software-003-local-builds", include_str!("../migrations/003-local-builds.sql"))],
        // Declaration order is build order: the page service reads this one.
        services: &[bilibi_module_registry::Service { id: SERVICE, factory: |services| {
            let software = Arc::new(Software {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                mutations: services.get("base/database.mutations")?,
                settings: services.get("base/settings.store")?,
                step_up: services.get(bilibi_step_up::SERVICE)?,
                notifications: services.get("base/notifications.sink")?,
                authentication: services.get("base/tenancy.authentication")?,
                // What this program is made of, as the host compiled it in.
                composition: services.get::<Composition>("bootstrap.composition")?,
                catalogue: catalogue::Reader::new(services.get("base/integration.exchanges")?),
                integration: services.get("base/integration.exchanges")?,
                paths: transport::Paths::from_env(),
                origin: std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),
            });
            BOUND.set(software.clone()).map_err(|_| bilibi_module_registry::RegistryError::Service("duplicate software binding".into()))?;
            Ok(software)
        } }, bilibi_module_registry::Service { id: web::PAGES, factory: |services| {
            let db = services.get::<Db>("base/database.pool")?.as_ref().clone();
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(web::Pages {
                follow: Arc::new(follow::Follow::new(db.clone())),
                db,
                settings: services.get("base/settings.store")?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                display: services.get("base/datetime.display")?,
                locales: services.get("base/locale.locales")?,
                software: services.get(SERVICE)?,
            }))))
        } }, bilibi_module_registry::Service { id: release_files::PAGES, factory: |services| {
            Ok(Arc::new(bilibi_ui::page::PageService::<Scope, Error>(Arc::new(release_files::Pages {
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                settings: services.get("base/settings.store")?,
                contracts: services.get(bilibi_agent_api::page::SERVICE)?,
                software: services.get(SERVICE)?,
            }))))
        } }, bilibi_module_registry::Service { id: release_files::FILES, factory: |services| {
            Ok(Arc::new(bilibi_ui::download::DownloadService::<Scope, Error>(Arc::new(release_files::Files { software: services.get(SERVICE)? }))))
        } }],
    }
}
