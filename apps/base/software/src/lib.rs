//! The software of this server: what is installed, what the catalogue
//! offers, and the server owner's requests to change it (build on Dev,
//! install a tested release elsewhere, undo, cancel). This module records and
//! validates those requests and shows their progress; a builder and an
//! installer, separate services, pick the queued job up (`README.md`, "How a
//! worker plugs in"). No Bilimbi counterpart: the Software page is Bilibi's.
//!
//! Facts come from one place each: the running program for the Domains and
//! versions (`bilibi_module_registry::Composition`, handed in by the host as
//! `bootstrap.composition`), `bilibi_system::Instance` for which instance
//! this is, the published catalogue for what can be chosen, and the job
//! tables here for history and plug-ins.
mod agent;
mod builder;
mod catalogue;
mod jobs;
mod review;
mod toolchain;
mod worker;

use bilibi_authz::{authorize_operator, CapabilityDefinition, VocabularyContribution, VOCABULARY};
use bilibi_database::{Context, Db, Error, MutationBoundary, MutationTransaction, Result};
use bilibi_module_registry::{Composition, Contribution, Provenance};
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
pub use worker::{Worker, PRINCIPAL as WORKER_PRINCIPAL, USAGE as WORKER_USAGE};

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
/// one build changes every company on the server.
pub const MANAGE: &str = "admin.system.software.manage";
/// The step-up operation every change asks for: a fresh code every time, on
/// every instance, Dev included, because a build changes the server itself.
pub const STEP_UP: &str = "base.software.change";
/// Where the published catalogue is read from.
pub const CATALOGUE_URL_KEY: &str = "software.catalogue_url";
/// How many compiler processes a build may run at once; 0 lets Cargo decide.
pub const BUILD_JOBS_KEY: &str = "software.build_jobs";
/// The action names the audit trail shows.
pub const ACTION_CHANGE: &str = "software.change";
pub const ACTION_INSTALL: &str = "software.install";
pub const ACTION_UNDO: &str = "software.undo";
pub const ACTION_CANCEL: &str = "software.cancel";
/// The action a worker's outcome is settled under (`Software::settle`).
pub const ACTION_FINISHED: &str = "software.finished";
/// How often the server records the outcomes workers wrote.
const SETTLE_EVERY: std::time::Duration = std::time::Duration::from_secs(15);

bilibi_step_up::declare!(
    "base/software",
    bilibi_step_up::Operation::new(
        STEP_UP,
        "Change the server's software (build, install, undo)"
    )
    .always()
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
}
static BOUND: OnceLock<Arc<Software>> = OnceLock::new();
/// The installed facade, for the agent contracts of this module.
fn bound() -> Result<&'static Arc<Software>> {
    BOUND.get().ok_or(Error::Internal)
}

impl Software {
    /// Whether this instance builds, what it announces itself as, what is
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
                    json!({"available": true, "job": done.id, "restores_backup": facts.restores_backup,
                                    "release": facts.release, "previous_release": facts.previous_release})
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
            "job": job,
            "undo": undo,
            "jobs": recent.iter().map(|job| json!({"id": job.id, "kind": job.kind, "state": job.state,
                "summary": job.review["summary"], "requested_at": job.requested_at, "finished_at": job.finished_at})).collect::<Vec<_>>(),
        }))
    }

    /// Puts the outcomes the builder and the installer wrote into the audit
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
            if jobs::settle(&mut tx, tenant, &job).await? {
                self.announce(&mut tx, tenant, &job).await?;
                settled += 1;
            }
            tx.commit().await?;
        }
        Ok(settled)
    }
    /// Tells the person who asked how their job ended, as the builder does
    /// for its own. Their account is re-proved live for the notification's
    /// scope; an account that is gone gets none, and the outcome is still
    /// recorded.
    async fn announce(
        &self,
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        job: &jobs::Job,
    ) -> Result<()> {
        let title = match job.state.as_str() {
            "done" => format!("Software job {} finished", job.id),
            "failed" => format!("Software job {} failed", job.id),
            "interrupted" => format!("Software job {} was interrupted", job.id),
            _ => format!("Software job {} stopped", job.id),
        };
        let body = job
            .result
            .as_ref()
            .and_then(|result| result["message"].as_str())
            .unwrap_or_default();
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
                        tx,
                        &scope,
                        &[user],
                        &Notice::new(title).kind("software.job").body(body),
                    )
                    .await
            }
            Err(_) => {
                eprintln!(
                    "software: account {user} of tenant {tenant} cannot be notified (not active)"
                );
                Ok(())
            }
        }
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

    /// The published catalogue, or why it could not be read.
    pub async fn catalogue(&self, scope: &Scope) -> Result<Value> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, VIEW).await?;
        let url = self.catalogue_url(&**client).await?;
        Ok(match self.catalogue.read(scope, &url).await {
            Ok(catalogue) => {
                let mut value = catalogue.json();
                value["available"] = json!(true);
                value["url"] = json!(url);
                value
            }
            Err(message) => json!({"available": false, "url": url, "message": message}),
        })
    }

    /// What a change (a build on Dev) would do, read without writing.
    pub async fn review_change(&self, scope: &Scope, input: &Value) -> SoftwareResult<Review> {
        let client = self.db.get().await?;
        authorize_operator(&**client, scope, MANAGE).await?;
        self.change_review(&**client, scope, input).await
    }
    /// Records the reviewed change as a queued job inside the caller's
    /// transaction, after the owner's fresh 2FA check.
    pub async fn change_in(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Value> {
        authorize_operator(tx.sql(), scope, MANAGE).await?;
        let review = self.change_review(tx.sql(), scope, input).await?;
        self.step_up.require(scope, STEP_UP, &review.target).await?;
        let new = jobs::NewJob {
            kind: "change",
            request: review.request.clone(),
            review: review.facts.clone(),
            undo_of: None,
            switch: review.switch.clone(),
            steps: jobs::change_steps(),
        };
        self.queue(tx, scope, new, &review).await
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
        let new = jobs::NewJob {
            kind: "install",
            request: review.request.clone(),
            review: review.facts.clone(),
            undo_of: None,
            switch: review.switch.clone(),
            steps: jobs::install_steps(),
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
        let new = jobs::NewJob {
            kind: "undo",
            request: review.request.clone(),
            review: review.facts.clone(),
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
            "change" => format!("Software change queued as job {}", job.id),
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

    async fn change_review(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let tenant = scope.tenant();
        let instance = Instance::resolve(&self.settings, client).await?;
        let active = jobs::active(client, tenant).await?;
        let url = self.catalogue_url(client).await?;
        let catalogue = self.catalogue.read(scope, &url).await;
        let enabled = jobs::enabled_plugins(client, tenant).await?;
        review::change(
            &instance,
            &self.composition,
            catalogue.as_deref().map_err(Clone::clone),
            &enabled,
            active.as_ref(),
            input,
        )
    }
    async fn install_review(
        &self,
        client: &(impl tokio_postgres::GenericClient + Sync),
        scope: &Scope,
        input: &Value,
    ) -> SoftwareResult<Review> {
        let instance = Instance::resolve(&self.settings, client).await?;
        let active = jobs::active(client, scope.tenant()).await?;
        review::install(&instance, &self.composition, active.as_ref(), input)
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
        // What a restore would lose, counted now so the owner reads a number.
        let lost = match last.as_ref().map(review::undo_facts) {
            Some(Ok(facts)) if facts.restores_backup => match &facts.backup_at {
                Some(since) => Some(jobs::changes_since(client, since).await?),
                None => None,
            },
            _ => None,
        };
        review::undo(
            &instance,
            active.as_ref(),
            last.as_ref(),
            named.as_ref(),
            lost.as_ref(),
            input,
        )
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
    roles: &[], system_principals: &[],
}) }
inventory::submit! { Contribution::new(DEFINITIONS, CATALOGUE_URL_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::String, scopes: &[ScopeKind::Global], default: || json!(DEFAULT_CATALOGUE_URL), nullable: false, encrypted: false,
    minimum: None, maximum: None,
    validator: Some(Validator { accepts: |value: &Value| value.as_str().is_some_and(|url| { let url = url.trim(); url.starts_with("https://") && url.len() <= 2048 && !url.chars().any(char::is_whitespace) }), message: "The catalogue address must start with https://, like https://raw.githubusercontent.com/BelimbingApp/bilibi/catalogue/catalogue.json." }),
    editor: Some(Editor { group: "Software", label: "Catalogue address: where the list of Domains and releases is read from",
        help: "The published catalogue.json that lists the Platform and Domain releases and which were tested together. Change it only to read a catalogue you publish yourself (server owner only).",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! { Contribution::new(DEFINITIONS, BUILD_JOBS_KEY, provenance!(), &Setting::Definition(Definition {
    value_type: ValueType::Integer, scopes: &[ScopeKind::Global], default: || json!(2), nullable: false, encrypted: false,
    minimum: Some(0.0), maximum: Some(64.0), validator: None,
    editor: Some(Editor { group: "Software", label: "Compilers at a time: how many parts a build compiles at once",
        help: "How many parts the build compiles at the same time. 2 keeps a Dev that shares its machine with Production responsive and takes about 10 to 15 minutes for a first build. Use 1 on a small server, or a higher number on a machine of its own. 0 means use every processor, which is fastest but slows everything else on the server. Builds always run at the lowest CPU priority (server owner only).",
        capability: Some(MANAGE), reveal_duration_ms: 5_000 }),
})) }
inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/software", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry", "base/database", "base/tenancy", "base/authz", "base/agent_api", "base/settings", "base/step_up", "base/system", "base/notifications", "base/integration", "base/audit"],
        migrations: &[("base-software-001-jobs", include_str!("../migrations/001-jobs.sql")), ("base-software-002-settled", include_str!("../migrations/002-settled.sql"))],
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
            });
            BOUND.set(software.clone()).map_err(|_| bilibi_module_registry::RegistryError::Service("duplicate software binding".into()))?;
            Ok(software)
        } }],
    }
}
