//! One instance's folder under `/opt/bilibi`, as `scripts/install-server.sh`
//! lays it out (`docs/server-install.md`): `env`, `releases/<n>`, the
//! `current` link, `backups/` and `data/`.
use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

/// The files the installer copies with a release, when the build wrote them.
const RELEASE_FILES: [&str; 4] = ["bilibi-web", "SHA256SUMS", "manifest.txt", "Cargo.lock"];

#[derive(Clone, Debug)]
pub struct Layout {
    /// `/opt/bilibi`
    pub root: PathBuf,
    /// The instance's folder name: `production`, `dev`.
    pub name: String,
    /// `/opt/bilibi/<name>`
    pub dir: PathBuf,
}
impl Layout {
    /// The instance whose folder this is. The folder must hold an `env`
    /// file, which only `install-server.sh create` writes.
    pub fn open(dir: &Path) -> Result<Self, String> {
        let dir = dir
            .canonicalize()
            .map_err(|error| format!("{}: {error}", dir.display()))?;
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| valid_instance_name(name))
            .ok_or_else(|| {
                format!(
                    "{} is not an instance folder: the name must be 1 to 32 lower-case letters, digits or hyphens",
                    dir.display()
                )
            })?
            .to_owned();
        let root = dir
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", dir.display()))?
            .to_path_buf();
        if !dir.join("env").is_file() {
            return Err(format!(
                "{} has no env file; create the instance with scripts/install-server.sh create {name} first",
                dir.display()
            ));
        }
        Ok(Self { root, name, dir })
    }
    /// Another instance on this machine, by its folder name.
    pub fn sibling(&self, name: &str) -> Result<Self, String> {
        if !valid_instance_name(name) {
            return Err(format!("{name} is not an instance name"));
        }
        Self::open(&self.root.join(name))
    }
    pub fn releases(&self) -> PathBuf {
        self.dir.join("releases")
    }
    pub fn release(&self, n: u64) -> PathBuf {
        self.releases().join(n.to_string())
    }
    pub fn backups(&self) -> PathBuf {
        self.dir.join("backups")
    }
    /// The kept release numbers, lowest first.
    pub fn release_numbers(&self) -> Vec<u64> {
        let mut numbers: Vec<u64> = fs::read_dir(self.releases())
            .map(|entries| {
                entries
                    .flatten()
                    .filter(|entry| entry.path().is_dir())
                    .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        numbers.sort_unstable();
        numbers
    }
    /// The release in service, if any: `current` is a fixed link to
    /// `releases/current`, which points at the release's number.
    pub fn current(&self) -> Option<u64> {
        let target = fs::canonicalize(self.dir.join("current")).ok()?;
        target.file_name()?.to_str()?.parse().ok()
    }
    pub fn next_release(&self) -> u64 {
        self.release_numbers().last().map_or(1, |last| last + 1)
    }
    /// Points the release in service at `releases/<n>` in one step: a new
    /// `releases/current` link is made beside the old one and renamed over
    /// it, so there is never a moment without one. The instance folder and
    /// its `current` link belong to root; `releases/` belongs to this user,
    /// which is why the moving link lives there.
    pub fn point_current(&self, n: u64) -> io::Result<()> {
        if !self.converted() {
            return Err(io::Error::other(format!(
                "{} must be a link to releases/current (rerun scripts/install-server.sh create {} to convert it)",
                self.dir.join("current").display(),
                self.name
            )));
        }
        let fresh = self.releases().join("current.new");
        let _ = fs::remove_file(&fresh);
        std::os::unix::fs::symlink(n.to_string(), &fresh)?;
        fs::rename(&fresh, self.releases().join("current"))
    }
    /// Whether `current` is the fixed link to `releases/current`, the shape
    /// the installer can move. An older instance is converted by
    /// `scripts/install-server.sh installer` or `create`.
    pub fn converted(&self) -> bool {
        fs::read_link(self.dir.join("current"))
            .is_ok_and(|link| link == Path::new("releases/current"))
    }
    /// What to tell the owner when `converted` is false.
    pub fn conversion_advice(&self) -> String {
        format!(
            "{} still has the older folder layout, so a release cannot be switched here. Run `sudo scripts/install-server.sh installer <bilibi-installer>` (or `create {}`) on the server to convert it, then start again.",
            self.name, self.name
        )
    }
    /// One value from the instance's `env` file (`BIND_ADDR`, `APP_ORIGIN`).
    pub fn env_value(&self, key: &str) -> Option<String> {
        let text = fs::read_to_string(self.dir.join("env")).ok()?;
        text.lines()
            .filter_map(|line| line.split_once('='))
            .find(|(name, _)| name.trim() == key)
            .map(|(_, value)| value.trim().to_owned())
    }
    /// The local port the instance's program listens on, from `BIND_ADDR`.
    pub fn port(&self) -> Option<u16> {
        let bind = self.env_value("BIND_ADDR")?;
        bind.rsplit_once(':')?.1.parse().ok()
    }
    /// The systemd unit that runs this instance.
    pub fn unit(&self) -> String {
        format!("bilibi@{}", self.name)
    }
}

/// The same rule `scripts/install-server.sh` and `bilibi_system` apply.
pub fn valid_instance_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !name.starts_with('-')
}

/// Checks `SHA256SUMS` in a release folder and returns the program's
/// checksum. Uses the system's `sha256sum`, the same tool that wrote the file.
pub fn verify_checksum(dir: &Path) -> Result<String, String> {
    let sums = fs::read_to_string(dir.join("SHA256SUMS"))
        .map_err(|error| format!("{} has no SHA256SUMS ({error})", dir.display()))?;
    let sha = sums
        .lines()
        .find_map(|line| {
            let (sha, name) = line.split_once(char::is_whitespace)?;
            (name.trim_start_matches('*').trim() == "bilibi-web" && sha.len() == 64)
                .then(|| sha.to_owned())
        })
        .ok_or_else(|| {
            format!(
                "{} does not name bilibi-web",
                dir.join("SHA256SUMS").display()
            )
        })?;
    let checked = Command::new("sha256sum")
        .args(["-c", "--quiet", "SHA256SUMS"])
        .current_dir(dir)
        .output()
        .map_err(|error| format!("could not run sha256sum: {error}"))?;
    if !checked.status.success() {
        return Err(format!(
            "the program in {} does not match its checksum",
            dir.display()
        ));
    }
    Ok(sha)
}

/// Copies a release folder's files into `dst` and returns the program's size.
pub fn copy_release(src: &Path, dst: &Path) -> io::Result<u64> {
    fs::create_dir_all(dst)?;
    fs::set_permissions(dst, fs::Permissions::from_mode(0o755))?;
    let mut bytes = 0;
    for name in RELEASE_FILES {
        let from = src.join(name);
        if !from.is_file() {
            continue;
        }
        let to = dst.join(name);
        let copied = fs::copy(&from, &to)?;
        let mode = if name == "bilibi-web" { 0o755 } else { 0o644 };
        fs::set_permissions(&to, fs::Permissions::from_mode(mode))?;
        if name == "bilibi-web" {
            bytes = copied;
        }
    }
    Ok(bytes)
}
/// Free space on the file system holding `path`, in bytes.
pub fn free_bytes(path: &Path) -> Option<u64> {
    let out = Command::new("df")
        .args(["--output=avail", "-B1"])
        .arg(path)
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .nth(1)?
        .trim()
        .parse()
        .ok()
}

/// Bytes as "1.2 GB" or "340 MB".
pub fn human(bytes: u64) -> String {
    const GB: f64 = 1_073_741_824.0;
    const MB: f64 = 1_048_576.0;
    let bytes = bytes as f64;
    if bytes >= GB {
        format!("{:.1} GB", bytes / GB)
    } else {
        format!("{:.0} MB", (bytes / MB).max(1.0))
    }
}
