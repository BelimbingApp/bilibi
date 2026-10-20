//! The steps of one job, in the order `base/software` created them, each
//! written back as it runs so the Software page can show it.
//!
//! A job found running when the installer starts was cut off by a restart,
//! and `resume` decides what that means. Before the pause nothing changed:
//! the job ends `interrupted`. During the pause the question is whether the
//! database update committed (the migration ledger grew): if not, the
//! previous program is started and the job ends `interrupted`; if so, the
//! flow carries on. A failure or restore path (`switch_back`, `restore`)
//! writes everything it needs to the job's steps and to files beside the
//! backup, so a restart in it ends with the right release in service.
use crate::{
    backup::{self, PgEnv},
    composition,
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
    /// The installer found this job running when it started.
    resumed: bool,
}
/// The release an undo takes back, as its job recorded it.
struct Undone {
    id: i64,
    release: u64,
    previous: u64,
    migrated: bool,
    original: Option<PathBuf>,
    since: String,
    /// The add-ons that were switched on before it, when it recorded them.
    plugins: Option<Vec<Value>>,
}
/// Where the release the owner asked for was found, and how to name it.
struct Found {
    from: PathBuf,
    /// The source as the result names it: an instance, an address, a file.
    name: String,
    /// "dev release 3", "https://dev.example.com release 3", "the uploaded file upload-2 (…)".
    text: String,
    /// The folder under `incoming/` an uploaded file was unpacked into; it
    /// stays for another try unless the file is spent or cannot be installed.
    uploaded: Option<PathBuf>,
}
/// Where a failed install or undo goes back to, and what it says about it.
struct Back {
    to: u64,
    restore_from: Option<PathBuf>,
    base: Value,
    lead: String,
    restored_note: String,
}
impl<'a> Run<'a> {
    pub async fn new(
        client: &'a mut Client,
        layout: &'a Layout,
        service: &'a Service,
        pg: &'a PgEnv,
        job: Job,
        resumed: bool,
    ) -> Db<Run<'a>> {
        let steps = jobs::steps(&*client, &job).await?;
        Ok(Self {
            client,
            layout,
            service,
            pg,
            job,
            steps,
            resumed,
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
    fn state(&self, key: &str) -> &str {
        self.step(key).map_or("", |step| step.state.as_str())
    }
    fn is_done(&self, key: &str) -> bool {
        self.state(key) == "done"
    }
    fn detail(&self, key: &str) -> Value {
        self.step(key)
            .map_or(Value::Null, |step| step.detail.clone())
    }
    /// The step that failed, not counting the one that goes back.
    fn failed_step(&self) -> Option<&Step> {
        self.steps
            .iter()
            .find(|step| step.state == "failed" && step.key != "switch_back")
    }
    fn failure_reason(&self) -> String {
        self.failed_step()
            .and_then(|step| step.detail["message"].as_str())
            .unwrap_or("a step failed")
            .trim_end_matches('.')
            .to_owned()
    }
    async fn set(&mut self, key: &str, state: &str, detail: Value) -> Db<()> {
        jobs::step_set(&*self.client, &self.job, key, state, &detail).await?;
        if let Some(step) = self.steps.iter_mut().find(|step| step.key == key) {
            step.state = state.into();
            step.detail = detail;
        }
        Ok(())
    }
    async fn add_step(&mut self, key: &str, label: &str, detail: Value) -> Db<()> {
        jobs::step_add(&*self.client, &self.job, key, label, &detail).await?;
        self.steps.push(Step {
            key: key.into(),
            state: "running".into(),
            detail,
        });
        Ok(())
    }
    /// Marks a step running. A step found running already was cut off by a
    /// restart; its detail says so and the step runs again.
    async fn start(&mut self, key: &str, message: &str) -> Db<()> {
        self.start_with(key, json!({"message": message})).await
    }
    async fn start_with(&mut self, key: &str, mut detail: Value) -> Db<()> {
        if self.state(key) == "running" {
            detail["resumed"] = json!(true);
            detail["message"] = json!(format!(
                "{} (the installer restarted during this step and continued)",
                detail["message"].as_str().unwrap_or("Running")
            ));
        }
        self.log(detail["message"].as_str().unwrap_or(key));
        self.set(key, "running", detail).await
    }
    async fn done(&mut self, key: &str, detail: Value) -> Db<()> {
        self.log(detail["message"].as_str().unwrap_or(key));
        self.set(key, "done", detail).await
    }
    async fn fail(&mut self, key: &str, reason: &str) -> Db<()> {
        self.fail_with(key, json!({"message": reason})).await
    }
    async fn fail_with(&mut self, key: &str, detail: Value) -> Db<()> {
        self.log(&format!(
            "{key} failed: {}",
            detail["message"].as_str().unwrap_or("")
        ));
        self.set(key, "failed", detail).await
    }
    async fn finish(&mut self, state: &str, result: Value) -> Db<()> {
        self.finish_with_plugins(state, result, None).await
    }
    /// `finish`, with the add-ons that are to be switched on after it put
    /// in the same transaction as the outcome.
    async fn finish_with_plugins(
        &mut self,
        state: &str,
        result: Value,
        plugins: Option<&[Value]>,
    ) -> Db<()> {
        self.forget_history();
        jobs::skip_pending(&*self.client, &self.job).await?;
        jobs::finish_with_plugins(&mut *self.client, &self.job, state, &result, plugins).await?;
        self.log(&format!(
            "{state}: {}",
            result["message"].as_str().unwrap_or("")
        ));
        Ok(())
    }
    /// The saved history of a restore in this job; see `backup::restore`.
    fn history_file(&self, key: &str) -> PathBuf {
        self.job_backups().join(format!("history-{key}.json"))
    }
    fn job_backups(&self) -> PathBuf {
        self.layout.backups().join(format!("job-{}", self.job.id))
    }
    /// Removed before the outcome is recorded, so a job that has finished
    /// never has a saved history that could be restored again.
    fn forget_history(&self) {
        for entry in fs::read_dir(self.job_backups())
            .into_iter()
            .flatten()
            .flatten()
        {
            if entry.file_name().to_string_lossy().starts_with("history-") {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    /// A step failed and nothing was changed on the instance: it runs the
    /// release it ran before. The service is started again when the pause
    /// had stopped it.
    async fn nothing_changed(&mut self, key: &str, reason: &str) -> Db<()> {
        self.fail(key, reason).await?;
        self.settle_clean_failure().await
    }
    async fn settle_clean_failure(&mut self) -> Db<()> {
        let mut message = format!("Nothing changed. {}.", self.failure_reason());
        if self.state("backup") != "pending" && self.service.state() != "active" {
            let started = match self.service.start() {
                Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.map(|_| ()),
                Err(why) => Err(why),
            };
            message.push_str(&match started {
                Ok(()) => " The service was started again and answers.".to_owned(),
                Err(why) => format!(
                    " The service was started again but does not answer: {why}. Check the service."
                ),
            });
        }
        let current = self.layout.current();
        let backup = self.detail("backup");
        let seconds = self.pause_seconds().await?;
        self.finish(
            "failed",
            json!({
                "release": current, "previous_release": current, "migrated": false,
                "backup": backup["backup"], "backup_at": backup["backup_at"],
                "pause_seconds": seconds, "message": with_pause(&message, seconds),
            }),
        )
        .await
    }
    /// The job ended without changing anything: the owner can start again.
    async fn interrupted(&mut self, message: &str) -> Db<()> {
        let current = self.layout.current();
        let seconds = self.pause_seconds().await?;
        let message = with_pause(message, seconds);
        self.finish(
            "interrupted",
            json!({"release": current, "previous_release": current, "migrated": false,
                "pause_seconds": seconds, "message": message}),
        )
        .await
    }
    /// How long the instance has been paused: from the moment the backup
    /// step stopped the service. None before the pause.
    async fn pause_seconds(&self) -> Db<Option<i64>> {
        match self.detail("backup")["paused_epoch"].as_i64() {
            Some(start) => Ok(Some((jobs::epoch(&*self.client).await? - start).max(0))),
            None => Ok(None),
        }
    }

    // ------------------------------------------------------------- resume
    /// What a restart means for a job found running. True when the restart
    /// ended the job; false when the flow carries on from the first step not
    /// yet done.
    pub async fn resume(&mut self) -> Db<bool> {
        let paused = self.state("backup") != "pending";
        if paused {
            // From the first moment: nothing may be entered while the
            // database is being decided on.
            if let Err(reason) = self.service.stop() {
                self.log(&reason);
            }
        }
        if self.failed_step().is_some() || self.step("switch_back").is_some() {
            self.after_failure().await?;
            return Ok(true);
        }
        if !paused {
            if let Some(stale) = self.detail("fetch")["release"].as_u64() {
                if Some(stale) != self.layout.current() {
                    let _ = fs::remove_dir_all(self.layout.release(stale));
                }
            }
            // A release downloaded for this job is downloaded again when the
            // owner starts again; an uploaded file stays for it.
            self.drop_download();
            self.interrupted(
                "The installer was interrupted before the pause. Nothing changed. Start again.",
            )
            .await?;
            return Ok(true);
        }
        if self.update_committed().await? {
            if self.is_done("switch") {
                let answered = match self.service.start() {
                    Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.map(|_| ()),
                    Err(why) => Err(why),
                };
                if let Err(reason) = answered {
                    self.fail("verify", &reason).await?;
                    self.after_failure().await?;
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let back = match self.service.start() {
            Ok(()) => match self.service.wait_healthy(HEALTH_LIMIT).await {
                Ok(_) => "The release that was in service is back and answers.".to_owned(),
                Err(why) => format!("The release that was in service was started again but does not answer: {why}. Check the service."),
            },
            Err(why) => format!("The release that was in service could not be started again: {why}. Check the service."),
        };
        self.interrupted(&format!(
            "The installer was interrupted during the pause, before the database was changed. {back} Nothing changed. Start again."
        ))
        .await?;
        Ok(true)
    }
    /// Whether the cut-off job had already made the change that cannot be
    /// taken back by starting the old program. An install's is the database
    /// update (the migration ledger grew); an undo's is the restore, or the
    /// switch when it restores nothing.
    async fn update_committed(&self) -> Db<bool> {
        if self.job.kind == "undo" {
            let changing = if self.step("restore").is_some() {
                "restore"
            } else {
                "switch"
            };
            return Ok(matches!(self.state(changing), "running" | "done"));
        }
        Ok(match self.state("migrate") {
            "done" => true,
            "running" => {
                let before = self.detail("migrate")["ledger_before"]
                    .as_i64()
                    .unwrap_or(i64::MAX);
                jobs::ledger_count(&*self.client).await? > before
            }
            _ => false,
        })
    }
    /// Closes a job one of whose steps failed, or that was going back, with
    /// the release in service that the failure calls for.
    async fn after_failure(&mut self) -> Db<()> {
        if self.step("switch_back").is_some() {
            return self.switch_back().await;
        }
        let Some(failed) = self.failed_step().cloned() else {
            return self.settle_clean_failure().await;
        };
        match failed.key.as_str() {
            "switch" | "verify" => self.switch_back().await,
            "migrate" if failed.detail["migrated"].as_bool() == Some(true) => {
                self.switch_back().await
            }
            "restore" => self.finish("failed", failed.detail).await,
            _ => self.settle_clean_failure().await,
        }
    }

    // ------------------------------------------------------------ install
    pub async fn install(&mut self) -> Db<()> {
        let fetched = if self.is_done("fetch") {
            self.detail("fetch")
        } else {
            self.start("fetch", "Checking the release and copying it into place")
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
            let detail = json!({"message": format!(
                "{} pauses from here: the service stops and the database is backed up.",
                self.layout.name
            )});
            if !jobs::begin_pause(&mut *self.client, &self.job, "backup", &detail).await? {
                self.log("stopped before the pause, as asked");
                return Ok(());
            }
            match self.backup().await {
                Ok(detail) => self.done("backup", detail).await?,
                Err(reason) => {
                    self.fail("backup", &format!("The backup failed: {reason}"))
                        .await?;
                    return self.settle_clean_failure().await;
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
                    // The update is judged on the migration ledger alone: if
                    // it grew, the database has changed shape, whichever
                    // command failed.
                    let migrated = jobs::ledger_count(&*self.client).await? > before;
                    self.fail_with(
                        "migrate",
                        json!({"message": reason, "ledger_before": before, "migrated": migrated}),
                    )
                    .await?;
                    return if migrated {
                        self.switch_back().await
                    } else {
                        self.settle_clean_failure().await
                    };
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
                return self.switch_back().await;
            }
            self.done(
                "switch",
                json!({"message": format!("The server is starting release {release}.")}),
            )
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
                    return self.switch_back().await;
                }
            }
        }
        let database = if migrated {
            " The database was updated."
        } else {
            " The database structure stayed the same."
        };
        let seconds = self.pause_seconds().await?;
        let from = match fetched["source_text"].as_str() {
            Some(text) => text.to_owned(),
            None => format!(
                "{} release {}",
                fetched["source"].as_str().unwrap_or("the source"),
                fetched["source_release"]
            ),
        };
        let after = fetched["plugins"].as_array().cloned();
        let before = match &after {
            Some(_) => Some(jobs::enabled_plugins(&*self.client, &self.job).await?),
            None => None,
        };
        let message = format!(
            "Release {release} is in service, installed from {from}.{database} {}{}",
            composition::describe(&fetched["changes"]),
            plugin_sentence(before.as_deref(), after.as_deref()),
        );
        let mut result = json!({
            "release": release, "previous_release": previous, "migrated": migrated,
            "backup": backup["backup"], "backup_at": backup["backup_at"],
            "source": fetched["source"], "source_release": fetched["source_release"],
            "sha256": fetched["sha256"], "program": fetched["program"],
            "platform": fetched["platform"], "domains": fetched["domains"],
            "extensions": fetched["extensions"], "changes": fetched["changes"],
            "pause_seconds": seconds, "message": with_pause(message.trim_end(), seconds),
        });
        if let Some(before) = &before {
            result["plugins"] = json!({"before": before, "after": after});
        }
        if fetched.get("github").is_some() {
            result["github"] = fetched["github"].clone();
        }
        self.finish_with_plugins("done", result, after.as_deref())
            .await?;
        self.prune(&[release, previous]);
        Ok(())
    }

    /// The release the owner asked for, copied into the next number here,
    /// checked and tried. It comes from a sibling instance's folder, from
    /// the folder the program downloaded it into (`incoming/job-<id>/`),
    /// or from an uploaded file (`incoming/upload-<n>/`); the checks are
    /// the same whichever way it came. A downloaded release is never reused
    /// (a new try downloads again under a new job), so its folder goes
    /// whatever the outcome.
    async fn fetch(&mut self) -> Result<Value, String> {
        let fetched = self.fetch_release().await;
        self.drop_download();
        fetched
    }
    /// Removes the folder the program downloaded this job's release into.
    fn drop_download(&self) {
        if let Some(dir) = self.detail("download")["folder"]
            .as_str()
            .and_then(|folder| self.layout.incoming(folder))
        {
            let _ = fs::remove_dir_all(dir);
        }
    }
    async fn fetch_release(&mut self) -> Result<Value, String> {
        if !self.layout.converted() {
            return Err(self.layout.conversion_advice());
        }
        let request = self.job.request.clone();
        let wanted = request["release"].as_u64();
        let source = match request["source"]["kind"].as_str() {
            Some("same_machine") => {
                let name = request["source"]["instance"]
                    .as_str()
                    .ok_or("the request names no source instance")?
                    .to_owned();
                let wanted = wanted.ok_or("the request names no release")?;
                let sibling = self.layout.sibling(&name).map_err(|reason| {
                    format!("{name} is not an instance on this machine: {reason}")
                })?;
                let from = sibling.release(wanted);
                if !from.join("bilibi-web").is_file() {
                    let have = sibling.release_numbers();
                    return Err(format!(
                        "{name} has no release {wanted}. It has: {}.",
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
                match sibling.current() {
                    Some(n) if n == wanted => {}
                    Some(n) => {
                        return Err(format!(
                            "{name} runs release {n} now, not {wanted}. Install the release it runs."
                        ))
                    }
                    None => {
                        return Err(format!(
                            "{name} has no release in service, so it cannot vouch for release {wanted}."
                        ))
                    }
                }
                let source_service = Service::of(&sibling)?;
                if !source_service.answers().await {
                    return Err(format!(
                        "{name} is not answering (its service is {}), so it cannot vouch for release {wanted}.",
                        source_service.state()
                    ));
                }
                Found {
                    from,
                    name: name.clone(),
                    text: format!("{name} release {wanted}"),
                    uploaded: None,
                }
            }
            Some("https") => {
                let address = request["source"]["address"]
                    .as_str()
                    .unwrap_or("the source")
                    .to_owned();
                let download = self.detail("download");
                let folder = download["folder"]
                    .as_str()
                    .ok_or("the download step recorded no folder, so the downloaded release cannot be found")?;
                let from = self
                    .layout
                    .incoming(folder)
                    .filter(|dir| dir.join("bilibi-web").is_file())
                    .ok_or_else(|| format!("the downloaded release is missing from incoming/{folder}; ask again so it is downloaded again"))?;
                let wanted = wanted.ok_or("the request names no release")?;
                Found {
                    from: from.clone(),
                    name: address.clone(),
                    text: format!("{address} release {wanted}"),
                    uploaded: None,
                }
            }
            Some("github") => {
                let repository = request["source"]["repository"]
                    .as_str()
                    .unwrap_or("the GitHub repository")
                    .to_owned();
                let tag = request["source"]["tag"].as_str().unwrap_or("").to_owned();
                let target = request["source"]["target"].as_str().unwrap_or("");
                let expected = request["source"]["digest"].as_str().unwrap_or("");
                let download = self.detail("download");
                let got = download["digest"].as_str().unwrap_or("");
                if expected.is_empty() || got != expected {
                    return Err(
                        "The downloaded GitHub file is not the one that was reviewed. Nothing was copied."
                            .into(),
                    );
                }
                let folder = download["folder"].as_str().ok_or(
                    "the download step recorded no folder, so the GitHub file cannot be found",
                )?;
                let from = self
                    .layout
                    .incoming(folder)
                    .filter(|dir| dir.join("bilibi-web").is_file())
                    .ok_or_else(|| format!("the downloaded release is missing from incoming/{folder}; ask again so it is downloaded again"))?;
                let words = match target {
                    "x86_64-unknown-linux-gnu" => "Linux x86-64",
                    "aarch64-unknown-linux-gnu" => "Linux ARM 64-bit",
                    _ => "this kind of computer",
                };
                Found {
                    from: from.clone(),
                    name: repository.clone(),
                    text: format!("GitHub {repository} tag {tag} ({words})"),
                    uploaded: None,
                }
            }
            Some("upload") => {
                let bundle = request["source"]["bundle"]
                    .as_str()
                    .ok_or("the request names no uploaded file")?
                    .to_owned();
                let from = self
                    .layout
                    .incoming(&bundle)
                    .filter(|dir| dir.join("bilibi-web").is_file())
                    .ok_or_else(|| {
                        format!("The uploaded file {bundle} is no longer here. Upload it again.")
                    })?;
                let origin = request["source"]["from"]["label"]
                    .as_str()
                    .map_or(String::new(), |label| format!(" from {label}"));
                let detail = match wanted {
                    Some(n) => format!(" (release {n}{origin})"),
                    None if origin.is_empty() => String::new(),
                    None => format!(" ({})", origin.trim_start()),
                };
                Found {
                    from: from.clone(),
                    name: bundle.clone(),
                    text: format!("the uploaded file {bundle}{detail}"),
                    uploaded: Some(from),
                }
            }
            _ => return Err("the request names no source".into()),
        };
        let checked = self.copy_checked(&source, wanted).await;
        if let Some(dir) = &source.uploaded {
            // An uploaded file waits for another try, unless it is now copied
            // into place or failed its checks.
            if checked.is_ok()
                || matches!(&checked, Err(reason) if reason.contains("checksum") || reason.contains("built for") || reason.contains("does not run"))
            {
                let _ = fs::remove_dir_all(dir);
            }
        }
        let mut checked = checked?;
        if request["source"]["kind"].as_str() == Some("github") {
            checked["github"] = json!({
                "repository": request["source"]["repository"],
                "repository_id": request["source"]["repository_id"],
                "release_id": request["source"]["release_id"],
                "asset_id": request["source"]["asset_id"],
                "tag": request["source"]["tag"],
                "target": request["source"]["target"],
                "archive_sha256": request["source"]["digest"],
                "program_sha256": checked["sha256"],
            });
        }
        Ok(checked)
    }
    /// The checks every way in shares, then the copy: the checksum, built
    /// for this kind of server, not already in service, disk for the
    /// program and a backup; then the copy into the next number, checked
    /// again and run once to prove it starts here.
    async fn copy_checked(&mut self, source: &Found, wanted: Option<u64>) -> Result<Value, String> {
        let from = &source.from;
        let sha = layout::verify_checksum(from)?;
        layout::built_for_this_server(from)?;
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
                "This instance already runs the same program as {} (release {current} here). There is nothing to install.",
                source.text
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
        let bytes = layout::copy_release(from, &dst).map_err(|error| {
            let _ = fs::remove_dir_all(&dst);
            format!("could not copy into {}: {error}", dst.display())
        })?;
        if layout::verify_checksum(&dst)? != sha {
            let _ = fs::remove_dir_all(&dst);
            return Err(format!(
                "the copy in {} does not match the source",
                dst.display()
            ));
        }
        let status = program_status(&dst).inspect_err(|_| {
            let _ = fs::remove_dir_all(&dst);
        })?;
        let plugins = layout::read_plugins(&dst).inspect_err(|_| {
            let _ = fs::remove_dir_all(&dst);
        })?;
        let program: String = status
            .lines()
            .next()
            .unwrap_or("Bilibi")
            .chars()
            .take(200)
            .collect();
        let now = composition::parse(&status);
        let changes = composition::diff(&composition_of(&self.layout.release(current)), &now);
        Ok(json!({
            "message": format!("Copied {} as release {n}. Bilibi version: {}.", source.text, now["platform"]["version"].as_str().unwrap_or("not recorded")),
            "release": n, "previous": current, "source_release": wanted, "source": source.name, "source_text": source.text,
            "sha256": sha, "bytes": bytes, "program": program,
            "platform": now["platform"], "domains": now["domains"], "extensions": now["extensions"],
            "changes": changes, "plugins": plugins,
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
                    json!({"message": "The job stopped before the pause, as requested. Nothing changed on this server."}),
                )
                .await?;
                return Ok(false);
            }
            if switch == "auto" {
                return Ok(true);
            }
            if !told {
                self.set("backup", "pending", json!({"message": "The update is ready. This server keeps running until the owner confirms installation."}))
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
        let paused_epoch = jobs::epoch(&*self.client)
            .await
            .map_err(|error| jobs::short(&error))?;
        let at = jobs::now(&*self.client)
            .await
            .map_err(|error| jobs::short(&error))?;
        let file = self.backup_file();
        let bytes = backup::dump(self.pg, &file)?;
        Ok(json!({
            "message": format!("{} is paused. Backed up the database ({}).", self.layout.name, human(bytes)),
            "backup": file.display().to_string(), "backup_at": at, "paused_epoch": paused_epoch, "keep_backups": KEEP_BACKUPS, "bytes": bytes,
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
    async fn migrate(&self, release: u64, before: i64) -> Result<Value, String> {
        let program = self.layout.release(release).join("bilibi-web");
        run_program(&program, "migrate", MIGRATE_LIMIT_SECONDS)?;
        run_program(&program, "seed-production", "600")?;
        let after = jobs::ledger_count(&*self.client)
            .await
            .map_err(|error| jobs::short(&error))?;
        let applied = (after - before).max(0);
        Ok(json!({
            "message": if applied == 0 { "The database structure is already up to date.".to_owned() }
                else { format!("Applied {applied} database update{} together.", if applied == 1 { "" } else { "s" }) },
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
    /// Goes back after a failure: the release that was in service returns,
    /// with the backup restored first when the database was changed. It is a
    /// step of the job like the others, so a restart in it ends the same way.
    async fn switch_back(&mut self) -> Db<()> {
        if matches!(self.state("switch_back"), "done" | "failed") {
            let mut result = self.detail("switch_back");
            if self.is_done("switch_back") {
                let answered = match self.service.start() {
                    Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.map(|_| ()),
                    Err(why) => Err(why),
                };
                if let Err(why) = answered {
                    result["previous_healthy"] = json!(false);
                    result["message"] = json!(format!(
                        "{} After the installer restarted, the release does not answer: {why}. Check the service.",
                        result["message"].as_str().unwrap_or("")
                    ));
                }
            }
            return self.finish("failed", result).await;
        }
        let reason = self.failure_reason();
        let back = match self.back_plan(&reason).await? {
            Ok(back) => back,
            Err(why) => {
                return self
                    .finish(
                        "failed",
                        json!({"message": format!("{reason}. {why} Check the service.")}),
                    )
                    .await
            }
        };
        let label = format!("Switch back to release {}", back.to);
        if self.step("switch_back").is_none() {
            self.add_step("switch_back", &label, json!({"message": label}))
                .await?;
        }
        self.log(&label);
        if let Err(why) = self.service.stop() {
            self.log(&why);
        }
        let mut restored = self.detail("switch_back")["restored"]
            .as_bool()
            .unwrap_or(false);
        let mut problems = Vec::new();
        if let (Some(file), false) = (&back.restore_from, restored) {
            match backup::restore(
                &*self.client,
                self.pg,
                file,
                back.base["backup_at"].as_str().unwrap_or(""),
                &self.history_file("switch_back"),
            )
            .await
            {
                Ok(done) => {
                    restored = true;
                    problems = done.problems;
                    self.set(
                        "switch_back",
                        "running",
                        json!({"message": label, "restored": true}),
                    )
                    .await?;
                }
                Err(why) => {
                    let mut result = back.base.clone();
                    result["switched_back"] = json!(false);
                    result["restored"] = json!(false);
                    result["message"] = json!(format!(
                        "{} The database could not be put back: {why}. The service was left stopped; restore {} by hand before starting it.",
                        back.lead, file.display()
                    ));
                    self.set("switch_back", "failed", result.clone()).await?;
                    return self.finish("failed", result).await;
                }
            }
        }
        let answered = match self.switch_to(back.to) {
            Ok(()) => self.service.wait_healthy(HEALTH_LIMIT).await.map(|_| ()),
            Err(why) => Err(why),
        };
        let mut notes = vec![match &answered {
            Ok(()) => format!("Switched back to release {}, which answers.", back.to),
            Err(why) => format!(
                "Switched back to release {}, but it does not answer either: {why}",
                back.to
            ),
        }];
        if restored {
            notes.push(back.restored_note.clone());
        } else if back.base["migrated"].as_bool() == Some(true) {
            notes.push(
                "The database structure changed, but no backup was recorded. The database was left as it is."
                    .to_owned(),
            );
        }
        notes.extend(problems);
        let seconds = self.pause_seconds().await?;
        if let Some(seconds) = seconds {
            notes.push(pause_sentence(seconds));
        }
        let mut result = back.base;
        result["pause_seconds"] = json!(seconds);
        result["switched_back"] = json!(true);
        result["restored"] = json!(restored);
        result["previous_healthy"] = json!(answered.is_ok());
        result["message"] = json!(format!("{} {}", back.lead, notes.join(" ")));
        self.set("switch_back", "done", result.clone()).await?;
        self.finish("failed", result).await
    }
    /// Where to go back to, from what the job's steps recorded.
    async fn back_plan(&self, reason: &str) -> Db<Result<Back, String>> {
        let backup = self.detail("backup");
        if self.job.kind == "undo" {
            let undone = match self.undone().await? {
                Ok(undone) => undone,
                Err(why) => return Ok(Err(why)),
            };
            let restore_from = self
                .step("restore")
                .and(backup["backup"].as_str().map(PathBuf::from));
            return Ok(Ok(Back {
                to: undone.release,
                restore_from,
                base: json!({"release": undone.release, "previous_release": undone.previous, "migrated": false,
                    "backup": backup["backup"], "backup_at": backup["backup_at"]}),
                lead: format!(
                    "Undo failed: release {} did not answer ({reason}).",
                    undone.previous
                ),
                restored_note: "The database is as it was before this undo.".into(),
            }));
        }
        let fetched = self.detail("fetch");
        let (Some(release), Some(previous)) =
            (fetched["release"].as_u64(), fetched["previous"].as_u64())
        else {
            return Ok(Err(
                "The releases it was switching between were not recorded.".into(),
            ));
        };
        let migrated = self.detail("migrate")["migrated"]
            .as_bool()
            .unwrap_or(false);
        let lead = if self.failed_step().is_some_and(|step| step.key == "migrate") {
            format!("The database update of release {release} failed after it had changed the database: {reason}.")
        } else {
            format!("Release {release} failed its check: {reason}.")
        };
        let nothing_lost = if self.resumed {
            "."
        } else {
            ", so nothing entered is lost."
        };
        Ok(Ok(Back {
            to: previous,
            restore_from: backup["backup"]
                .as_str()
                .map(PathBuf::from)
                .filter(|_| migrated),
            base: json!({"release": release, "previous_release": previous, "migrated": migrated,
                "backup": backup["backup"], "backup_at": backup["backup_at"]}),
            lead,
            restored_note: format!(
                "The database backup taken before the update was restored{nothing_lost}"
            ),
        }))
    }

    // --------------------------------------------------------------- undo
    pub async fn undo(&mut self) -> Db<()> {
        let undone = match self.undone().await? {
            Ok(undone) => undone,
            Err(reason) => return self.nothing_changed("backup", &reason).await,
        };
        let restores = self.step("restore").is_some();
        if !self.resumed {
            if !self.layout.converted() {
                let advice = self.layout.conversion_advice();
                return self.nothing_changed("backup", &advice).await;
            }
            let current = self.layout.current();
            if current != Some(undone.release) {
                return self
                    .nothing_changed("backup", &format!("this instance runs release {} now, not release {}, so job {} is not what is in service", current.map_or("none".to_owned(), |n| n.to_string()), undone.release, undone.id))
                    .await;
            }
            if let Err(reason) = layout::verify_checksum(&self.layout.release(undone.previous)) {
                return self
                    .nothing_changed(
                        "backup",
                        &format!("release {} cannot be put back: {reason}", undone.previous),
                    )
                    .await;
            }
            if restores && !undone.original.as_ref().is_some_and(|file| file.is_file()) {
                return self.nothing_changed("backup", "the backup taken before the change is missing, so the database cannot be put back").await;
            }
        }
        if !self.is_done("backup") {
            let detail = json!({"message": format!(
                "{} pauses from here: the service stops and the database as it is now is backed up first.",
                self.layout.name
            )});
            if !jobs::begin_pause(&mut *self.client, &self.job, "backup", &detail).await? {
                return Ok(());
            }
            match self.backup().await {
                Ok(detail) => self.done("backup", detail).await?,
                Err(reason) => {
                    self.fail("backup", &format!("The safety backup failed: {reason}"))
                        .await?;
                    return self.settle_clean_failure().await;
                }
            }
        }
        let safety = self.detail("backup");
        if restores && !self.is_done("restore") {
            let lost = match self.detail("restore")["lost"].clone() {
                lost if lost.is_object() => lost,
                _ => jobs::changes_since(&*self.client, &undone.since).await?,
            };
            self.start_with(
                "restore",
                json!({"message": format!("Restoring the backup taken at {}", undone.since), "lost": lost}),
            )
            .await?;
            let _ = self.service.stop();
            let file = undone.original.clone().unwrap_or_default();
            match backup::restore(
                &*self.client,
                self.pg,
                &file,
                &undone.since,
                &self.history_file("restore"),
            )
            .await
            {
                Ok(done) => {
                    self.done("restore", json!({"message": format!("Restored the backup from {}. {}", undone.since, done.problems.join(" ")), "lost": lost, "carried": done.carried, "problems": done.problems}))
                        .await?
                }
                Err(why) => {
                    let result = json!({
                        "release": undone.release, "previous_release": undone.release, "migrated": false,
                        "backup": safety["backup"], "backup_at": safety["backup_at"], "restored": false,
                        "message": format!("The database could not be put back: {why}. The service was left stopped; restore {} by hand (the state before this undo is in {}).", file.display(), safety["backup"].as_str().unwrap_or("the safety backup")),
                    });
                    self.fail_with("restore", result.clone()).await?;
                    return self.finish("failed", result).await;
                }
            }
        }
        let previous = undone.previous;
        if !self.is_done("switch") {
            self.start("switch", &format!("Switching back to release {previous}"))
                .await?;
            if let Err(reason) = self.switch_to(previous) {
                self.fail("switch", &reason).await?;
                return self.switch_back().await;
            }
            self.done(
                "switch",
                json!({"message": format!("The server is starting release {previous}.")}),
            )
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
                    return self.switch_back().await;
                }
            }
        }
        let data = if restores {
            format!("The database was put back as it was at {}.", undone.since)
        } else {
            "Your data stayed as it was.".to_owned()
        };
        let now = composition_of(&self.layout.release(previous));
        let changes =
            composition::diff(&composition_of(&self.layout.release(undone.release)), &now);
        let seconds = self.pause_seconds().await?;
        let before = match &undone.plugins {
            Some(_) => Some(jobs::enabled_plugins(&*self.client, &self.job).await?),
            None => None,
        };
        let message = format!(
            "Back on release {previous}. {data} {}{}",
            composition::describe(&changes),
            plugin_sentence(before.as_deref(), undone.plugins.as_deref()),
        );
        let mut result = json!({
            "release": previous, "previous_release": undone.release, "migrated": restores && undone.migrated,
            "backup": safety["backup"], "backup_at": safety["backup_at"], "restored": restores,
            "lost": self.detail("restore")["lost"],
            "platform": now["platform"], "domains": now["domains"], "extensions": now["extensions"],
            "changes": changes,
            "pause_seconds": seconds, "message": with_pause(message.trim_end(), seconds),
        });
        if let Some(before) = &before {
            result["plugins"] = json!({"before": before, "after": undone.plugins});
        }
        self.finish_with_plugins("done", result, undone.plugins.as_deref())
            .await?;
        self.prune(&[previous, undone.release]);
        Ok(())
    }
    /// The job this undo takes back, and what it recorded for Undo.
    async fn undone(&self) -> Db<Result<Undone, String>> {
        let job = match self.job.undo_of {
            Some(id) => jobs::get(&*self.client, self.job.tenant, id).await?,
            None => None,
        };
        let Some(job) = job else {
            return Ok(Err("the job to undo was not found".into()));
        };
        let Some(facts) = job.result.as_ref() else {
            return Ok(Err(
                "the job to undo recorded no result, so there is nothing to go back to".into(),
            ));
        };
        let (Some(release), Some(previous)) = (
            facts["release"].as_u64(),
            facts["previous_release"].as_u64(),
        ) else {
            return Ok(Err(
                "the job to undo did not record which releases it switched between".into(),
            ));
        };
        Ok(Ok(Undone {
            id: job.id,
            release,
            previous,
            migrated: facts["migrated"].as_bool().unwrap_or(false),
            original: facts["backup"].as_str().map(PathBuf::from),
            since: facts["backup_at"].as_str().unwrap_or("").to_owned(),
            plugins: facts["plugins"]["before"].as_array().cloned(),
        }))
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

/// `bilibi-web status` on a release folder: it proves the program runs on
/// this machine, and its text names what the release holds.
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
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
/// What a release folder's program says it is made of; null when it does not
/// run.
fn composition_of(dir: &std::path::Path) -> Value {
    program_status(dir).map_or(Value::Null, |status| composition::parse(&status))
}
/// What changes in the add-ons switched on, in words, or nothing when the
/// release does not say or the list stays the same.
fn plugin_sentence(before: Option<&[Value]>, after: Option<&[Value]>) -> String {
    let (Some(before), Some(after)) = (before, after) else {
        return String::new();
    };
    let ids = |list: &[Value]| -> Vec<String> {
        list.iter()
            .filter_map(|plugin| plugin["id"].as_str().map(str::to_owned))
            .collect()
    };
    let (was, now) = (ids(before), ids(after));
    let on: Vec<&String> = now.iter().filter(|id| !was.contains(id)).collect();
    let off: Vec<&String> = was.iter().filter(|id| !now.contains(id)).collect();
    let mut parts = Vec::new();
    if !on.is_empty() {
        parts.push(format!(
            "switched on {}",
            on.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if !off.is_empty() {
        parts.push(format!(
            "switched off {}",
            off.iter()
                .map(|id| id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!(" Add-ons: {}.", parts.join("; "))
    }
}
fn pause_sentence(seconds: i64) -> String {
    format!(
        "The pause lasted {seconds} second{}.",
        if seconds == 1 { "" } else { "s" }
    )
}
fn with_pause(message: &str, seconds: Option<i64>) -> String {
    match seconds {
        Some(seconds) => format!("{message} {}", pause_sentence(seconds)),
        None => message.to_owned(),
    }
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
