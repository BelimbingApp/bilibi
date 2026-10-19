//! The builder service: `bilibi-web software-worker`, run as `bilibi-build`
//! by `bilibi-worker@<instance>` beside the instance it builds for
//! (`docs/server-install.md`). It follows the worker contract of
//! `README.md` ("How a worker plugs in"): wake on `NOTIFY`, claim a queued
//! change job, report each step, honour a cancel, record the outcome as an
//! audited action and a notification. It builds only on Dev: on any other
//! role a change job is refused with the reason, and nothing is compiled.
//! Install and undo jobs belong to the installer, a separate piece; a change
//! job whose build is done is handed to it through the job row (`hand_off`).
use crate::{
    builder::{Build, Built, Failure, Paths, Plan},
    jobs::{Job, JOB_COLUMNS},
    toolchain::Toolchain,
    BUILD_JOBS_KEY, CHANNEL, RECORD,
};
use bilibi_audit::{record_action, Attribution};
use bilibi_database::{Change, Context, Db, Error, MutationBoundary, Result};
use bilibi_module_registry::{Composition, Layer, Services};
use bilibi_notifications::{Notice, NotificationBoundary};
use bilibi_settings::Settings;
use bilibi_system::{Instance, Role};
use bilibi_tenancy::Authentication;
use serde_json::{json, Value};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicI32, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, watch};
use tokio_postgres::{AsyncMessage, GenericClient, NoTls};

/// The system principal the audit trail shows for the builder's writes.
pub const PRINCIPAL: &str = "software-builder";
/// The action kinds the builder records (`base_audit_actions`).
pub const ACTION_COMPLETED: &str = "software.build.completed";
pub const ACTION_FAILED: &str = "software.build.failed";
pub const ACTION_CANCELLED: &str = "software.build.cancelled";
pub const ACTION_REFUSED: &str = "software.build.refused";
pub const ACTION_INTERRUPTED: &str = "software.build.interrupted";
pub const USAGE: &str = "usage: bilibi-web software-worker [--once | --prepare]
Runs the software builder for this instance (Dev only builds; the role is BILIBI_ENV).
  --once     handle the job waiting now, then exit
  --prepare  install the Rust build tools for this user now, then exit
Reads BILIBI_INSTANCE_DIR (the instance folder with releases/) and BILIBI_BUILD_DIR
(the build folder, default /opt/bilibi/build) from the environment.";

/// A notification while nothing is queued is not expected; poll this often
/// anyway, because a NOTIFY sent while the worker was down is lost.
const POLL: Duration = Duration::from_secs(30);
/// How often a running build looks for a cancel.
const CANCEL_POLL: Duration = Duration::from_secs(2);
/// The steps of a change job the builder owns; the rest are the installer's.
const BUILDER_STEPS: [&str; 3] = ["check", "fetch", "build"];

pub struct Worker {
    db: Db,
    settings: Arc<Settings>,
    mutations: Arc<MutationBoundary>,
    notifications: Arc<NotificationBoundary>,
    authentication: Arc<Authentication>,
    composition: Arc<Composition>,
    database_url: String,
    paths: Paths,
    toolchain: Toolchain,
    /// This worker's name in the job row: `builder@<host>`.
    name: String,
}
impl Worker {
    /// The host's services (`bilibi_web::App::new`) and the database address
    /// for the notification connection. Paths come from the environment.
    pub fn new(services: &Services, database_url: String) -> Result<Self> {
        let paths = Paths::from_env().map_err(|why| {
            eprintln!("software-worker: {why}");
            Error::Invalid
        })?;
        let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|h| h.trim().to_owned())
            .unwrap_or_else(|_| "host".into());
        Ok(Self {
            db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
            settings: services.get("base/settings.store")?,
            mutations: services.get("base/database.mutations")?,
            notifications: services.get("base/notifications.sink")?,
            authentication: services.get("base/tenancy.authentication")?,
            composition: services.get("bootstrap.composition")?,
            database_url,
            toolchain: Toolchain::in_home(&paths.home),
            paths,
            name: format!("builder@{host}").chars().take(64).collect(),
        })
    }
    async fn instance(&self) -> Result<Instance> {
        let client = self.db.get().await?;
        Instance::resolve(&self.settings, &**client).await
    }
    /// "Prepare this server for builds": the toolchain, now, on Dev only.
    pub async fn prepare(&self) -> Result<()> {
        let instance = self.instance().await?;
        if !instance.role.builds() {
            eprintln!("{}", refusal(&instance));
            return Err(Error::Forbidden);
        }
        if let Some(version) = self.toolchain.version().await {
            println!(
                "Build tools ready: Rust {version} in {}",
                self.toolchain.bin_dir().display()
            );
            return Ok(());
        }
        println!(
            "Installing the Rust build tools into {} (about 1.5 GB, once)",
            self.paths.home.display()
        );
        match self.toolchain.install(&self.paths.build_dir).await {
            Ok(version) => {
                println!("Build tools ready: Rust {version}");
                Ok(())
            }
            Err(why) => {
                eprintln!("The Rust build tools could not be installed: {why}");
                Err(Error::Internal)
            }
        }
    }
    /// The service loop: recover from a restart, then handle queued jobs as
    /// they are announced, or every `POLL` at the latest.
    pub async fn run(&self, once: bool) -> Result<()> {
        let instance = self.instance().await?;
        let tools = match self.toolchain.version().await {
            Some(version) => format!("Rust {version}"),
            None => "no Rust yet (installed on the first build)".into(),
        };
        println!(
            "Software worker {} on {} ({}): {}; builds go to {}, releases to {}",
            self.name,
            instance.label,
            if instance.role.builds() {
                "builds"
            } else {
                "never builds"
            },
            tools,
            self.paths.build_dir.display(),
            self.paths.releases().display()
        );
        self.recover().await?;
        // Ctrl-C (systemd's stop signal) ends the loop; a build in flight is
        // stopped and its job marked interrupted first, so no compiler is
        // left behind and no job is left running.
        let (stop_tx, mut stop) = watch::channel(false);
        tokio::spawn(async move {
            let _ = tokio::signal::ctrl_c().await;
            let _ = stop_tx.send(true);
        });
        let mut wake = self.listen().await;
        loop {
            self.sweep(&stop).await?;
            if once || *stop.borrow() {
                return Ok(());
            }
            match &mut wake {
                Some(listener) => {
                    tokio::select! {
                        heard = listener.rx.recv() => {
                            if heard.is_none() {
                                wake = None;
                            }
                        }
                        () = tokio::time::sleep(POLL) => {}
                        _ = stop.changed() => {}
                    }
                }
                None => {
                    tokio::select! {
                        () = tokio::time::sleep(POLL) => {}
                        _ = stop.changed() => {}
                    }
                    wake = self.listen().await;
                }
            }
        }
    }

    /// A dedicated connection on `LISTEN bilibi_software_jobs`; `None` when
    /// it cannot be opened, and the loop then polls until it can.
    async fn listen(&self) -> Option<Listener> {
        let (client, mut connection) =
            match tokio_postgres::connect(&self.database_url, NoTls).await {
                Ok(opened) => opened,
                Err(e) => {
                    eprintln!("software-worker: cannot open the notification connection: {e}");
                    return None;
                }
            };
        let (tx, rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                match std::future::poll_fn(|cx| connection.poll_message(cx)).await {
                    Some(Ok(AsyncMessage::Notification(notification))) => {
                        if tx.send(notification.payload().to_owned()).is_err() {
                            break;
                        }
                    }
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        eprintln!("software-worker: the notification connection dropped: {e}");
                        break;
                    }
                    None => break,
                }
            }
        });
        if let Err(e) = client.batch_execute(&format!("LISTEN {CHANNEL}")).await {
            eprintln!("software-worker: LISTEN failed: {e}");
            return None;
        }
        Some(Listener {
            _client: client,
            rx,
        })
    }

    /// After a restart: a change job this builder was compiling is marked
    /// interrupted (nothing changed, the cache keeps the finished parts). A
    /// job already handed to the installer is left alone.
    async fn recover(&self) -> Result<()> {
        let client = self.db.get().await?;
        let rows = client
            .query(
                &format!("SELECT tenant_id, {JOB_COLUMNS} FROM base_software_jobs j WHERE state='running' AND kind='change' AND worker LIKE 'builder@%' AND NOT EXISTS (SELECT 1 FROM base_software_steps s WHERE s.job_id=j.id AND s.key='build' AND s.state='done') ORDER BY id"),
                &[],
            )
            .await?;
        for row in &rows {
            let tenant: i64 = row.get(0);
            let job = Job::from_row_offset(row, 1);
            let message = "The builder was restarted before the build finished. Nothing changed. Start the change again; the parts already compiled are kept, so it is quicker.";
            client
                .execute(
                    "UPDATE base_software_steps SET state='skipped', finished_at=CASE WHEN state='running' THEN now() ELSE finished_at END, detail=CASE WHEN state='running' THEN $3::jsonb ELSE detail END WHERE tenant_id=$1 AND job_id=$2 AND state IN ('pending','running')",
                    &[&tenant, &job.id, &json!({"message": "Interrupted by a restart"})],
                )
                .await?;
            self.finish(
                tenant,
                &job,
                "interrupted",
                json!({"message": message, "release": null}),
                ACTION_INTERRUPTED,
                json!({"job": job.id}),
                Notice::new(format!("Software job {} was interrupted", job.id))
                    .kind("software.job")
                    .body(message),
            )
            .await?;
            println!(
                "software-worker: job {} marked interrupted after a restart",
                job.id
            );
        }
        Ok(())
    }

    /// Everything waiting: cancels asked after a hand-off, then the queued
    /// job, if it is a change (an install or undo is the installer's).
    async fn sweep(&self, stop: &watch::Receiver<bool>) -> Result<()> {
        self.honour_cancels_after_hand_off().await?;
        if *stop.borrow() {
            return Ok(());
        }
        let (tenant, job) = {
            let client = self.db.get().await?;
            let row = client
                .query_opt(
                    &format!("SELECT tenant_id, {JOB_COLUMNS} FROM base_software_jobs WHERE state='queued' ORDER BY id LIMIT 1"),
                    &[],
                )
                .await?;
            match row {
                Some(row) => (row.get::<_, i64>(0), Job::from_row_offset(&row, 1)),
                None => return Ok(()),
            }
        };
        if job.kind != "change" {
            return Ok(());
        }
        self.handle_change(tenant, job, stop.clone()).await
    }

    async fn handle_change(
        &self,
        tenant: i64,
        job: Job,
        mut stop: watch::Receiver<bool>,
    ) -> Result<()> {
        let instance = self.instance().await?;
        if !instance.role.builds() {
            return self.refuse(tenant, &job, &instance).await;
        }
        let plan = match Plan::from_request(&job.request) {
            Ok(mut plan) => {
                let client = self.db.get().await?;
                plan.plugins = crate::jobs::planned_plugins(&**client, tenant, &job.review).await?;
                plan
            }
            Err(why) => {
                let client = self.db.get().await?;
                if self.claim(&**client, tenant, job.id).await? == 0 {
                    return Ok(());
                }
                return self
                    .fail(
                        tenant,
                        &job,
                        &instance,
                        "check",
                        &format!("The request cannot be built: {why}."),
                        None,
                    )
                    .await;
            }
        };
        let jobs = {
            let client = self.db.get().await?;
            if self.claim(&**client, tenant, job.id).await? == 0 {
                return Ok(());
            }
            self.settings
                .get_global(&**client, BUILD_JOBS_KEY)
                .await?
                .as_u64()
                .unwrap_or(2) as u32
        };
        println!(
            "software-worker: job {} claimed: {}",
            job.id,
            plan.describe()
        );
        // Progress rows: the builder sends details, a task writes them to the
        // running step; another task watches for a cancel.
        let (progress_tx, mut progress_rx) = mpsc::channel::<Value>(64);
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let current = Arc::new(AtomicI32::new(0));
        let writer = {
            let db = self.db.clone();
            let current = current.clone();
            let id = job.id;
            tokio::spawn(async move {
                while let Some(detail) = progress_rx.recv().await {
                    let seq = current.load(Ordering::SeqCst);
                    if let Ok(client) = db.get().await {
                        let _ = client
                            .execute(
                                "UPDATE base_software_steps SET detail=$4 WHERE tenant_id=$1 AND job_id=$2 AND seq=$3 AND state='running'",
                                &[&tenant, &id, &seq, &detail],
                            )
                            .await;
                    }
                }
            })
        };
        // The watcher stops the build for two reasons: the owner's cancel
        // (polled from the job row) or the worker being stopped.
        let stopping = Arc::new(AtomicBool::new(false));
        let watcher = {
            let db = self.db.clone();
            let id = job.id;
            let stopping = stopping.clone();
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        () = tokio::time::sleep(CANCEL_POLL) => {}
                        _ = stop.changed() => {
                            stopping.store(true, Ordering::SeqCst);
                            let _ = cancel_tx.send(true);
                            break;
                        }
                    }
                    let Ok(client) = db.get().await else {
                        continue;
                    };
                    let asked: Option<bool> = client
                        .query_opt(
                            "SELECT cancel_requested_at IS NOT NULL FROM base_software_jobs WHERE tenant_id=$1 AND id=$2",
                            &[&tenant, &id],
                        )
                        .await
                        .ok()
                        .flatten()
                        .map(|row| row.get(0));
                    if asked.unwrap_or(false) {
                        let _ = cancel_tx.send(true);
                        break;
                    }
                }
            })
        };
        let mut build = Build {
            paths: &self.paths,
            toolchain: &self.toolchain,
            plan: &plan,
            job: job.id,
            jobs,
            progress: progress_tx,
            cancel: cancel_rx,
        };
        let mut outcome: Result<Option<Built>> = Ok(None);
        for key in BUILDER_STEPS {
            let seq = self.start_step(tenant, job.id, key).await?;
            current.store(seq, Ordering::SeqCst);
            let result = match key {
                "check" => build.check().await.map(|detail| (detail, None)),
                "fetch" => build.fetch().await.map(|detail| (detail, None)),
                _ => build
                    .build()
                    .await
                    .map(|built| (built_detail(&built), Some(built))),
            };
            match result {
                Ok((detail, built)) => {
                    self.end_step(tenant, job.id, key, "done", &detail).await?;
                    if let Some(built) = built {
                        outcome = Ok(Some(built));
                    }
                }
                Err(Failure::Cancelled) if stopping.load(Ordering::SeqCst) => {
                    self.end_step(
                        tenant,
                        job.id,
                        key,
                        "skipped",
                        &json!({"message": "Interrupted: the worker was stopped"}),
                    )
                    .await?;
                    outcome = self.interrupt(tenant, &job).await.map(|()| None);
                    break;
                }
                Err(Failure::Cancelled) => {
                    self.end_step(
                        tenant,
                        job.id,
                        key,
                        "skipped",
                        &json!({"message": "Stopped: the owner cancelled the job"}),
                    )
                    .await?;
                    outcome = self.cancel(tenant, &job, &instance).await.map(|()| None);
                    break;
                }
                Err(Failure::Failed { message, details }) => {
                    self.end_step(
                        tenant,
                        job.id,
                        key,
                        "failed",
                        &json!({"message": message, "details": details}),
                    )
                    .await?;
                    outcome = self
                        .fail(tenant, &job, &instance, key, &message, details.as_deref())
                        .await
                        .map(|()| None);
                    break;
                }
            }
        }
        drop(build);
        writer.abort();
        watcher.abort();
        if let Some(built) = outcome? {
            self.hand_off(tenant, &job, &instance, &plan, &built)
                .await?;
        }
        Ok(())
    }

    async fn claim(&self, client: &impl GenericClient, tenant: i64, id: i64) -> Result<u64> {
        Ok(client
            .execute(
                "UPDATE base_software_jobs SET state='running', worker=$3, started_at=now() WHERE tenant_id=$1 AND id=$2 AND state='queued'",
                &[&tenant, &id, &self.name],
            )
            .await?)
    }
    async fn start_step(&self, tenant: i64, job: i64, key: &str) -> Result<i32> {
        let client = self.db.get().await?;
        let row = client
            .query_one(
                "UPDATE base_software_steps SET state='running', started_at=now(), detail=NULL WHERE tenant_id=$1 AND job_id=$2 AND key=$3 RETURNING seq",
                &[&tenant, &job, &key],
            )
            .await?;
        Ok(row.get(0))
    }
    async fn end_step(
        &self,
        tenant: i64,
        job: i64,
        key: &str,
        state: &str,
        detail: &Value,
    ) -> Result<()> {
        let client = self.db.get().await?;
        client
            .execute(
                "UPDATE base_software_steps SET state=$4, finished_at=now(), detail=$5 WHERE tenant_id=$1 AND job_id=$2 AND key=$3",
                &[&tenant, &job, &key, &state, detail],
            )
            .await?;
        Ok(())
    }
    async fn skip_rest(&self, client: &impl GenericClient, tenant: i64, job: i64) -> Result<()> {
        client
            .execute(
                "UPDATE base_software_steps SET state='skipped' WHERE tenant_id=$1 AND job_id=$2 AND state='pending'",
                &[&tenant, &job],
            )
            .await?;
        Ok(())
    }
    fn running_text(&self) -> String {
        let domains: Vec<String> = self
            .composition
            .containers
            .iter()
            .filter(|c| c.layer == Layer::Domains)
            .map(|c| format!("{} {}", c.id, c.version.unwrap_or("unpinned")))
            .collect();
        format!(
            "Bilibi {} with {}",
            self.composition.version,
            if domains.is_empty() {
                "no Domains".to_owned()
            } else {
                domains.join(", ")
            }
        )
    }

    /// A change job on an instance that never builds: refused, with the
    /// reason, before anything is claimed or touched.
    async fn refuse(&self, tenant: i64, job: &Job, instance: &Instance) -> Result<()> {
        let message = refusal(instance);
        let client = self.db.get().await?;
        if self.claim(&**client, tenant, job.id).await? == 0 {
            return Ok(());
        }
        self.skip_rest(&**client, tenant, job.id).await?;
        drop(client);
        self.finish(
            tenant,
            job,
            "failed",
            json!({"message": message, "release": null, "refused": true}),
            ACTION_REFUSED,
            json!({"job": job.id, "role": instance.role.env_value()}),
            Notice::new(format!("Software job {} refused", job.id))
                .kind("software.job")
                .body(&message),
        )
        .await?;
        println!("software-worker: job {} refused: {message}", job.id);
        Ok(())
    }
    async fn fail(
        &self,
        tenant: i64,
        job: &Job,
        instance: &Instance,
        step: &str,
        reason: &str,
        details: Option<&str>,
    ) -> Result<()> {
        let message = format!(
            "Nothing changed. {} still runs {}. {reason}",
            instance.label,
            self.running_text()
        );
        {
            let client = self.db.get().await?;
            self.skip_rest(&**client, tenant, job.id).await?;
        }
        self.finish(
            tenant,
            job,
            "failed",
            json!({"message": message, "release": null, "failed_step": step, "reason": reason, "details": details}),
            ACTION_FAILED,
            json!({"job": job.id, "step": step, "reason": reason}),
            Notice::new(format!("Software job {} failed", job.id))
                .kind("software.job")
                .body(&message),
        )
        .await?;
        println!("software-worker: job {} failed at {step}: {reason}", job.id);
        Ok(())
    }
    /// The worker was stopped (a restart, `systemctl stop`) while it was
    /// building: the same outcome `recover` records after a crash, written
    /// now, so the job never sits running without a worker.
    async fn interrupt(&self, tenant: i64, job: &Job) -> Result<()> {
        let message = "The builder was stopped before the build finished. Nothing changed. Start the change again; the parts already compiled are kept, so it is quicker.";
        {
            let client = self.db.get().await?;
            self.skip_rest(&**client, tenant, job.id).await?;
        }
        self.finish(
            tenant,
            job,
            "interrupted",
            json!({"message": message, "release": null}),
            ACTION_INTERRUPTED,
            json!({"job": job.id}),
            Notice::new(format!("Software job {} was interrupted", job.id))
                .kind("software.job")
                .body(message),
        )
        .await?;
        println!("software-worker: job {} interrupted by a stop", job.id);
        Ok(())
    }
    async fn cancel(&self, tenant: i64, job: &Job, instance: &Instance) -> Result<()> {
        let message = format!(
            "Stopped before anything changed. {} still runs {}.",
            instance.label,
            self.running_text()
        );
        {
            let client = self.db.get().await?;
            self.skip_rest(&**client, tenant, job.id).await?;
        }
        self.finish(
            tenant,
            job,
            "cancelled",
            json!({"message": message, "release": null}),
            ACTION_CANCELLED,
            json!({"job": job.id}),
            Notice::new(format!("Software job {} stopped", job.id))
                .kind("software.job")
                .body(&message),
        )
        .await?;
        println!("software-worker: job {} cancelled", job.id);
        Ok(())
    }
    /// The build is done and copied; the job stays running for the installer,
    /// which takes a change job whose `build` step is done from its first
    /// pending step, reading the release number from that step's detail.
    async fn hand_off(
        &self,
        tenant: i64,
        job: &Job,
        instance: &Instance,
        plan: &Plan,
        built: &Built,
    ) -> Result<()> {
        let body = format!(
            "Release {} holds {}, built in {}. Next: it is put into service on {} (back up, update the database, switch, check).",
            built.release,
            plan.describe(),
            duration(built.seconds),
            instance.label
        );
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(
                &mut client,
                Context::system(tenant, PRINCIPAL, ACTION_COMPLETED),
            )
            .await?;
        let mut who = Attribution::system(Some(PRINCIPAL))?;
        who.tenant = Some(tenant);
        record_action(
            tx.sql(),
            &who,
            None,
            ACTION_COMPLETED,
            json!({"job": job.id, "release": built.release, "path": built.path.display().to_string(), "seconds": built.seconds, "compiled": built.compiled, "fresh": built.fresh}),
            true,
        )
        .await?;
        self.notify(
            &mut tx,
            tenant,
            job.requested_by,
            Notice::new(format!(
                "Release {} is built on {}",
                built.release, instance.label
            ))
            .kind("software.job")
            .body(&body),
        )
        .await?;
        tx.sql()
            .execute("SELECT pg_notify($1, $2)", &[&CHANNEL, &job.id.to_string()])
            .await?;
        tx.commit().await?;
        println!(
            "software-worker: job {} built release {} at {} in {}",
            job.id,
            built.release,
            built.path.display(),
            duration(built.seconds)
        );
        Ok(())
    }
    /// A cancel asked after the hand-off, before the installer reached the
    /// pause: the job is cancelled here, under the same row lock the
    /// installer takes before its pausing step, so the two cannot cross.
    async fn honour_cancels_after_hand_off(&self) -> Result<()> {
        let candidates = {
            let client = self.db.get().await?;
            client
                .query(
                    "SELECT tenant_id, id FROM base_software_jobs j WHERE state='running' AND kind='change' AND cancel_requested_at IS NOT NULL AND EXISTS (SELECT 1 FROM base_software_steps s WHERE s.job_id=j.id AND s.key='build' AND s.state='done') ORDER BY id",
                    &[],
                )
                .await?
        };
        for row in &candidates {
            let (tenant, id): (i64, i64) = (row.get(0), row.get(1));
            let mut client = self.db.get().await?;
            let mut tx = self
                .mutations
                .begin(
                    &mut client,
                    Context::system(tenant, PRINCIPAL, ACTION_CANCELLED),
                )
                .await?;
            let Some(row) = tx
                .sql()
                .query_opt(
                    &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs j WHERE j.tenant_id=$1 AND j.id=$2 AND j.state='running' AND NOT EXISTS (SELECT 1 FROM base_software_steps s WHERE s.tenant_id=j.tenant_id AND s.job_id=j.id AND s.pauses AND s.state<>'pending') FOR UPDATE OF j"),
                    &[&tenant, &id],
                )
                .await?
            else {
                continue;
            };
            let job = Job::from_row_offset(&row, 0);
            let instance = Instance::resolve(&self.settings, tx.sql()).await?;
            let message = format!(
                "Stopped before the pause. {} still runs {}; the built release stays in its folder, unused.",
                instance.label,
                self.running_text()
            );
            self.skip_rest(tx.sql(), tenant, id).await?;
            let result = json!({"message": message, "release": null});
            let (old, new) = self
                .set_state(&mut tx, tenant, job.id, "cancelled", &result)
                .await?;
            tx.mutate(move |_| {
                Box::pin(async move {
                    Ok((
                        (),
                        vec![Change {
                            kind: RECORD.into(),
                            id,
                            old,
                            new,
                        }],
                    ))
                })
            })
            .await?;
            let mut who = Attribution::system(Some(PRINCIPAL))?;
            who.tenant = Some(tenant);
            record_action(
                tx.sql(),
                &who,
                None,
                ACTION_CANCELLED,
                json!({"job": id, "after_build": true}),
                true,
            )
            .await?;
            self.notify(
                &mut tx,
                tenant,
                job.requested_by,
                Notice::new(format!("Software job {id} stopped"))
                    .kind("software.job")
                    .body(&message),
            )
            .await?;
            tx.commit().await?;
            println!("software-worker: job {id} cancelled after its build, before the pause");
        }
        Ok(())
    }

    /// The final state and result, as one audited record of the job and one
    /// retained action by the builder, with the notification to the person
    /// who asked, all in one transaction.
    #[allow(clippy::too_many_arguments)]
    async fn finish(
        &self,
        tenant: i64,
        job: &Job,
        state: &str,
        result: Value,
        action: &str,
        payload: Value,
        notice: Notice,
    ) -> Result<()> {
        let mut client = self.db.get().await?;
        let mut tx = self
            .mutations
            .begin(&mut client, Context::system(tenant, PRINCIPAL, action))
            .await?;
        let (old, new) = self
            .set_state(&mut tx, tenant, job.id, state, &result)
            .await?;
        let id = job.id;
        tx.mutate(move |_| {
            Box::pin(async move {
                Ok((
                    (),
                    vec![Change {
                        kind: RECORD.into(),
                        id,
                        old,
                        new,
                    }],
                ))
            })
        })
        .await?;
        let mut who = Attribution::system(Some(PRINCIPAL))?;
        who.tenant = Some(tenant);
        record_action(tx.sql(), &who, None, action, payload, true).await?;
        self.notify(&mut tx, tenant, job.requested_by, notice)
            .await?;
        tx.commit().await
    }
    async fn set_state(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        tenant: i64,
        id: i64,
        state: &str,
        result: &Value,
    ) -> Result<(Value, Value)> {
        let before = tx
            .sql()
            .query_one(
                &format!("SELECT {JOB_COLUMNS} FROM base_software_jobs WHERE tenant_id=$1 AND id=$2 FOR UPDATE"),
                &[&tenant, &id],
            )
            .await?;
        let row = tx
            .sql()
            .query_one(
                &format!("UPDATE base_software_jobs SET state=$3, finished_at=now(), settled_at=now(), result=$4 WHERE tenant_id=$1 AND id=$2 RETURNING {JOB_COLUMNS}"),
                &[&tenant, &id, &state, result],
            )
            .await?;
        Ok((
            Job::from_row_offset(&before, 0).json(),
            Job::from_row_offset(&row, 0).json(),
        ))
    }
    /// Tells the person who asked. Their account is re-proved live for the
    /// notification's scope; an account that is gone gets none, and the
    /// outcome is still recorded.
    async fn notify(
        &self,
        tx: &mut bilibi_database::MutationTransaction<'_>,
        tenant: i64,
        user: i64,
        notice: Notice,
    ) -> Result<()> {
        match self
            .authentication
            .authenticate(
                tx.sql(),
                tenant,
                user,
                format!("{PRINCIPAL}-{tenant}-{user}"),
            )
            .await
        {
            Ok(scope) => {
                self.notifications
                    .notify(tx, &scope, &[user], &notice)
                    .await
            }
            Err(_) => {
                eprintln!("software-worker: account {user} of tenant {tenant} cannot be notified (not active)");
                Ok(())
            }
        }
    }
}

struct Listener {
    /// Dropping the client closes the connection, so it lives with the receiver.
    _client: tokio_postgres::Client,
    rx: mpsc::UnboundedReceiver<String>,
}

/// Why a job is refused on an instance that does not build.
fn refusal(instance: &Instance) -> String {
    format!(
        "This instance is {} ({}). It installs releases that another instance tested and never builds; only a Dev instance (BILIBI_ENV=development) builds. Build on Dev, then install the tested release here.",
        instance.label,
        match instance.role {
            Role::Dev => "development",
            Role::Staging => "staging",
            Role::Production => "production",
        }
    )
}
fn built_detail(built: &Built) -> Value {
    json!({
        "message": format!("Built release {} in {}{}{}", built.release, duration(built.seconds),
            if built.fresh > 0 { format!(" ({} of {} parts from the cache)", built.fresh, built.compiled) } else { format!(" ({} parts)", built.compiled) },
            if built.status.is_none() { "; this release is too old to report its own make-up" } else { "" }),
        "release": built.release, "path": built.path.display().to_string(), "seconds": built.seconds,
        "compiled": built.compiled, "fresh": built.fresh, "status": built.status,
        "platform": built.platform, "domains": built.domains,
    })
}
fn duration(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds} s"),
        _ => format!("{} min {} s", seconds / 60, seconds % 60),
    }
}
