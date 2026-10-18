//! The steps of one job, in the order `base/software` created them, each
//! written back as it runs so the Software page can show it. Every step can
//! be run again from where it was: when the installer is stopped mid-way
//! (a restart), the same flow resumes from the first step not yet done.
use crate::{
    backup::{self, PgEnv},
    jobs::{self, Job, Step},
    layout::{self, human, Layout},
    service::Service,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf, process::Command, time::Duration};
use tokio_postgres::Client;

/// How long a release gets to answer after a start.
const HEALTH_LIMIT: Duration = Duration::from_secs(60);
/// Room kept free beyond the program and the backup.
const DISK_MARGIN: u64 = 64 * 1024 * 1024;
/// A migration that runs longer than this is cut off and rolled back.
const MIGRATE_LIMIT_SECONDS: &str = "1800";
/// How many releases besides the current and previous one are kept.
const KEEP_RELEASES: usize = 3;
const KEEP_BACKUPS: usize = 3;

type Db<T> = Result<T, tokio_postgres::Error>;

/// One job being carried out.
pub struct Run<'a> {
    client: &'a mut Client,
    layout: &'a Layout,
    service: &'a Service,
    pg: &'a PgEnv,
    job: Job,
    steps: Vec<Step>,
}
impl<'a> Run<'a> {
    pub async fn new(
        client: &'a mut Client,
        layout: &'a Layout,
        service: &'a Service,
        pg: &'a PgEnv,
        job: Job,
    ) -> Db<Run<'a>> {
        let steps = jobs::steps(&*client, &job).await?;
        Ok(Self {
            client,
            layout,
            service,
            pg,
            job,
            steps,
        })
    }
    pub fn job(&self) -> &Job {
        &self.job
    }
    fn log(&self, text: &str) {
        println!("job {}: {text}", self.job.id);
    }
    fn step(&self, key: &str) -> Option<&Step> {
        self.steps.iter().find(|step| step.key == key)
    }
    fn is_done(&self, key: &str) -> bool {
        self.step(key).is_some_and(|step| step.state == "done")
    }
    fn detail(&self, key: &str) -> Value {
        self.step(key)
            .map_or(Value::Null, |step| step.detail.clone())
    }
    async fn set(&mut self, key: &str, state: &str, detail: Value) -> Db<()> {
        jobs::step_set(&*self.client, &self.job, key, state, &detail).await?;
        if let Some(step) = self.steps.iter_mut().find(|step| step.key == key) {
            step.state = state.into();
            step.detail = detail;
        }
        Ok(())
    }
    /// Marks a step running. A step found running already was cut off by a
    /// restart; its detail says so and the step runs again.
    async fn start(&mut self, key: &str, message: &str) -> Db<()> {
        self.start_with(key, json!({"message": message})).await
    }
    async fn start_with(&mut self, key: &str, mut detail: Value) -> Db<()> {
        if self.step(key).is_some_and(|step| step.state == "running") {
            detail["resumed"] = json!(true);
            detail["message"] = json!(format!(
                "{} (the installer restarted during this step and continued)",
                detail["message"].as_str().unwrap_or("Running")
            ));
        }
        self.log(detail["message"].as_str().unwrap_or(key));
        self.set(key, "running", detail).await
    }
    /// The detail the pausing step starts with; a pause cut off by a restart
    /// is marked resumed like any other step.
    fn pause_detail(&self, message: String) -> Value {
        let mut detail = json!({"message": message});
        if self
            .step("backup")
            .is_some_and(|step| step.state == "running")
        {
            detail["resumed"] = json!(true);
            detail["message"] = json!(format!(
                "{message} (the installer restarted during this step and continued)"
            ));
        }
        detail
    }
    async fn done(&mut self, key: &str, detail: Value) -> Db<()> {
        self.log(detail["message"].as_str().unwrap_or(key));
        self.set(key, "done", detail).await
    }
    async fn fail(&mut self, key: &str, reason: &str) -> Db<()> {
        self.log(&format!("{key} failed: {reason}"));
        self.set(key, "failed", json!({"message": reason})).await
    }
    async fn finish(&mut self, state: &str, result: Value) -> Db<()> {
        jobs::skip_pending(&*self.client, &self.job).await?;
        jobs::finish(&*self.client, &self.job, state, &result).await?;
        self.log(&format!(
            "{state}: {}",
            result["message"].as_str().unwrap_or("")
        ));
        Ok(())
    }
    /// A step failed before anything changed on the instance.
    async fn nothing_changed(&mut self, key: &str, reason: &str) -> Db<()> {
        self.fail(key, reason).await?;
        let current = self.layout.current();
        self.finish(
            "failed",
            json!({
                "release": current, "previous_release": current, "migrated": false,
                "message": format!("Nothing changed. {reason}"),
            }),
        )
        .await
    }

    // ------------------------------------------------------------ install
    pub async fn install(&mut self) -> Db<()> {
        if self.steps.iter().any(|step| step.state == "failed") {
            return self.finish_after_failed_step().await;
        }
        let fetched = if self.is_done("fetch") {
            self.detail("fetch")
        } else {
            self.start("fetch", "Copying the release and checking it")
                .await?;
            match self.fetch().await {
                Ok(detail) => {
                    self.done("fetch", detail.clone()).await?;
                    detail
                }
                Err(reason) => return self.nothing_changed("fetch", &reason).await,
            }
        };
        let Some(release) = fetched["release"].as_u64() else {
            return self
                .nothing_changed("fetch", "the copied release was not recorded")
                .await;
        };
        // The release in service when the copy was made, kept in the step so a
        // resume after the switch still knows what to go back to.
        let Some(previous) = fetched["previous"].as_u64() else {
            return self
                .nothing_changed(
                    "fetch",
                    "the release in service before the copy was not recorded",
                )
                .await;
        };
        if !self.is_done("backup") {
            if !self.wait_for_go().await? {
                return Ok(());
            }
            let detail = self.pause_detail(format!(
                "{} pauses from here: the service stops and the database is backed up.",
                self.layout.name
            ));
            if !jobs::begin_pause(&mut *self.client, &self.job, "backup", &detail).await? {
                self.log("stopped before the pause, as asked");
                return Ok(());
            }
            match self.backup().await {
                Ok(detail) => self.done("backup", detail).await?,
                Err(reason) => {
                    self.fail("backup", &reason).await?;
                    let _ = self.service.start();
                    return self
                        .finish(
                            "failed",
                            json!({"release": previous, "previous_release": previous, "migrated": false,
                                "message": format!("Nothing changed: the backup failed, so the service was started again. {reason}")}),
                        )
                        .await;
                }
            }
        }
        let backup = self.detail("backup");
        if !self.is_done("migrate") {
            let before = match self.detail("migrate")["ledger_before"].as_i64() {
                Some(before) => before,
                None => jobs::ledger_count(&*self.client).await?,
            };
            self.start_with(
                "migrate",
                json!({"message": "Updating the database", "ledger_before": before}),
            )
            .await?;
            if let Err(reason) = self.service.stop() {
                self.log(&reason);
            }
            match self.migrate(release, before).await {
                Ok(detail) => self.done("migrate", detail).await?,
                Err(reason) => {
                    self.fail("migrate", &reason).await?;
                    let healthy = match self.service.start() {
                        Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.is_ok(),
                        Err(_) => false,
                    };
                    let message = if healthy {
                        format!("Nothing changed: the database update failed as one piece, so release {previous} is back in service. {reason}")
                    } else {
                        format!("The database update failed as one piece, but release {previous} did not answer when started again; check the service. {reason}")
                    };
                    return self
                        .finish("failed", json!({"release": previous, "previous_release": previous, "migrated": false,
                            "backup": backup["backup"], "backup_at": backup["backup_at"], "message": message}))
                        .await;
                }
            }
        }
        let migrated = self.detail("migrate")["migrated"]
            .as_bool()
            .unwrap_or(false);
        if !self.is_done("switch") {
            self.start("switch", &format!("Switching to release {release}"))
                .await?;
            if let Err(reason) = self.switch_to(release) {
                self.fail("switch", &reason).await?;
                return self
                    .switch_back(release, previous, migrated, &backup, &reason)
                    .await;
            }
            self.done("switch", json!({"message": format!("current points at release {release}; the service is starting")}))
                .await?;
        }
        if !self.is_done("verify") {
            self.start("verify", "Checking that the new release answers")
                .await?;
            match self.service.wait_healthy(HEALTH_LIMIT).await {
                Ok(took) => {
                    self.done("verify", json!({"message": format!("Release {release} answered after {} s", took.as_secs().max(1))}))
                        .await?
                }
                Err(reason) => {
                    self.fail("verify", &reason).await?;
                    return self.switch_back(release, previous, migrated, &backup, &reason).await;
                }
            }
        }
        let database = if migrated {
            " The database was updated."
        } else {
            " The database kept its shape."
        };
        self.finish(
            "done",
            json!({
                "release": release, "previous_release": previous, "migrated": migrated,
                "backup": backup["backup"], "backup_at": backup["backup_at"],
                "source": fetched["source"], "source_release": fetched["source_release"],
                "sha256": fetched["sha256"], "program": fetched["program"],
                "message": format!("Release {release} is in service, installed from {} release {}.{database}",
                    fetched["source"].as_str().unwrap_or("the source"), fetched["source_release"]),
            }),
        )
        .await?;
        self.prune(&[release, previous]);
        Ok(())
    }

    /// The release the owner asked for, copied from the sibling instance's
    /// folder into the next number here, checked and tried.
    async fn fetch(&mut self) -> Result<Value, String> {
        if let Some(stale) = self.detail("fetch")["release"].as_u64() {
            // A copy cut off by a restart: start it over.
            if Some(stale) != self.layout.current() {
                let _ = fs::remove_dir_all(self.layout.release(stale));
            }
        }
        let request = self.job.request.clone();
        let wanted = request["release"]
            .as_u64()
            .ok_or("the request names no release")?;
        let source_name = match request["source"]["kind"].as_str() {
            Some("same_machine") => request["source"]["instance"]
                .as_str()
                .ok_or("the request names no source instance")?
                .to_owned(),
            Some("https") => {
                return Err(format!(
                    "Downloading a release from {} is not possible yet: a release reaches this machine only as a copy between instance folders. Set \"Install releases tested on\" to an instance on this machine, or copy the release folder here.",
                    request["source"]["address"].as_str().unwrap_or("another machine")
                ))
            }
            _ => return Err("the request names no source".into()),
        };
        let source = self.layout.sibling(&source_name).map_err(|reason| {
            format!("{source_name} is not an instance on this machine: {reason}")
        })?;
        let from = source.release(wanted);
        if !from.join("bilibi-web").is_file() {
            let have = source.release_numbers();
            return Err(format!(
                "{source_name} has no release {wanted}. It has: {}.",
                if have.is_empty() {
                    "none".to_owned()
                } else {
                    have.iter()
                        .map(u64::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ));
        }
        let sha = layout::verify_checksum(&from)?;
        match source.current() {
            Some(n) if n == wanted => {}
            Some(n) => {
                return Err(format!(
                    "{source_name} runs release {n} now, not {wanted}. Install the release it runs."
                ))
            }
            None => {
                return Err(format!(
                "{source_name} has no release in service, so it cannot vouch for release {wanted}."
            ))
            }
        }
        let source_service = Service::of(&source)?;
        if !source_service.answers().await {
            return Err(format!(
                "{source_name} is not answering (its service is {}), so it cannot vouch for release {wanted}.",
                source_service.state()
            ));
        }
        let current = self
            .layout
            .current()
            .ok_or("this instance has no release in service; put the first one in with scripts/install-server.sh release")?;
        if layout::verify_checksum(&self.layout.release(current))
            .ok()
            .as_deref()
            == Some(sha.as_str())
        {
            return Err(format!(
                "This instance already runs the same program as {source_name} release {wanted} (release {current} here). There is nothing to install."
            ));
        }
        let program_bytes = fs::metadata(from.join("bilibi-web"))
            .map(|meta| meta.len())
            .unwrap_or(0);
        let database_bytes = jobs::database_size(&*self.client)
            .await
            .map_err(|error| jobs::short(&error))? as u64;
        let need = program_bytes + database_bytes + DISK_MARGIN;
        if let Some(free) = layout::free_bytes(&self.layout.releases()) {
            if free < need {
                return Err(format!(
                    "Not enough disk: {} free, about {} needed for the program and a database backup.",
                    human(free),
                    human(need)
                ));
            }
        }
        let n = self.layout.next_release();
        let dst = self.layout.release(n);
        jobs::step_set(
            &*self.client,
            &self.job,
            "fetch",
            "pending",
            &json!({"message": format!("Copying into releases/{n}"), "release": n}),
        )
        .await
        .map_err(|error| jobs::short(&error))?;
        let bytes = layout::copy_release(&from, &dst)
            .map_err(|error| format!("could not copy into {}: {error}", dst.display()))?;
        if layout::verify_checksum(&dst)? != sha {
            let _ = fs::remove_dir_all(&dst);
            return Err(format!(
                "the copy in {} does not match the source",
                dst.display()
            ));
        }
        let program = program_status(&dst)?;
        Ok(json!({
            "message": format!("Copied {source_name} release {wanted} as release {n} here: {program}"),
            "release": n, "previous": current, "source_release": wanted, "source": source_name,
            "sha256": sha, "bytes": bytes, "program": program,
        }))
    }
    /// With `switch: ask`, the instance keeps running until the owner says
    /// go (the job's `switch` turns `auto`) or stops the job.
    async fn wait_for_go(&mut self) -> Db<bool> {
        let mut told = false;
        loop {
            let (cancelled, switch) = jobs::watch(&*self.client, &self.job).await?;
            if cancelled {
                self.finish(
                    "cancelled",
                    json!({"message": "Stopped before the pause, as asked. Nothing changed on this instance."}),
                )
                .await?;
                return Ok(false);
            }
            if switch == "auto" {
                return Ok(true);
            }
            if !told {
                self.set("backup", "pending", json!({"message": "Ready. Waiting for the owner to say when to switch; the instance keeps running until then."}))
                    .await?;
                self.log("waiting for the owner to say when to switch");
                told = true;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    /// The pause begins: stop the service, then dump the database.
    async fn backup(&mut self) -> Result<Value, String> {
        self.service.stop()?;
        let at = jobs::now(&*self.client)
            .await
            .map_err(|error| jobs::short(&error))?;
        let file = self.backup_file();
        let bytes = backup::dump(self.pg, &file)?;
        Ok(json!({
            "message": format!("{} is paused. Backed up the database ({}).", self.layout.name, human(bytes)),
            "backup": file.display().to_string(), "backup_at": at, "bytes": bytes,
        }))
    }
    fn backup_file(&self) -> PathBuf {
        self.layout
            .backups()
            .join(format!("job-{}", self.job.id))
            .join(format!("{}.dump", self.pg.dbname()))
    }
    /// The new program's `migrate` (one transaction, all or nothing) and its
    /// production seeds. `migrated` says whether the shape changed.
    async fn migrate(&mut self, release: u64, before: i64) -> Result<Value, String> {
        let program = self.layout.release(release).join("bilibi-web");
        run_program(&program, "migrate", MIGRATE_LIMIT_SECONDS)?;
        run_program(&program, "seed-production", "600")?;
        let after = jobs::ledger_count(&*self.client)
            .await
            .map_err(|error| jobs::short(&error))?;
        let applied = (after - before).max(0);
        Ok(json!({
            "message": if applied == 0 { "The database already had this release's shape; nothing to apply.".to_owned() }
                else { format!("Applied {applied} migration{} as one piece.", if applied == 1 { "" } else { "s" }) },
            "ledger_before": before, "applied": applied, "migrated": applied > 0,
        }))
    }
    fn switch_to(&self, release: u64) -> Result<(), String> {
        if !self.layout.release(release).join("bilibi-web").is_file() {
            return Err(format!(
                "release {release} is missing from {}",
                self.layout.releases().display()
            ));
        }
        if let Err(reason) = self.service.stop() {
            self.log(&reason);
        }
        self.layout
            .point_current(release)
            .map_err(|error| format!("could not point current at release {release}: {error}"))?;
        self.service.start()
    }
    /// The new release did not work: go back to the previous one, restoring
    /// the backup first when the database changed shape. Nothing entered is
    /// lost, because the instance was paused the whole time.
    async fn switch_back(
        &mut self,
        release: u64,
        previous: u64,
        migrated: bool,
        backup: &Value,
        reason: &str,
    ) -> Db<()> {
        self.log(&format!("switching back to release {previous}"));
        let _ = self.service.stop();
        let mut notes = Vec::new();
        let mut restored = false;
        if migrated {
            match backup["backup"].as_str() {
                Some(file) => match backup::restore(&*self.client, self.pg, &PathBuf::from(file)).await {
                    Ok(done) => {
                        restored = true;
                        notes.push("The database backup taken before the update was restored, so nothing entered is lost.".to_owned());
                        notes.extend(done.problems);
                    }
                    Err(why) => {
                        notes.push(format!("The database could not be put back: {why}. The service was left stopped; restore {file} by hand before starting it."));
                        return self
                            .finish("failed", json!({"release": release, "previous_release": previous, "migrated": migrated,
                                "backup": backup["backup"], "backup_at": backup["backup_at"], "switched_back": false, "restored": false,
                                "message": format!("Release {release} failed its check ({reason}). {}", notes.join(" "))}))
                            .await;
                    }
                },
                None => notes.push("The database changed shape but no backup was recorded, so it was left as it is.".to_owned()),
            }
        }
        let back = match self.switch_to(previous) {
            Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.map(|_| ()),
            Err(why) => Err(why),
        };
        match &back {
            Ok(()) => notes.insert(
                0,
                format!("Switched back to release {previous}, which answers."),
            ),
            Err(why) => notes.insert(
                0,
                format!(
                    "Switched back to release {previous}, but it does not answer either: {why}"
                ),
            ),
        }
        self.finish(
            "failed",
            json!({
                "release": release, "previous_release": previous, "migrated": migrated,
                "backup": backup["backup"], "backup_at": backup["backup_at"],
                "switched_back": true, "restored": restored, "previous_healthy": back.is_ok(),
                "message": format!("Release {release} failed its check: {reason}. {}", notes.join(" ")),
            }),
        )
        .await
    }

    // --------------------------------------------------------------- undo
    pub async fn undo(&mut self) -> Db<()> {
        if self.steps.iter().any(|step| step.state == "failed") {
            return self.finish_after_failed_step().await;
        }
        let undone = match self.job.undo_of {
            Some(id) => jobs::get(&*self.client, self.job.tenant, id).await?,
            None => None,
        };
        let Some(facts) = undone.as_ref().and_then(|job| job.result.clone()) else {
            return self
                .nothing_changed(
                    "backup",
                    "the job to undo recorded no result, so there is nothing to go back to",
                )
                .await;
        };
        let (Some(release), Some(previous)) = (
            facts["release"].as_u64(),
            facts["previous_release"].as_u64(),
        ) else {
            return self
                .nothing_changed(
                    "backup",
                    "the job to undo did not record which releases it switched between",
                )
                .await;
        };
        let migrated = facts["migrated"].as_bool().unwrap_or(false);
        let restores = self.step("restore").is_some();
        let current = self.layout.current();
        if !self.is_done("switch") && current != Some(release) {
            return self
                .nothing_changed("backup", &format!("this instance runs release {} now, not release {release}, so job {} is not what is in service", current.map_or("none".to_owned(), |n| n.to_string()), undone.as_ref().map_or(0, |job| job.id)))
                .await;
        }
        if let Err(reason) = layout::verify_checksum(&self.layout.release(previous)) {
            return self
                .nothing_changed(
                    "backup",
                    &format!("release {previous} cannot be put back: {reason}"),
                )
                .await;
        }
        let original = facts["backup"].as_str().map(PathBuf::from);
        if restores && !original.as_ref().is_some_and(|file| file.is_file()) {
            return self.nothing_changed("backup", "the backup taken before the change is missing, so the database cannot be put back").await;
        }
        if !self.is_done("backup") {
            let detail = self.pause_detail(format!(
                "{} pauses from here: the service stops and the database as it is now is backed up first.",
                self.layout.name
            ));
            if !jobs::begin_pause(&mut *self.client, &self.job, "backup", &detail).await? {
                return Ok(());
            }
            match self.backup().await {
                Ok(detail) => self.done("backup", detail).await?,
                Err(reason) => {
                    self.fail("backup", &reason).await?;
                    let _ = self.service.start();
                    return self
                        .finish("failed", json!({"release": release, "previous_release": release, "migrated": false,
                            "message": format!("Nothing changed: the safety backup failed, so the service was started again. {reason}")}))
                        .await;
                }
            }
        }
        let safety = self.detail("backup");
        if restores && !self.is_done("restore") {
            let since = facts["backup_at"].as_str().unwrap_or("").to_owned();
            self.start("restore", &format!("Restoring the backup taken at {since}"))
                .await?;
            let _ = self.service.stop();
            let lost = jobs::changes_since(&*self.client, &since)
                .await
                .unwrap_or_else(|_| json!({"since": since, "changes": 0, "by_kind": []}));
            let file = original.clone().unwrap_or_default();
            match backup::restore(&*self.client, self.pg, &file).await {
                Ok(done) => {
                    let mut notes = vec![jobs::loss_sentence(&lost)];
                    notes.extend(done.problems.clone());
                    self.done("restore", json!({"message": format!("Restored the backup from {since}. {}", notes.join(" ")), "lost": lost, "carried": done.carried, "problems": done.problems}))
                        .await?
                }
                Err(why) => {
                    self.fail("restore", &why).await?;
                    return self
                        .finish("failed", json!({"release": release, "previous_release": release, "migrated": false,
                            "backup": safety["backup"], "backup_at": safety["backup_at"], "restored": false,
                            "message": format!("The database could not be put back: {why}. The service was left stopped; restore {} by hand (the state before this undo is in {}).", file.display(), safety["backup"].as_str().unwrap_or("the safety backup"))}))
                        .await;
                }
            }
        }
        let lost = self.detail("restore")["lost"].clone();
        if !self.is_done("switch") {
            self.start("switch", &format!("Switching back to release {previous}"))
                .await?;
            if let Err(reason) = self.switch_to(previous) {
                self.fail("switch", &reason).await?;
                return self
                    .undo_failed(release, previous, restores, &safety, &reason)
                    .await;
            }
            self.done("switch", json!({"message": format!("current points at release {previous}; the service is starting")}))
                .await?;
        }
        if !self.is_done("verify") {
            self.start("verify", "Checking that the previous release answers")
                .await?;
            match self.service.wait_healthy(HEALTH_LIMIT).await {
                Ok(took) => {
                    self.done("verify", json!({"message": format!("Release {previous} answered after {} s", took.as_secs().max(1))}))
                        .await?
                }
                Err(reason) => {
                    self.fail("verify", &reason).await?;
                    return self.undo_failed(release, previous, restores, &safety, &reason).await;
                }
            }
        }
        let data = if restores {
            jobs::loss_sentence(&lost)
        } else {
            "Your data stayed as it was.".to_owned()
        };
        self.finish(
            "done",
            json!({
                "release": previous, "previous_release": release, "migrated": restores && migrated,
                "backup": safety["backup"], "backup_at": safety["backup_at"], "restored": restores, "lost": lost,
                "message": format!("Back on release {previous}. {data}"),
            }),
        )
        .await?;
        self.prune(&[previous, release]);
        Ok(())
    }
    /// The previous release did not come back: return to the one that was
    /// running, with the safety backup when the database was put back.
    async fn undo_failed(
        &mut self,
        release: u64,
        previous: u64,
        restored: bool,
        safety: &Value,
        reason: &str,
    ) -> Db<()> {
        self.log(&format!("undo failed, returning to release {release}"));
        let _ = self.service.stop();
        let mut notes = Vec::new();
        if restored {
            match safety["backup"].as_str() {
                Some(file) => match backup::restore(&*self.client, self.pg, &PathBuf::from(file)).await {
                    Ok(_) => notes.push("The database is as it was before this undo.".to_owned()),
                    Err(why) => {
                        return self
                            .finish("failed", json!({"release": previous, "previous_release": release, "migrated": true,
                                "backup": safety["backup"], "backup_at": safety["backup_at"],
                                "message": format!("Release {previous} did not answer ({reason}), and the database could not be put back either: {why}. The service was left stopped; restore {file} by hand before starting it.")}))
                            .await
                    }
                },
                None => notes.push("No safety backup was recorded, so the database was left as it is.".to_owned()),
            }
        }
        let back = match self.switch_to(release) {
            Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.map(|_| ()),
            Err(why) => Err(why),
        };
        match &back {
            Ok(()) => notes.insert(0, format!("Release {release} is back in service.")),
            Err(why) => notes.insert(
                0,
                format!("Release {release} was put back but does not answer: {why}"),
            ),
        }
        self.finish(
            "failed",
            json!({
                "release": release, "previous_release": previous, "migrated": false,
                "backup": safety["backup"], "backup_at": safety["backup_at"], "previous_healthy": back.is_ok(),
                "message": format!("Undo failed: release {previous} did not answer ({reason}). {}", notes.join(" ")),
            }),
        )
        .await
    }

    /// A restart landed after a step was marked failed but before the job
    /// was closed: close it, with the service running whatever `current` is.
    async fn finish_after_failed_step(&mut self) -> Db<()> {
        let failed: Vec<String> = self
            .steps
            .iter()
            .filter(|step| step.state == "failed")
            .map(|step| {
                format!(
                    "{}: {}",
                    step.label,
                    step.detail["message"].as_str().unwrap_or("failed")
                )
            })
            .collect();
        if self.service.state() != "active" {
            let _ = self.service.start();
        }
        let current = self.layout.current();
        self.finish(
            "failed",
            json!({"release": current, "previous_release": current, "migrated": false,
                "message": format!("The installer was restarted after a step failed. {} Release {} is in service.", failed.join(" "), current.map_or("none".to_owned(), |n| n.to_string()))}),
        )
        .await
    }

    /// Keeps the releases named, the newest few, and the newest backups.
    fn prune(&self, keep: &[u64]) {
        let numbers = self.layout.release_numbers();
        let newest: Vec<u64> = numbers.iter().rev().take(KEEP_RELEASES).copied().collect();
        for n in numbers {
            if keep.contains(&n) || newest.contains(&n) || Some(n) == self.layout.current() {
                continue;
            }
            match fs::remove_dir_all(self.layout.release(n)) {
                Ok(()) => self.log(&format!("removed old release {n}")),
                Err(error) => self.log(&format!("could not remove old release {n}: {error}")),
            }
        }
        let mut backups: Vec<(u64, PathBuf)> = fs::read_dir(self.layout.backups())
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_str()?.to_owned();
                        let id: u64 = name.strip_prefix("job-")?.parse().ok()?;
                        Some((id, entry.path()))
                    })
                    .collect()
            })
            .unwrap_or_default();
        backups.sort_unstable_by_key(|(id, _)| std::cmp::Reverse(*id));
        for (id, path) in backups.into_iter().skip(KEEP_BACKUPS) {
            match fs::remove_dir_all(&path) {
                Ok(()) => self.log(&format!("removed the backup of job {id}")),
                Err(error) => {
                    self.log(&format!("could not remove the backup of job {id}: {error}"))
                }
            }
        }
    }
}

/// `bilibi-web status` on a copied release: it proves the program runs on
/// this machine, and its first line names what the release holds.
fn program_status(dir: &std::path::Path) -> Result<String, String> {
    let out = Command::new("timeout")
        .arg("30")
        .arg(dir.join("bilibi-web"))
        .arg("status")
        .output()
        .map_err(|error| format!("could not run the copied program: {error}"))?;
    if !out.status.success() {
        return Err(format!(
            "the copied program does not run on this server: {}",
            backup::tail(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .next()
        .unwrap_or("Bilibi")
        .chars()
        .take(200)
        .collect())
}
/// Runs one command of a release's program with the instance's environment
/// (this process has it from the env file), cut off after `limit` seconds.
fn run_program(program: &std::path::Path, command: &str, limit: &str) -> Result<(), String> {
    let out = Command::new("timeout")
        .arg(limit)
        .arg(program)
        .arg(command)
        .output()
        .map_err(|error| format!("could not run {command}: {error}"))?;
    if out.status.success() {
        return Ok(());
    }
    let why = if out.status.code() == Some(124) {
        format!("{command} ran longer than {limit} seconds and was stopped")
    } else {
        backup::tail(&[out.stderr.as_slice(), out.stdout.as_slice()].concat())
    };
    Err(format!("{command} failed: {why}"))
}
