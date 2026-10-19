//! How a release travels from the instance that tested it to the one that
//! installs it (`README.md`, "Release transport"). Three ways in, one shape
//! out: a release folder (`bilibi-web`, `SHA256SUMS`, `manifest.txt`,
//! `Cargo.lock`, `release.json`, and `plugins/` when the release has any)
//! that the installer copies and checks like a sibling instance's.
//!
//! - **Same machine.** The installer copies the sibling's folder itself; this
//!   module only lists what the sibling has.
//! - **HTTPS pull.** The target downloads the release file from its source's
//!   `/transport/releases/<n>` with the pairing key, unpacks it into
//!   `incoming/job-<id>/` and lets the installer take it from there.
//! - **A file.** The owner downloads the release file on the source and
//!   uploads it on the target, which unpacks it into `incoming/upload-<n>/`.
//!
//! The release file is a gzip-compressed tar of the folder plus `bundle.json`,
//! which names the instance it came from. This module holds what needs no
//! database: the folders, the file, and the checks every way in shares.
use bilibi_module_registry::{Composition, Layer};
use bilibi_system::Instance;
use flate2::{read::GzDecoder, write::GzEncoder, Compression};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{self, Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Component, Path, PathBuf},
    process::Command,
};

/// The header a pulling instance presents (`README.md`, "Pairing").
pub const PAIRING_HEADER: &str = "x-bilibi-pairing-key";
/// Where a source lists its releases, under its origin.
pub const LISTING_PATH: &str = "/transport/releases";
/// The media type of a release file.
pub const BUNDLE_TYPE: &str = "application/gzip";
/// The files of a release folder, in the order the file carries them. A
/// release put in by `install-server.sh release` has only the first two.
pub const RELEASE_FILES: [&str; 6] = [
    "bilibi-web",
    "SHA256SUMS",
    "manifest.txt",
    "Cargo.lock",
    "release.json",
    "bundle.json",
];
/// Plug-ins that travel with the release live here, inside the folder.
pub const PLUGINS_DIR: &str = "plugins";
/// The plug-ins the release switches on, a list of `{id, name, publisher,
/// version, source}` the builder writes in `PLUGINS_DIR`.
pub const PLUGINS_ENABLED: &str = "enabled.json";
/// A release file larger than this is refused unread: a program is tens of
/// megabytes, never gigabytes.
pub const MAX_BUNDLE_BYTES: u64 = 1024 * 1024 * 1024;
/// Prefixes of the folders under `incoming/`.
pub const JOB_PREFIX: &str = "job-";
pub const UPLOAD_PREFIX: &str = "upload-";

/// The path of release `n`'s file under the source's origin.
pub fn bundle_path(release: u64) -> String {
    format!("{LISTING_PATH}/{release}")
}
/// The name a browser saves a release file under.
pub fn bundle_filename(release: u64) -> String {
    format!("bilibi-release-{release}.tar.gz")
}

/// The instance folder this program serves (`BILIBI_INSTANCE_DIR`, from the
/// instance's `env` file): where its releases are kept, and where a release
/// on its way in waits for the installer.
#[derive(Clone, Debug)]
pub struct Paths {
    /// `/opt/bilibi`
    pub root: PathBuf,
    /// The folder name: `dev`, `production`.
    pub name: String,
    /// `/opt/bilibi/<name>`
    pub dir: PathBuf,
}
impl Paths {
    pub const INSTANCE_DIR_KEY: &'static str = "BILIBI_INSTANCE_DIR";
    /// `None` when the program runs without an instance folder (a developer
    /// run): then there are no releases here and nothing can be received.
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var_os(Self::INSTANCE_DIR_KEY)
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)?;
        Self::open(&dir).ok()
    }
    pub fn open(dir: &Path) -> Result<Self, String> {
        let dir = dir
            .canonicalize()
            .map_err(|error| format!("{}: {error}", dir.display()))?;
        let name = dir
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| bilibi_system::valid_instance_name(name))
            .ok_or_else(|| format!("{} is not an instance folder", dir.display()))?
            .to_owned();
        let root = dir
            .parent()
            .ok_or_else(|| format!("{} has no parent folder", dir.display()))?
            .to_path_buf();
        Ok(Self { root, name, dir })
    }
    /// Another instance on this machine, by its folder name.
    pub fn sibling(&self, name: &str) -> Result<Self, String> {
        if !bilibi_system::valid_instance_name(name) {
            return Err(format!("{name} is not an instance name"));
        }
        let dir = self.root.join(name);
        if !dir.is_dir() {
            return Err(format!(
                "there is no instance called {name} on this machine ({} does not exist)",
                dir.display()
            ));
        }
        Self::open(&dir)
    }
    pub fn releases(&self) -> PathBuf {
        self.dir.join("releases")
    }
    pub fn release(&self, n: u64) -> PathBuf {
        self.releases().join(n.to_string())
    }
    /// Where a release on its way in waits: written by the program (user
    /// `bilibi`), read and removed by the installer (`bilibi-build`).
    pub fn incoming(&self) -> PathBuf {
        self.dir.join("incoming")
    }
    pub fn incoming_folder(&self, name: &str) -> PathBuf {
        self.incoming().join(name)
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
    /// The release in service: `current` resolves to `releases/<n>`.
    pub fn current(&self) -> Option<u64> {
        let target = fs::canonicalize(self.dir.join("current")).ok()?;
        target.file_name()?.to_str()?.parse().ok()
    }
    /// The uploaded release files waiting here, oldest first, as
    /// (folder name, facts).
    pub fn uploads(&self) -> Vec<(String, Value)> {
        let mut found: Vec<(u64, String)> = fs::read_dir(self.incoming())
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        let name = entry.file_name().to_str()?.to_owned();
                        let n: u64 = name.strip_prefix(UPLOAD_PREFIX)?.parse().ok()?;
                        entry.path().is_dir().then_some((n, name))
                    })
                    .collect()
            })
            .unwrap_or_default();
        found.sort_unstable();
        found
            .into_iter()
            .map(|(_, name)| {
                let facts = folder_facts(&self.incoming_folder(&name));
                (name, facts)
            })
            .collect()
    }
    /// The next free `upload-<n>` folder name.
    pub fn next_upload(&self) -> String {
        let last = fs::read_dir(self.incoming())
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| {
                        entry
                            .file_name()
                            .to_str()?
                            .strip_prefix(UPLOAD_PREFIX)?
                            .parse::<u64>()
                            .ok()
                    })
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        format!("{UPLOAD_PREFIX}{}", last + 1)
    }
}

/// Whether `name` is a folder name under `incoming/` this module made:
/// `job-<n>` or `upload-<n>`, nothing that could leave the folder.
pub fn valid_incoming_name(name: &str) -> bool {
    let digits = |rest: &str| {
        !rest.is_empty() && rest.len() <= 12 && rest.bytes().all(|b| b.is_ascii_digit())
    };
    name.strip_prefix(JOB_PREFIX)
        .or_else(|| name.strip_prefix(UPLOAD_PREFIX))
        .is_some_and(digits)
}

/// What a release folder says about itself: its program's checksum and
/// size, and the recipe the builder wrote (`release.json`) with the
/// `bundle.json` of a file that was unpacked here. Null fields mean the
/// folder lacks that file.
pub fn folder_facts(dir: &Path) -> Value {
    let read_json = |name: &str| -> Value {
        fs::read_to_string(dir.join(name))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Value::Null)
    };
    let recipe = read_json("release.json");
    let bundle = read_json("bundle.json");
    let bytes = fs::metadata(dir.join("bilibi-web"))
        .map(|meta| meta.len())
        .unwrap_or(0);
    let plugins = fs::read_to_string(dir.join(PLUGINS_DIR).join(PLUGINS_ENABLED))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .filter(Value::is_array)
        .unwrap_or(Value::Null);
    json!({
        "sha256": expected_checksum(dir).ok(),
        "bytes": bytes,
        "platform": recipe["platform"].as_object().map(|p| json!({"version": p.get("version"), "commit": p.get("commit")})),
        "domains": recipe["domains"].as_array().map(|list| list.iter().map(|d| json!({"id": d["id"], "version": d["version"], "commit": d["reference"]})).collect::<Vec<_>>()),
        "built_for": recipe["built_for"].clone(),
        "release": bundle["release"].as_u64().or_else(|| recipe["release"].as_u64()),
        "from": bundle["from"].clone(),
        "program": recipe["status"].as_str().and_then(|s| s.lines().next()).map(|s| s.chars().take(200).collect::<String>()),
        "plugins": plugins,
    })
}

/// A kept release as the source lists it. The release in service answers
/// with what the running program says it is made of, which cannot lie; the
/// others with their recipe.
pub fn release_json(
    paths: &Paths,
    n: u64,
    in_service: bool,
    running: Option<&Composition>,
) -> Value {
    let dir = paths.release(n);
    let mut facts = folder_facts(&dir);
    facts["release"] = json!(n);
    facts["in_service"] = json!(in_service);
    if let (true, Some(made)) = (in_service, running) {
        facts["platform"] =
            json!({"version": made.version, "release": made.tag, "commit": made.commit});
        facts["domains"] = json!(made
            .containers
            .iter()
            .filter(|c| c.layer == Layer::Domains)
            .map(|c| json!({"id": c.id, "version": c.version, "commit": c.commit}))
            .collect::<Vec<_>>());
    }
    facts
}

/// What a source answers at `/transport/releases`: which instance it is,
/// the release in service, and every kept release with its checksum.
pub fn listing(paths: &Paths, instance: &Instance, running: &Composition) -> Value {
    let current = paths.current();
    let releases: Vec<Value> = paths
        .release_numbers()
        .into_iter()
        .rev()
        .filter(|n| paths.release(*n).join("bilibi-web").is_file())
        .map(|n| release_json(paths, n, current == Some(n), Some(running)))
        .collect();
    json!({
        "format": 1,
        "instance": {"label": instance.label, "environment": instance.role.env_value(), "name": paths.name},
        "current": current,
        "healthy": true,
        "releases": releases,
    })
}

// ------------------------------------------------------------- the file

/// Writes release `n`'s file: the folder's files and `plugins/`, plus a
/// `bundle.json` naming where it came from, as a gzip-compressed tar.
pub fn write_bundle(dir: &Path, release: u64, from: &Value, sink: impl Write) -> io::Result<()> {
    let sha256 = expected_checksum(dir).map_err(io::Error::other)?;
    let encoder = GzEncoder::new(sink, Compression::fast());
    let mut tar = tar::Builder::new(encoder);
    tar.mode(tar::HeaderMode::Deterministic);
    for name in RELEASE_FILES {
        if name == "bundle.json" {
            continue;
        }
        let path = dir.join(name);
        if path.is_file() {
            tar.append_path_with_name(&path, name)?;
        }
    }
    let plugins = dir.join(PLUGINS_DIR);
    if plugins.is_dir() {
        tar.append_dir_all(PLUGINS_DIR, &plugins)?;
    }
    let bundle = serde_json::to_vec_pretty(&json!({
        "format": 1, "release": release, "sha256": sha256, "from": from,
    }))
    .unwrap_or_default();
    let mut header = tar::Header::new_gnu();
    header.set_size(bundle.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, "bundle.json", bundle.as_slice())?;
    let encoder = tar.into_inner()?;
    encoder.finish()?.flush()
}

/// What `unpack_bundle` found.
pub struct Unpacked {
    /// Bytes of the program.
    pub bytes: u64,
    pub files: Vec<String>,
}
/// Reads a release file into `into` (which must not exist yet), taking only
/// the files a release folder has: a path that is not one of them, a link
/// or anything outside the folder is refused and the folder removed.
pub fn unpack_bundle(reader: impl Read, into: &Path) -> Result<Unpacked, String> {
    // The installer, another user in the same group, removes the folder
    // once the release is copied into place, so the group may write it.
    make_group_dir(into).map_err(|error| {
        format!(
            "The incoming folder on this server could not be written ({}: {error}). Run scripts/install-server.sh create <instance> again on the server to put its permissions right.",
            into.display()
        )
    })?;
    let result = unpack_into(reader, into);
    if result.is_err() {
        let _ = fs::remove_dir_all(into);
    }
    result
}
fn unpack_into(reader: impl Read, into: &Path) -> Result<Unpacked, String> {
    let mut archive = tar::Archive::new(GzDecoder::new(reader));
    archive.set_preserve_permissions(false);
    archive.set_preserve_mtime(false);
    archive.set_unpack_xattrs(false);
    let mut files = Vec::new();
    let mut total = 0u64;
    let mut bytes = 0u64;
    let entries = archive.entries().map_err(|_| {
        "This is not a Bilibi release file: it is not a compressed archive.".to_owned()
    })?;
    for entry in entries {
        let mut entry = entry.map_err(|_| {
            "This is not a Bilibi release file: it is damaged or not an archive.".to_owned()
        })?;
        let path = entry
            .path()
            .map_err(|_| "The release file holds a name that cannot be read.".to_owned())?
            .into_owned();
        let name = bundle_entry_name(&path).ok_or_else(|| {
            format!(
                "The release file holds something a release never has ({}). Download it again from the source.",
                path.display()
            )
        })?;
        match entry.header().entry_type() {
            tar::EntryType::Directory => {
                if name == PLUGINS_DIR || name.starts_with("plugins/") {
                    make_group_dir(&into.join(&name))
                        .map_err(|error| format!("could not make {name}: {error}"))?;
                }
                continue;
            }
            tar::EntryType::Regular | tar::EntryType::Continuous => {}
            _ => {
                return Err(format!(
                    "The release file holds {name} as something other than a plain file. Download it again from the source."
                ))
            }
        }
        total += entry.size();
        if total > MAX_BUNDLE_BYTES {
            return Err("The release file is larger than a release can be. Download it again from the source.".into());
        }
        let target = into.join(&name);
        if let Some(parent) = target.parent().filter(|parent| *parent != into) {
            make_group_dir(parent)
                .map_err(|error| format!("could not make {}: {error}", parent.display()))?;
        }
        let mut file = fs::File::create(&target)
            .map_err(|error| format!("could not write {name}: {error}"))?;
        let written = io::copy(&mut entry, &mut file).map_err(|_| {
            "The release file ended early: it is incomplete. Download it again from the source."
                .to_owned()
        })?;
        let mode = if name == "bilibi-web" { 0o755 } else { 0o644 };
        fs::set_permissions(&target, fs::Permissions::from_mode(mode))
            .map_err(|error| format!("could not set the mode of {name}: {error}"))?;
        if name == "bilibi-web" {
            bytes = written;
        }
        files.push(name);
    }
    if !files.iter().any(|name| name == "bilibi-web") {
        return Err(
            "The release file holds no program (bilibi-web). It is not a Bilibi release file."
                .into(),
        );
    }
    if !files.iter().any(|name| name == "SHA256SUMS") {
        return Err("The release file has no checksum list (SHA256SUMS), so it cannot be checked. Download it again from the source.".into());
    }
    Ok(Unpacked { bytes, files })
}
/// A folder of a received release, writable by the group so the installer
/// (another user in it) can remove it once the release is in place.
fn make_group_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    // The group comes from incoming/ (set-group-id); the program is not a
    // member of it, so it may not set that bit itself.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o770))
}
/// The name an archive entry is allowed to have inside a release folder.
fn bundle_entry_name(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_owned()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    match parts.as_slice() {
        [name] if RELEASE_FILES.contains(&name.as_str()) || name == PLUGINS_DIR => {
            Some(name.clone())
        }
        [first, rest @ ..] if first == PLUGINS_DIR && !rest.is_empty() => {
            let safe = rest.iter().all(|part| {
                !part.is_empty()
                    && part.len() <= 128
                    && part
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                    && !part.starts_with('.')
            });
            safe.then(|| parts.join("/"))
        }
        _ => None,
    }
}

// ------------------------------------------------------------ the checks

/// The program's checksum as `SHA256SUMS` records it.
pub fn expected_checksum(dir: &Path) -> Result<String, String> {
    let sums = fs::read_to_string(dir.join("SHA256SUMS")).map_err(|_| {
        "The release has no checksum list (SHA256SUMS), so it cannot be checked.".to_owned()
    })?;
    sums.lines()
        .find_map(|line| {
            let (sha, name) = line.split_once(char::is_whitespace)?;
            (name.trim_start_matches('*').trim() == "bilibi-web"
                && sha.len() == 64
                && sha.bytes().all(|b| b.is_ascii_hexdigit()))
            .then(|| sha.to_ascii_lowercase())
        })
        .ok_or_else(|| {
            "The release's checksum list (SHA256SUMS) does not name the program.".to_owned()
        })
}
/// Checks the program against `SHA256SUMS` and returns its checksum.
pub fn verify_checksum(dir: &Path) -> Result<String, String> {
    let expected = expected_checksum(dir)?;
    let actual = sha256_of(&dir.join("bilibi-web"))
        .map_err(|error| format!("The program could not be read to check it: {error}."))?;
    if actual != expected {
        return Err("The program does not match its checksum: the release file was damaged on the way. Download it again from the source, or make the file again there.".into());
    }
    Ok(expected)
}
pub fn sha256_of(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// What this server is, as a release records what it was built for.
pub fn this_server() -> Value {
    json!({
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "system_library": system_library(),
    })
}
/// `glibc 2.39`, from `getconf GNU_LIBC_VERSION`; null when unknown.
pub fn system_library() -> Option<String> {
    let out = Command::new("getconf")
        .arg("GNU_LIBC_VERSION")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (out.status.success() && !text.is_empty()).then_some(text)
}
/// The version part of `glibc 2.39` as numbers.
fn library_version(text: &str) -> Option<(u64, u64)> {
    let version = text.split_whitespace().last()?;
    let (major, minor) = version.split_once('.')?;
    let minor: String = minor.chars().take_while(char::is_ascii_digit).collect();
    Some((major.parse().ok()?, minor.parse().ok()?))
}
/// Whether the program in `dir` was built for this kind of server: a Linux
/// program for this processor (read from the program itself), needing a
/// system library this server has (from `release.json`, when the builder
/// recorded it). Each refusal says what to do.
pub fn built_for_this_server(dir: &Path) -> Result<(), String> {
    let mut head = [0u8; 20];
    let read = fs::File::open(dir.join("bilibi-web"))
        .and_then(|mut file| file.read(&mut head))
        .map_err(|error| format!("The program could not be read: {error}."))?;
    if read < 20 || head[..4] != [0x7f, b'E', b'L', b'F'] {
        return Err("The file called bilibi-web is not a program. Make the release file again on the source.".into());
    }
    let machine = u16::from_le_bytes([head[18], head[19]]);
    let built_arch = match (head[4], head[5], machine) {
        (2, 1, 0x3E) => "x86_64",
        (2, 1, 0xB7) => "aarch64",
        (1, 1, 0x03) => "x86",
        (1, 1, 0x28) => "arm",
        _ => "another kind of processor",
    };
    let here = std::env::consts::ARCH;
    if built_arch != here {
        return Err(format!(
            "This release was built for {} computers, and this server is {}. Build it on a Dev of the same kind as this server.",
            describe_arch(built_arch),
            describe_arch(here)
        ));
    }
    if !matches!(head[7], 0 | 3) || std::env::consts::OS != "linux" {
        return Err(
            "This release was built for another operating system. Build it on a Linux Dev.".into(),
        );
    }
    let recipe: Value = fs::read_to_string(dir.join("release.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null);
    if let Some(needed) = recipe["built_for"]["system_library"].as_str() {
        if let (Some(need), Some(have_text)) = (library_version(needed), system_library()) {
            if let Some(have) = library_version(&have_text) {
                if have < need {
                    return Err(format!(
                        "This release needs the system library {needed}, and this server has {have_text}. Update this server, or build on a Dev whose system library is not newer than this server's."
                    ));
                }
            }
        }
    }
    Ok(())
}
fn describe_arch(arch: &str) -> &str {
    match arch {
        "x86_64" => "Linux x86-64",
        "aarch64" => "Linux ARM 64-bit",
        other => other,
    }
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
