//! The build itself: check the server, fetch the Platform and the Domains at
//! the commits the owner reviewed, pin, compile a release at low priority
//! into a kept cache, and copy the result into `releases/<n>` beside the
//! running program. The running instance is never touched. This file knows
//! nothing of the job tables: progress goes out through a channel and a
//! cancel comes in through another; `worker.rs` wires both to the database.
use crate::toolchain::{tail, version_at_least, Toolchain};
use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    sync::{mpsc, watch},
};

/// Where the Platform is cloned from when the request does not say.
const DEFAULT_PLATFORM_REPOSITORY: &str = "https://github.com/BelimbingApp/bilibi";
/// A release build needs this much free disk where the cache lives and
/// this much memory (swap counts): the scout's numbers, report section 6.
const BUILD_DISK_MIN_GB: u64 = 8;
const RELEASE_DISK_MIN_GB: u64 = 1;
const MEMORY_MIN_GB: u64 = 4;
/// Seconds one compiled part takes before any build has been measured here:
/// about 10 minutes for 400 parts with two compilers.
const DEFAULT_SECONDS_PER_PART: f64 = 1.5;
const HISTORY_KEPT: usize = 20;
/// How often a running compile reports its count.
const PROGRESS_EVERY: Duration = Duration::from_secs(2);
const GB: u64 = 1 << 30;

/// Where the builder keeps and puts things.
#[derive(Clone, Debug)]
pub struct Paths {
    /// The builder's own folder: the Platform checkout, the build cache, logs.
    pub build_dir: PathBuf,
    /// The instance folder whose `releases/` receives the result.
    pub instance_dir: PathBuf,
    /// The builder user's home, where the toolchain lives.
    pub home: PathBuf,
}
impl Paths {
    pub const BUILD_DIR_KEY: &'static str = "BILIBI_BUILD_DIR";
    pub const INSTANCE_DIR_KEY: &'static str = "BILIBI_INSTANCE_DIR";
    const DEFAULT_BUILD_DIR: &'static str = "/opt/bilibi/build";
    /// `BILIBI_INSTANCE_DIR` (the instance's folder, required) and
    /// `BILIBI_BUILD_DIR` (default `/opt/bilibi/build`), as the instance's
    /// `env` file written by `scripts/install-server.sh` sets them.
    pub fn from_env() -> Result<Self, String> {
        let instance_dir = std::env::var_os(Self::INSTANCE_DIR_KEY)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .ok_or_else(|| format!("set {} to the instance's folder, like /opt/bilibi/dev (scripts/install-server.sh writes it into the instance's env file)", Self::INSTANCE_DIR_KEY))?;
        let build_dir = std::env::var_os(Self::BUILD_DIR_KEY)
            .filter(|v| !v.is_empty())
            .map_or_else(|| PathBuf::from(Self::DEFAULT_BUILD_DIR), PathBuf::from);
        let home = std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map_or_else(|| build_dir.clone(), PathBuf::from);
        Ok(Self {
            build_dir,
            instance_dir,
            home,
        })
    }
    pub fn src(&self) -> PathBuf {
        self.build_dir.join("src")
    }
    pub fn target(&self) -> PathBuf {
        self.build_dir.join("target")
    }
    pub fn logs(&self) -> PathBuf {
        self.build_dir.join("logs")
    }
    fn history(&self) -> PathBuf {
        self.build_dir.join("history.json")
    }
    pub fn releases(&self) -> PathBuf {
        self.instance_dir.join("releases")
    }
}

/// What the job's request asks for, as the review stored it: the Platform
/// commit and each Domain's repository and commit (or ref, outside the
/// catalogue). Re-checked here because paths and Git arguments are built
/// from it.
#[derive(Clone, Debug)]
pub struct Plan {
    pub platform_repository: String,
    pub platform_commit: String,
    pub platform_version: String,
    pub domains: Vec<PlannedDomain>,
}
#[derive(Clone, Debug)]
pub struct PlannedDomain {
    pub id: String,
    pub repository: String,
    /// A commit (catalogue) or a tag, branch or commit (outside it).
    pub reference: String,
    /// The catalogue version, when it has one.
    pub version: Option<String>,
}
impl Plan {
    pub fn from_request(request: &Value) -> Result<Self, String> {
        let text = |value: &Value| {
            value
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
        };
        let platform_version =
            text(&request["platform"]).ok_or("the request names no Platform version")?;
        let platform_commit = text(&request["platform_commit"])
            .ok_or("the request names no Platform commit to build from; plan the change again")?;
        if !valid_commit(&platform_commit) {
            return Err(format!(
                "the Platform commit {platform_commit} is not a Git commit id"
            ));
        }
        let platform_repository = text(&request["platform_repository"])
            .unwrap_or_else(|| DEFAULT_PLATFORM_REPOSITORY.to_owned());
        if !valid_repository(&platform_repository) {
            return Err(format!(
                "the Platform repository {platform_repository} is not an https:// Git address"
            ));
        }
        let mut domains = Vec::new();
        for entry in request["domains"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let id = text(&entry["id"]).ok_or("a Domain in the request has no id")?;
            if !valid_id(&id) {
                return Err(format!("the Domain id {id} is not snake_case"));
            }
            let repository = text(&entry["repository"]).ok_or_else(|| {
                format!("Domain {id} has no repository in the request; plan the change again")
            })?;
            if !valid_repository(&repository) {
                return Err(format!(
                    "the repository of Domain {id} is not an https:// Git address"
                ));
            }
            let version = text(&entry["version"]);
            let reference = match (text(&entry["commit"]), text(&entry["ref"])) {
                (Some(commit), _) if valid_commit(&commit) => commit,
                (Some(commit), _) => {
                    return Err(format!(
                        "the commit of Domain {id} ({commit}) is not a Git commit id"
                    ))
                }
                (None, Some(reference)) if valid_reference(&reference) => reference,
                (None, Some(reference)) => {
                    return Err(format!(
                        "the ref of Domain {id} ({reference}) is not a Git ref"
                    ))
                }
                (None, None) => {
                    return Err(format!(
                        "Domain {id} has neither a commit nor a ref in the request"
                    ))
                }
            };
            domains.push(PlannedDomain {
                id,
                repository,
                reference,
                version,
            });
        }
        Ok(Self {
            platform_repository,
            platform_commit,
            platform_version,
            domains,
        })
    }
    /// "Bilibi 0.1.0 with finance 0.1.0 and factory 0.1.0", or "with no Domains".
    pub fn describe(&self) -> String {
        let domains: Vec<String> = self
            .domains
            .iter()
            .map(|d| match &d.version {
                Some(version) => format!("{} {version}", d.id),
                None => format!("{} at {}", d.id, short(&d.reference)),
            })
            .collect();
        format!(
            "Bilibi {} with {}",
            self.platform_version,
            if domains.is_empty() {
                "no Domains".to_owned()
            } else {
                join_and(&domains)
            }
        )
    }
}
fn valid_commit(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}
fn valid_id(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn valid_reference(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 128
        && !text.starts_with('-')
        && !text.chars().any(|c| c.is_whitespace() || c.is_control())
}
fn valid_repository(text: &str) -> bool {
    text.strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .is_some_and(|host| !host.is_empty() && !host.contains('@'))
        && !text.contains(['?', '#'])
        && text.len() <= 512
        && !text.chars().any(char::is_whitespace)
}
fn short(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}
fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

/// Why a step did not finish.
#[derive(Debug)]
pub enum Failure {
    /// The owner asked to stop; the step was abandoned where it was.
    Cancelled,
    /// What went wrong in plain words, with the tail of the tool's output.
    Failed {
        message: String,
        details: Option<String>,
    },
}
impl Failure {
    fn failed(message: impl Into<String>) -> Self {
        Self::Failed {
            message: message.into(),
            details: None,
        }
    }
    fn with(message: impl Into<String>, details: impl Into<String>) -> Self {
        let details: String = details.into();
        Self::Failed {
            message: message.into(),
            details: (!details.trim().is_empty()).then_some(details),
        }
    }
}
type Outcome<T = Value> = Result<T, Failure>;

/// What a finished build produced.
#[derive(Clone, Debug)]
pub struct Built {
    pub release: u64,
    pub path: PathBuf,
    pub seconds: u64,
    pub compiled: u64,
    pub fresh: u64,
    /// What the new program says it is made of (`bilibi-web status`), when
    /// it is new enough to say.
    pub status: Option<String>,
    pub platform: Value,
    pub domains: Vec<Value>,
}

/// One build in progress. `progress` receives the running step's detail (a
/// plain `message` and numbers); `cancel` turns true when the owner asked
/// to stop, and every command the builder runs then stops too.
pub struct Build<'a> {
    pub paths: &'a Paths,
    pub toolchain: &'a Toolchain,
    pub plan: &'a Plan,
    pub job: i64,
    /// Compiler processes at once; 0 lets Cargo decide.
    pub jobs: u32,
    pub progress: mpsc::Sender<Value>,
    pub cancel: watch::Receiver<bool>,
}
impl Build<'_> {
    fn log_path(&self) -> PathBuf {
        self.paths.logs().join(format!("job-{}.log", self.job))
    }
    fn log_file(&self) -> std::io::Result<std::fs::File> {
        std::fs::create_dir_all(self.paths.logs())?;
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.log_path())
    }
    async fn say(&self, message: impl Into<String>) {
        let _ = self.progress.send(json!({"message": message.into()})).await;
    }
    /// Resolves when the owner cancels; never, when nobody watches any more.
    async fn cancelled(&mut self) {
        if self.cancel.wait_for(|stop| *stop).await.is_err() {
            std::future::pending::<()>().await
        }
    }
    fn is_cancelled(&self) -> bool {
        *self.cancel.borrow()
    }

    /// A command with the toolchain on its path, the build cache as its
    /// target folder, and the lowest CPU priority: the instance that runs
    /// beside the build keeps its speed.
    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env("PATH", self.toolchain.path_env())
            .env("HOME", &self.paths.home)
            .env("CARGO_TARGET_DIR", self.paths.target())
            .env("CARGO_TERM_COLOR", "never")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("CARGO_INCREMENTAL")
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .process_group(0);
        if self.jobs > 0 {
            command.env("CARGO_BUILD_JOBS", self.jobs.to_string());
        }
        // SAFETY: setpriority is async-signal-safe and touches only this
        // process, between fork and exec.
        unsafe {
            command.pre_exec(|| {
                libc::setpriority(libc::PRIO_PROCESS, 0, 19);
                Ok(())
            });
        }
        command
    }
    /// Runs a command to completion unless a cancel arrives first, in which
    /// case its whole process group is stopped. Output goes to the job log
    /// and comes back for the failure detail.
    async fn run(&mut self, mut command: Command, what: &str) -> Outcome<String> {
        if self.is_cancelled() {
            return Err(Failure::Cancelled);
        }
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let child = command
            .spawn()
            .map_err(|e| Failure::failed(format!("could not start {what}: {e}")))?;
        let pid = child.id();
        let waited = tokio::select! {
            output = child.wait_with_output() => output,
            () = self.cancelled() => {
                stop_group(pid).await;
                return Err(Failure::Cancelled);
            }
        };
        let output = waited.map_err(|e| Failure::failed(format!("{what} did not finish: {e}")))?;
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        if let Ok(mut log) = self.log_file() {
            use std::io::Write;
            let _ = writeln!(log, "\n$ {what}\n{text}");
        }
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).into_owned())
        } else {
            Err(Failure::with(
                format!("{what} failed ({})", describe_exit(&output.status)),
                tail(&text, 20),
            ))
        }
    }

    /// Step 1, "Check the server": the build tools (installed now when
    /// missing), free disk, memory. Nothing is changed when a check fails.
    pub async fn check(&mut self) -> Outcome {
        let rust = match self.toolchain.version().await {
            Some(version) => version,
            None => {
                self.say("Installing the Rust build tools (about 1.5 GB, once)")
                    .await;
                self.toolchain
                    .install(&self.paths.build_dir)
                    .await
                    .map_err(|why| {
                        Failure::failed(format!(
                            "The Rust build tools could not be installed: {why}."
                        ))
                    })?
            }
        };
        let mut missing = Vec::new();
        let mut versions = Vec::new();
        for (program, package) in [
            ("git", "git"),
            ("python3", "python3"),
            ("cc", "build-essential"),
        ] {
            match Command::new(program)
                .arg("--version")
                .env("PATH", self.toolchain.path_env())
                .output()
                .await
            {
                Ok(output) if output.status.success() => versions.push(
                    String::from_utf8_lossy(&output.stdout)
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .to_owned(),
                ),
                _ => missing.push(package),
            }
        }
        if !missing.is_empty() {
            return Err(Failure::failed(format!(
                "This server lacks build tools: {}. Ask the server owner to run: sudo apt-get install {}",
                join_and(&missing.iter().map(|m| m.to_string()).collect::<Vec<_>>()),
                missing.join(" ")
            )));
        }
        std::fs::create_dir_all(&self.paths.build_dir).map_err(|e| {
            Failure::failed(format!(
                "The build folder {} cannot be created: {e}.",
                self.paths.build_dir.display()
            ))
        })?;
        let build_free = free_space(&self.paths.build_dir)?;
        if build_free < BUILD_DISK_MIN_GB * GB {
            return Err(Failure::failed(format!(
                "Only {} free on the disk holding {}; a build needs {BUILD_DISK_MIN_GB} GB. Free some space, or move the build folder.",
                gb(build_free),
                self.paths.build_dir.display()
            )));
        }
        let releases = self.paths.releases();
        std::fs::create_dir_all(&releases).map_err(|e| {
            Failure::failed(format!(
                "The releases folder {} cannot be created: {e}.",
                releases.display()
            ))
        })?;
        let release_free = free_space(&releases)?;
        if release_free < RELEASE_DISK_MIN_GB * GB {
            return Err(Failure::failed(format!(
                "Only {} free on the disk holding {}; a release needs {RELEASE_DISK_MIN_GB} GB.",
                gb(release_free),
                releases.display()
            )));
        }
        let memory = Memory::read()?;
        if memory.total + memory.swap < MEMORY_MIN_GB * GB {
            return Err(Failure::failed(format!(
                "This server has {} of memory{}; a build needs {MEMORY_MIN_GB} GB. Build on a bigger machine, or add swap.",
                gb(memory.total),
                if memory.swap > 0 { format!(" plus {} swap", gb(memory.swap)) } else { String::new() }
            )));
        }
        let message = format!(
            "Disk {} free, memory {}{}, Rust {rust}, {}{}",
            gb(build_free),
            gb(memory.total),
            if memory.swap > 0 {
                format!(" plus {} swap", gb(memory.swap))
            } else {
                String::new()
            },
            versions.join(", "),
            if self.jobs > 0 {
                format!(
                    ", {} compiler{} at a time",
                    self.jobs,
                    if self.jobs == 1 { "" } else { "s" }
                )
            } else {
                String::new()
            }
        );
        Ok(json!({
            "message": message,
            "disk_free_gb": build_free / GB, "memory_gb": memory.total / GB, "swap_gb": memory.swap / GB,
            "rust": rust, "tools": versions, "compiler_jobs": self.jobs,
        }))
    }

    /// Step 2, "Download the Platform and the Domains": one kept Platform
    /// checkout at the reviewed commit, each wanted Domain cloned into
    /// `apps/domains/<id>` at its commit, every other Domain removed.
    pub async fn fetch(&mut self) -> Outcome {
        let src = self.paths.src();
        let plan = self.plan.clone();
        if !src.join(".git").is_dir() {
            self.say(format!(
                "Downloading Bilibi from {}",
                plan.platform_repository
            ))
            .await;
            let _ = std::fs::remove_dir_all(&src);
            let mut clone = self.command("git");
            clone.arg("clone").arg(&plan.platform_repository).arg(&src);
            self.run(clone, "git clone of Bilibi").await?;
        } else {
            self.say("Updating the Bilibi checkout").await;
            let mut url = self.command("git");
            url.current_dir(&src)
                .args(["remote", "set-url", "origin"])
                .arg(&plan.platform_repository);
            self.run(url, "git remote set-url for Bilibi").await?;
            let mut fetch = self.command("git");
            fetch
                .current_dir(&src)
                .args(["fetch", "--tags", "--force", "--prune", "origin"]);
            self.run(fetch, "git fetch of Bilibi").await?;
        }
        let mut checkout = self.command("git");
        checkout
            .current_dir(&src)
            .args(["checkout", "-f", "--detach"])
            .arg(&plan.platform_commit);
        self.run(checkout, "git checkout of the Bilibi commit")
            .await
            .map_err(|failure| match failure {
                Failure::Failed { details, .. } => Failure::Failed {
                    message: format!(
                        "Bilibi commit {} is not in {}.",
                        short(&plan.platform_commit),
                        plan.platform_repository
                    ),
                    details,
                },
                other => other,
            })?;
        let mut clean = self.command("git");
        clean.current_dir(&src).args(["clean", "-fd"]);
        self.run(clean, "git clean of Bilibi").await?;
        let platform_tag = self.tag_at_head(&src).await;
        // Domains: remove what is not wanted, then clone or update each.
        let domains_dir = src.join("apps/domains");
        if let Ok(entries) = std::fs::read_dir(&domains_dir) {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if entry.path().is_dir()
                    && name != "src"
                    && !plan.domains.iter().any(|d| d.id == name)
                {
                    std::fs::remove_dir_all(entry.path()).map_err(|e| {
                        Failure::failed(format!("could not remove apps/domains/{name}: {e}"))
                    })?;
                }
            }
        }
        let mut fetched = Vec::new();
        for domain in &plan.domains {
            let path = domains_dir.join(&domain.id);
            let same_remote = match std::process::Command::new("git")
                .current_dir(&path)
                .args(["remote", "get-url", "origin"])
                .output()
            {
                Ok(output) if output.status.success() => {
                    String::from_utf8_lossy(&output.stdout).trim() == domain.repository
                }
                _ => false,
            };
            if path.join(".git").is_dir() && same_remote {
                self.say(format!("Updating {}", domain.id)).await;
                let mut fetch = self.command("git");
                fetch
                    .current_dir(&path)
                    .args(["fetch", "--tags", "--force", "--prune", "origin"]);
                self.run(fetch, &format!("git fetch of {}", domain.id))
                    .await?;
            } else {
                self.say(format!(
                    "Downloading {} from {}",
                    domain.id, domain.repository
                ))
                .await;
                let _ = std::fs::remove_dir_all(&path);
                let mut clone = self.command("git");
                clone.arg("clone").arg(&domain.repository).arg(&path);
                self.run(clone, &format!("git clone of {}", domain.id))
                    .await?;
            }
            let mut checkout = self.command("git");
            checkout
                .current_dir(&path)
                .args(["checkout", "-f", "--detach"])
                .arg(&domain.reference);
            if let Err(first) = self
                .run(checkout, &format!("git checkout of {}", domain.id))
                .await
            {
                if matches!(first, Failure::Cancelled) {
                    return Err(first);
                }
                // A branch name on an existing clone lives under origin/.
                let mut remote = self.command("git");
                remote
                    .current_dir(&path)
                    .args(["checkout", "-f", "--detach"])
                    .arg(format!("origin/{}", domain.reference));
                self.run(remote, &format!("git checkout of {}", domain.id))
                    .await
                    .map_err(|failure| match failure {
                        Failure::Failed { details, .. } => Failure::Failed {
                            message: format!(
                                "{} {} is not in {}.",
                                domain.id, domain.reference, domain.repository
                            ),
                            details,
                        },
                        other => other,
                    })?;
            }
            let mut clean = self.command("git");
            clean.current_dir(&path).args(["clean", "-fdx"]);
            self.run(clean, &format!("git clean of {}", domain.id))
                .await?;
            let head = self.head(&path).await?;
            let tag = self.tag_at_head(&path).await;
            let mut warning = None;
            if let Some(version) = &domain.version {
                if tag.as_deref() != Some(&format!("v{version}")) {
                    warning = Some(format!(
                        "{} {} is not tagged v{version} in its repository; the pin records the commit as it is.",
                        domain.id,
                        short(&head)
                    ));
                }
            }
            fetched.push(json!({"id": domain.id, "commit": head, "tag": tag, "version": domain.version, "repository": domain.repository, "warning": warning}));
        }
        // The Platform's own floor for the compiler.
        let needed = rust_version(&src.join("Cargo.toml"));
        let mut rust = self.toolchain.version().await.unwrap_or_default();
        if let Some(needed) = &needed {
            if !version_at_least(&rust, needed) {
                self.say(format!(
                    "Updating the Rust build tools to {needed} or newer"
                ))
                .await;
                rust = self.toolchain.update().await.map_err(Failure::failed)?;
                if !version_at_least(&rust, needed) {
                    return Err(Failure::failed(format!(
                        "Bilibi {} needs Rust {needed}, but the newest stable release is {rust}. Wait for that Rust release, or pick an older Bilibi.",
                        plan.platform_version
                    )));
                }
            }
        }
        let parts: Vec<String> = std::iter::once(format!(
            "Bilibi {} ({})",
            platform_tag
                .clone()
                .unwrap_or_else(|| plan.platform_version.clone()),
            short(&plan.platform_commit)
        ))
        .chain(fetched.iter().map(|d| {
            format!(
                "{} {} ({})",
                d["id"].as_str().unwrap_or_default(),
                d["tag"]
                    .as_str()
                    .map(str::to_owned)
                    .or_else(|| d["version"].as_str().map(str::to_owned))
                    .unwrap_or_else(|| "untagged".into()),
                short(d["commit"].as_str().unwrap_or_default())
            )
        }))
        .collect();
        let warnings: Vec<Value> = fetched
            .iter()
            .filter(|d| !d["warning"].is_null())
            .map(|d| d["warning"].clone())
            .collect();
        Ok(json!({
            "message": format!("Fetched {}", join_and(&parts)),
            "platform": {"commit": plan.platform_commit, "tag": platform_tag, "version": plan.platform_version, "repository": plan.platform_repository},
            "domains": fetched, "rust": rust, "warnings": warnings,
        }))
    }
    async fn head(&mut self, path: &Path) -> Outcome<String> {
        let mut command = self.command("git");
        command.current_dir(path).args(["rev-parse", "HEAD"]);
        Ok(self.run(command, "git rev-parse").await?.trim().to_owned())
    }
    /// The `vX.Y.Z` tag at HEAD, if one.
    async fn tag_at_head(&self, path: &Path) -> Option<String> {
        let output = std::process::Command::new("git")
            .current_dir(path)
            .args(["tag", "--points-at", "HEAD"])
            .output()
            .ok()?;
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .find(|tag| {
                tag.strip_prefix('v').is_some_and(|rest| {
                    rest.split('.').count() == 3
                        && rest
                            .split('.')
                            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
                })
            })
            .map(str::to_owned)
    }

    /// Step 3, "Build the new program": generate the composition, resolve
    /// and pin, compile a release into the kept cache, copy the program and
    /// its recipe into the next `releases/<n>`, and ask the new program what
    /// it is made of. The running instance is untouched.
    pub async fn build(&mut self) -> Outcome<Built> {
        let src = self.paths.src();
        let started = Instant::now();
        self.say("Preparing the composition").await;
        let mut discover = self.command("python3");
        discover
            .current_dir(&src)
            .arg("scripts/discover-modules.py");
        self.run(discover, "scripts/discover-modules.py").await?;
        let mut metadata = self.command("cargo");
        metadata
            .current_dir(&src)
            .args(["metadata", "--format-version", "1"]);
        let metadata = self.run(metadata, "cargo metadata").await?;
        let total = count_units(&metadata).unwrap_or(0);
        let mut pin = self.command("python3");
        pin.current_dir(&src)
            .args(["scripts/discover-modules.py", "--pin"]);
        let pinned = self.run(pin, "scripts/discover-modules.py --pin").await?;
        let history = History::read(&self.paths.history());
        let estimated = total;
        let total = if estimated > 0 {
            (estimated as f64 * history.scale().unwrap_or(1.0)).round() as u64
        } else {
            history.last_compiled().unwrap_or(400)
        };
        self.say(format!("Compiling about {total} parts ({})", pinned.trim()))
            .await;
        let counted = self.compile(total, history.rate()).await?;
        let seconds = started.elapsed().as_secs();
        // The measured pace is kept whatever happens to the copy below.
        history.push(
            &self.paths.history(),
            self.job,
            counted.compiled,
            counted.fresh,
            estimated,
            seconds,
        );
        // Copy the result beside the running program; a folder that could
        // not be completed is removed, so the next number is clean.
        self.say("Copying the new program into its release folder")
            .await;
        let releases = self.paths.releases();
        let release = next_release(&releases)?;
        let dir = releases.join(release.to_string());
        std::fs::create_dir(&dir)
            .map_err(|e| Failure::failed(format!("could not create {}: {e}", dir.display())))?;
        match self.package(&dir, release).await {
            Ok(status) => {
                let platform = json!({"version": self.plan.platform_version, "commit": self.plan.platform_commit});
                let domains = self
                    .plan
                    .domains
                    .iter()
                    .map(|d| json!({"id": d.id, "version": d.version, "reference": d.reference, "repository": d.repository}))
                    .collect();
                Ok(Built {
                    release,
                    path: dir,
                    seconds,
                    compiled: counted.compiled,
                    fresh: counted.fresh,
                    status,
                    platform,
                    domains,
                })
            }
            Err(failure) => {
                let _ = std::fs::remove_dir_all(&dir);
                Err(failure)
            }
        }
    }
    /// The program, its recipe (`manifest.txt`, `Cargo.lock`), `SHA256SUMS`
    /// and `release.json` into `dir`; then the new program's own `status`,
    /// which proves it starts. A release older than that command (before
    /// 0.1.1) answers with an error and is kept, with no status recorded;
    /// a program that cannot be run at all is a failure.
    async fn package(&mut self, dir: &Path, release: u64) -> Outcome<Option<String>> {
        let src = self.paths.src();
        copy_file(
            &self.paths.target().join("release/bilibi-web"),
            &dir.join("bilibi-web"),
            0o755,
        )?;
        for recipe in ["manifest.txt", "Cargo.lock"] {
            copy_file(
                &src.join(".local/composition").join(recipe),
                &dir.join(recipe),
                0o644,
            )?;
        }
        let mut checksum = self.command("sha256sum");
        checksum.current_dir(dir).arg("bilibi-web");
        let sums = self.run(checksum, "sha256sum").await?;
        std::fs::write(dir.join("SHA256SUMS"), sums)
            .map_err(|e| Failure::failed(format!("could not write SHA256SUMS: {e}")))?;
        let mut status = Command::new(dir.join("bilibi-web"));
        status
            .arg("status")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .process_group(0);
        let ran = tokio::time::timeout(Duration::from_secs(60), async {
            status
                .spawn()
                .map_err(|e| Failure::failed(format!("The new program was built but cannot be run ({e}); nothing was put in service.")))?
                .wait_with_output()
                .await
                .map_err(|e| Failure::failed(format!("The new program was built but did not finish its status check: {e}")))
        })
        .await
        .map_err(|_| Failure::failed("The new program was built but its status check did not finish within a minute; nothing was put in service."))??;
        use std::os::unix::process::ExitStatusExt;
        if ran.status.signal().is_some() {
            return Err(Failure::with(
                format!(
                    "The new program was built but stops with {}; nothing was put in service.",
                    describe_exit(&ran.status)
                ),
                String::from_utf8_lossy(&ran.stderr).into_owned(),
            ));
        }
        let status = ran
            .status
            .success()
            .then(|| String::from_utf8_lossy(&ran.stdout).trim().to_owned());
        let recipe = json!({
            "job": self.job, "release": release,
            "platform": {"version": self.plan.platform_version, "commit": self.plan.platform_commit},
            "domains": self.plan.domains.iter().map(|d| json!({"id": d.id, "version": d.version, "reference": d.reference, "repository": d.repository})).collect::<Vec<_>>(),
            "status": status,
        });
        std::fs::write(
            dir.join("release.json"),
            serde_json::to_vec_pretty(&recipe).unwrap_or_default(),
        )
        .map_err(|e| Failure::failed(format!("could not write release.json: {e}")))?;
        Ok(status)
    }
    /// `cargo build --release --locked -p bilibi-web`, counting the parts
    /// Cargo reports and saying how many are left and about how long.
    async fn compile(&mut self, total: u64, known_rate: Option<f64>) -> Outcome<Counted> {
        let src = self.paths.src();
        let log = self
            .log_file()
            .map_err(|e| Failure::failed(format!("could not open the build log: {e}")))?;
        let mut command = self.command("cargo");
        command
            .current_dir(&src)
            .args([
                "build",
                "--release",
                "--locked",
                "-p",
                "bilibi-web",
                "--message-format",
                "json-render-diagnostics",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log));
        let mut child = command
            .spawn()
            .map_err(|e| Failure::failed(format!("could not start cargo: {e}")))?;
        let pid = child.id();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Failure::failed("cargo gave no output"))?;
        let mut lines = BufReader::new(stdout).lines();
        let mut counted = Counted::default();
        let mut errors: Vec<String> = Vec::new();
        let started = Instant::now();
        let mut first_compile: Option<Instant> = None;
        let mut last_report = Instant::now() - PROGRESS_EVERY;
        loop {
            // A big crate says nothing for minutes, so the count is reported
            // on a timer too, not only when Cargo prints a line.
            let next = tokio::select! {
                line = lines.next_line() => Some(line),
                () = self.cancelled() => {
                    stop_group(pid).await;
                    return Err(Failure::Cancelled);
                }
                () = tokio::time::sleep(PROGRESS_EVERY) => None,
            };
            let line = match next {
                Some(Ok(Some(line))) => Some(line),
                Some(Ok(None)) => break,
                Some(Err(e)) => return Err(Failure::failed(format!("lost cargo's output: {e}"))),
                None => None,
            };
            let message = line
                .as_deref()
                .and_then(|line| serde_json::from_str::<Value>(line).ok())
                .unwrap_or(Value::Null);
            match message["reason"].as_str() {
                Some("compiler-artifact") => {
                    counted.compiled += 1;
                    if message["fresh"].as_bool().unwrap_or(false) {
                        counted.fresh += 1;
                    } else if first_compile.is_none() {
                        first_compile = Some(Instant::now());
                    }
                }
                Some("compiler-message")
                    if message["message"]["level"].as_str() == Some("error") =>
                {
                    if let Some(text) = message["message"]["rendered"].as_str() {
                        errors.push(text.to_owned());
                    }
                }
                _ => {}
            }
            if last_report.elapsed() >= PROGRESS_EVERY {
                last_report = Instant::now();
                let total = total.max(counted.compiled);
                let built_here = counted.compiled.saturating_sub(counted.fresh);
                // The first parts are small and quick; trust the measured
                // pace only once a fifth of the parts are behind us.
                let rate = match first_compile {
                    Some(at) if built_here >= (total / 5).max(20) => {
                        at.elapsed().as_secs_f64() / built_here as f64
                    }
                    _ => known_rate.unwrap_or(DEFAULT_SECONDS_PER_PART),
                };
                let left = ((total - counted.compiled) as f64 * rate).round() as u64;
                let cached = if counted.fresh > 0 {
                    format!(" ({} from the cache)", counted.fresh)
                } else {
                    String::new()
                };
                let message = if counted.compiled >= total {
                    format!(
                        "Compiled {} parts{cached}, finishing the last ones",
                        counted.compiled
                    )
                } else {
                    format!(
                        "Compiled {} of about {total} parts{cached}, about {} left",
                        counted.compiled,
                        minutes(left)
                    )
                };
                let _ = self
                    .progress
                    .send(json!({
                        "message": message,
                        "compiled": counted.compiled, "total": total, "fresh": counted.fresh,
                        "seconds_left": left, "seconds": started.elapsed().as_secs(),
                    }))
                    .await;
            }
        }
        let status = tokio::select! {
            status = child.wait() => status.map_err(|e| Failure::failed(format!("cargo did not finish: {e}")))?,
            () = self.cancelled() => {
                stop_group(pid).await;
                return Err(Failure::Cancelled);
            }
        };
        if status.success() {
            return Ok(counted);
        }
        let log_tail = std::fs::read_to_string(self.log_path())
            .map(|text| tail(&text, 30))
            .unwrap_or_default();
        let details = if errors.is_empty() {
            log_tail
        } else {
            errors.join("\n")
        };
        use std::os::unix::process::ExitStatusExt;
        let message = if status.signal() == Some(libc::SIGKILL) || status.code() == Some(137) {
            "The compiler was stopped because it used more memory than this machine allows (exit 137). Build with fewer Domains, or on a machine with more memory.".to_owned()
        } else if !errors.is_empty() {
            format!(
                "The new program does not compile ({} error{}). The chosen versions may not work together; pick a set the catalogue marks as tested.",
                errors.len(),
                if errors.len() == 1 { "" } else { "s" }
            )
        } else {
            format!("cargo build failed ({}).", describe_exit(&status))
        };
        Err(Failure::with(message, details))
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct Counted {
    compiled: u64,
    fresh: u64,
}

/// Stops a whole process group (cargo and its compilers): politely, then not.
async fn stop_group(pid: Option<u32>) {
    let Some(pid) = pid.and_then(|pid| i32::try_from(pid).ok()) else {
        return;
    };
    // SAFETY: a signal to a process group this process created.
    unsafe {
        libc::kill(-pid, libc::SIGTERM);
    }
    tokio::time::sleep(Duration::from_secs(3)).await;
    unsafe {
        libc::kill(-pid, libc::SIGKILL);
    }
}
fn describe_exit(status: &std::process::ExitStatus) -> String {
    use std::os::unix::process::ExitStatusExt;
    match (status.code(), status.signal()) {
        (Some(code), _) => format!("exit {code}"),
        (None, Some(signal)) => format!("signal {signal}"),
        _ => "unknown exit".into(),
    }
}

/// The parts a release build of `bilibi-web` compiles, as Cargo will report
/// them: every package reachable through normal and build dependencies, and
/// each build script once. Dev-only dependencies are left out. An estimate,
/// not a count: dependencies of other platforms are included, which the
/// measured ratio of the last build (`History::scale`) corrects.
fn count_units(metadata: &str) -> Option<u64> {
    let metadata: Value = serde_json::from_str(metadata).ok()?;
    let packages = metadata["packages"].as_array()?;
    let root = packages
        .iter()
        .find(|p| p["name"].as_str() == Some("bilibi-web"))?["id"]
        .as_str()?
        .to_owned();
    let mut deps_of = std::collections::HashMap::new();
    for node in metadata["resolve"]["nodes"].as_array()? {
        let id = node["id"].as_str()?.to_owned();
        let deps: Vec<String> = node["deps"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .filter(|dep| {
                dep["dep_kinds"]
                    .as_array()
                    .map(Vec::as_slice)
                    .unwrap_or_default()
                    .iter()
                    .any(|kind| {
                        kind["kind"].as_str() != Some("dev")
                            && kind["target"].as_str().is_none_or(built_on_linux)
                    })
            })
            .filter_map(|dep| dep["pkg"].as_str().map(str::to_owned))
            .collect();
        deps_of.insert(id, deps);
    }
    let mut seen = std::collections::HashSet::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        if !seen.insert(id.clone()) {
            continue;
        }
        if let Some(deps) = deps_of.get(&id) {
            stack.extend(deps.iter().cloned());
        }
    }
    let mut units = 0;
    for package in packages {
        let Some(id) = package["id"].as_str() else {
            continue;
        };
        if !seen.contains(id) {
            continue;
        }
        for target in package["targets"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_default()
        {
            let kinds = target["kind"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default();
            // A library, a program or a build script is one part each;
            // tests, examples and benches are not built.
            if kinds.iter().any(|k| {
                matches!(
                    k.as_str(),
                    Some(
                        "custom-build"
                            | "lib"
                            | "rlib"
                            | "proc-macro"
                            | "bin"
                            | "dylib"
                            | "cdylib"
                            | "staticlib"
                    )
                )
            }) {
                units += 1;
            }
        }
    }
    Some(units)
}

/// Whether a dependency's `cfg(...)` can apply to the Linux server this
/// runs on: a rough reading that keeps `unix` and `linux` conditions and
/// the negations of others, and drops `windows`, `wasm` and the like.
fn built_on_linux(target: &str) -> bool {
    let names_us = target.contains("unix") || target.contains("linux");
    names_us || target.starts_with("cfg(not(")
}

/// The builds made here, so the next estimate starts from measured time.
#[derive(Default)]
struct History {
    builds: Vec<Value>,
}
impl History {
    fn read(path: &Path) -> Self {
        let builds = std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .and_then(|v| v["builds"].as_array().cloned())
            .unwrap_or_default();
        Self { builds }
    }
    fn last_compiled(&self) -> Option<u64> {
        self.builds.last().and_then(|b| b["compiled"].as_u64())
    }
    /// Parts Cargo really reported over parts estimated, from the newest
    /// build: the estimate counts dependencies of other platforms too.
    fn scale(&self) -> Option<f64> {
        self.builds.iter().rev().find_map(|b| {
            let (units, compiled) = (b["units"].as_u64()?, b["compiled"].as_u64()?);
            (units > 0 && compiled > 0).then(|| compiled as f64 / units as f64)
        })
    }
    /// Seconds per part of the newest build, cached parts included: the
    /// pace to expect from a build like the last one.
    fn rate(&self) -> Option<f64> {
        self.builds.last().and_then(|b| b["rate"].as_f64())
    }
    /// `units` is the estimate before any scaling, `compiled` what Cargo
    /// reported, `rate` the seconds per reported part.
    fn push(mut self, path: &Path, job: i64, compiled: u64, fresh: u64, units: u64, seconds: u64) {
        self.builds.push(json!({
            "job": job, "units": units, "compiled": compiled, "fresh": fresh, "seconds": seconds,
            "rate": seconds as f64 / compiled.max(1) as f64,
        }));
        let start = self.builds.len().saturating_sub(HISTORY_KEPT);
        let kept = json!({"builds": self.builds[start..]});
        let _ = std::fs::write(path, serde_json::to_vec_pretty(&kept).unwrap_or_default());
    }
}

/// Highest numbered folder plus one; the install script numbers the same way.
fn next_release(releases: &Path) -> Outcome<u64> {
    let highest = std::fs::read_dir(releases)
        .map_err(|e| Failure::failed(format!("cannot read {}: {e}", releases.display())))?
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().to_string_lossy().parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    Ok(highest + 1)
}
fn copy_file(from: &Path, to: &Path, mode: u32) -> Outcome<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::copy(from, to).map_err(|e| {
        Failure::failed(format!(
            "could not copy {} to {}: {e}",
            from.display(),
            to.display()
        ))
    })?;
    std::fs::set_permissions(to, std::fs::Permissions::from_mode(mode)).map_err(|e| {
        Failure::failed(format!(
            "could not set permissions on {}: {e}",
            to.display()
        ))
    })?;
    Ok(())
}
/// `rust-version = "1.99"` under `[workspace.package]`.
fn rust_version(cargo_toml: &Path) -> Option<String> {
    std::fs::read_to_string(cargo_toml)
        .ok()?
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("rust-version")
                .and_then(|rest| rest.trim().strip_prefix('='))
                .map(|rest| rest.trim().trim_matches('"').to_owned())
        })
}

struct Memory {
    total: u64,
    swap: u64,
}
impl Memory {
    fn read() -> Outcome<Self> {
        let text = std::fs::read_to_string("/proc/meminfo")
            .map_err(|e| Failure::failed(format!("cannot read memory size: {e}")))?;
        let kib = |key: &str| -> u64 {
            text.lines()
                .find_map(|line| line.strip_prefix(key))
                .and_then(|rest| {
                    rest.trim()
                        .trim_start_matches(':')
                        .split_whitespace()
                        .next()?
                        .parse::<u64>()
                        .ok()
                })
                .unwrap_or(0)
        };
        Ok(Self {
            total: kib("MemTotal:") * 1024,
            swap: kib("SwapTotal:") * 1024,
        })
    }
}
/// Free bytes on the file system holding `path`, for an unprivileged user.
fn free_space(path: &Path) -> Outcome<u64> {
    use std::os::unix::ffi::OsStrExt;
    let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Failure::failed("a path holds a NUL byte"))?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    // SAFETY: a valid C string and a zeroed statvfs the call fills in.
    let code = unsafe { libc::statvfs(c_path.as_ptr(), &mut stats) };
    if code != 0 {
        return Err(Failure::failed(format!(
            "cannot read free space at {}: {}",
            path.display(),
            std::io::Error::last_os_error()
        )));
    }
    Ok(stats.f_bavail as u64 * stats.f_frsize as u64)
}
fn gb(bytes: u64) -> String {
    format!("{:.0} GB", bytes as f64 / GB as f64)
}
/// "7 minutes", "about 40 seconds", "1 minute".
fn minutes(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds} seconds"),
        60..=119 => "1 minute".into(),
        _ => format!("{} minutes", (seconds + 30) / 60),
    }
}
