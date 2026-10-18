//! The Rust toolchain the builder compiles with. It lives in the builder's
//! own home (`~/.cargo`, `~/.rustup`, as rustup lays it out), so on a server
//! it belongs to `bilibi-build` and never to the account that serves pages.
//! "Prepare this server for builds" installs it once; the first build does
//! the same when nobody asked beforehand. This is the Rust half of
//! `scripts/setup-steps/20-rust.sh` for a user that has no checkout.
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
};
use tokio::process::Command;

const RUSTUP_INIT: &str = "https://sh.rustup.rs";

pub struct Toolchain {
    home: PathBuf,
}
impl Toolchain {
    /// The toolchain under `home` (the builder user's `$HOME`).
    pub fn in_home(home: &Path) -> Self {
        Self {
            home: home.to_path_buf(),
        }
    }
    pub fn bin_dir(&self) -> PathBuf {
        self.home.join(".cargo/bin")
    }
    /// `PATH` with the toolchain first, for every command the builder runs.
    pub fn path_env(&self) -> OsString {
        let mut path = self.bin_dir().into_os_string();
        if let Some(rest) = std::env::var_os("PATH") {
            path.push(":");
            path.push(rest);
        }
        path
    }
    /// A command that sees only what a build needs: the toolchain on `PATH`,
    /// the builder's `$HOME` and a locale. The instance's secrets
    /// (`DATABASE_URL`, the `BILIBI_*` keys) reach neither Git, Cargo, nor a
    /// Domain's build script, whatever the worker itself was started with.
    pub fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", self.path_env())
            .env("HOME", &self.home)
            .env("LANG", "C.UTF-8")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("CARGO_TERM_COLOR", "never");
        command
    }
    /// `Some("1.99.0")` when `rustc` runs from this home.
    pub async fn version(&self) -> Option<String> {
        let output = self
            .command(self.bin_dir().join("rustc"))
            .arg("--version")
            .output()
            .await
            .ok()?;
        if !output.status.success() {
            return None;
        }
        String::from_utf8_lossy(&output.stdout)
            .split_whitespace()
            .nth(1)
            .map(str::to_owned)
    }
    /// Installs rustup with the stable channel (minimal profile) into the
    /// home, downloading the installer into `scratch`. About 1.5 GB.
    pub async fn install(&self, scratch: &Path) -> Result<String, String> {
        std::fs::create_dir_all(scratch)
            .map_err(|e| format!("cannot create {}: {e}", scratch.display()))?;
        let script = scratch.join("rustup-init.sh");
        let fetched = self
            .command("curl")
            .args(["-fsSL", "--proto", "=https", "--tlsv1.2", RUSTUP_INIT, "-o"])
            .arg(&script)
            .output()
            .await
            .map_err(|e| {
                format!("curl is not installed ({e}); ask the server owner to install curl")
            })?;
        if !fetched.status.success() {
            return Err(format!(
                "could not download the Rust installer from {RUSTUP_INIT}: {}",
                String::from_utf8_lossy(&fetched.stderr).trim()
            ));
        }
        let installed = self
            .command("sh")
            .arg(&script)
            .args([
                "-y",
                "--profile",
                "minimal",
                "--default-toolchain",
                "stable",
                "--no-modify-path",
            ])
            .output()
            .await
            .map_err(|e| format!("could not run the Rust installer: {e}"))?;
        let _ = std::fs::remove_file(&script);
        if !installed.status.success() {
            return Err(format!(
                "the Rust installer failed: {}",
                tail(&String::from_utf8_lossy(&installed.stderr), 12)
            ));
        }
        self.version()
            .await
            .ok_or_else(|| "rustc does not run after the install".to_owned())
    }
    /// Moves the stable channel forward, for a Platform whose `rust-version`
    /// is newer than what is installed.
    pub async fn update(&self) -> Result<String, String> {
        let updated = self
            .command(self.bin_dir().join("rustup"))
            .args(["toolchain", "install", "stable", "--profile", "minimal"])
            .output()
            .await
            .map_err(|e| format!("rustup does not run: {e}"))?;
        if !updated.status.success() {
            return Err(format!(
                "rustup could not update the stable toolchain: {}",
                tail(&String::from_utf8_lossy(&updated.stderr), 12)
            ));
        }
        self.version()
            .await
            .ok_or_else(|| "rustc does not run after the update".to_owned())
    }
}

/// `current >= needed` for dotted versions (`1.99.0` against `1.99`).
pub fn version_at_least(current: &str, needed: &str) -> bool {
    let parse = |text: &str| -> Vec<u64> {
        text.split('.')
            .map(|part| part.trim().parse().unwrap_or(0))
            .collect()
    };
    let (mut have, mut need) = (parse(current), parse(needed));
    let width = have.len().max(need.len());
    have.resize(width, 0);
    need.resize(width, 0);
    have >= need
}

/// The last `lines` lines of a command's output, for a plain failure detail.
pub fn tail(text: &str, lines: usize) -> String {
    let all: Vec<&str> = text.lines().collect();
    let start = all.len().saturating_sub(lines);
    all[start..].join("\n")
}
