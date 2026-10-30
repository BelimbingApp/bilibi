//! AES-256 PDF protection through `qpdf` (QPDFJob JSON, R6 security handler).
//!
//! Passwords reach the tool only inside a `0600` job file in a random `0700`
//! directory, never argv, environment or a log, and that directory (plaintext
//! input, job, encrypted output) is removed on success, failure, timeout and
//! cancellation. The tool leads its own process group, which is killed on every
//! exit. Tool output is discarded and callers only ever see fixed errors.
//!
//! Password types and result provenance live here too: `Secret` cannot be
//! printed, cloned or serialized, so an options value is safe to pass around
//! and to hold in a `Debug`-derived struct.
use crate::{engine, pdf, Error, Metadata, Result};
use serde_json::{json, Value};
use std::{
    fmt,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{process::Command, time::timeout};

/// A PDF password. The R6 handler uses the first 127 UTF-8 bytes and silently
/// ignores the rest, so a longer password would be weaker than it looks: it is
/// refused instead. Never printed, cloned or serialized.
pub struct Secret(String);
impl Secret {
    pub fn new(password: impl Into<String>) -> Result<Self> {
        let password = password.into();
        if password.is_empty() || password.len() > 127 || password.chars().any(char::is_control) {
            return Err(Error::InvalidPassword);
        }
        Ok(Self(password))
    }
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(<redacted>)")
    }
}

/// Versions recorded as provenance, plus an optional password. An empty
/// password is not "no protection": `Secret::new` refuses it, so an owner whose
/// lookup came back empty fails instead of storing an unprotected copy.
#[derive(Debug, Default)]
pub struct PdfOptions {
    template_version: Option<String>,
    data_version: Option<String>,
    password: Option<Secret>,
    owner_password: Option<Secret>,
    retention: Option<crate::Retention>,
}
const UNVERSIONED: &str = "unversioned";
impl PdfOptions {
    pub fn template_version(mut self, version: impl Into<String>) -> Self {
        self.template_version = Some(version.into());
        self
    }
    pub fn data_version(mut self, version: impl Into<String>) -> Self {
        self.data_version = Some(version.into());
        self
    }
    /// Encrypt with AES-256. The user password opens the file; an omitted owner
    /// password falls back to it.
    pub fn password(mut self, user: Secret, owner: Option<Secret>) -> Self {
        self.password = Some(user);
        self.owner_password = owner;
        self
    }
    pub fn protected(&self) -> bool {
        self.password.is_some()
    }
    pub(crate) fn validate(&self) -> Result<()> {
        for version in [&self.template_version, &self.data_version]
            .into_iter()
            .flatten()
        {
            if !crate::bounded(version, 255) {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
    /// Publish the PDF records-grade, as `put_retained` does.
    pub fn retained(mut self, retention: crate::Retention) -> Self {
        self.retention = Some(retention);
        self
    }
    pub(crate) fn retention(&self) -> Option<&crate::Retention> {
        self.retention.as_ref()
    }
    pub(crate) fn passwords(&self) -> Option<(&Secret, Option<&Secret>)> {
        self.password
            .as_ref()
            .map(|user| (user, self.owner_password.as_ref()))
    }
}

/// A stored PDF plus how it was produced. Holds the private artifact metadata,
/// never a filesystem path or any credential.
#[derive(Clone, Debug)]
pub struct PdfArtifact {
    pub document: Metadata,
    pub template_version: String,
    pub data_version: String,
    pub produced_by: i64,
    /// UTC, from the database clock.
    pub produced_at: String,
}
const ENCRYPTED: &str = " (encrypted)";
impl PdfArtifact {
    pub(crate) fn new(
        document: Metadata,
        options: &PdfOptions,
        produced_by: i64,
        produced_at: String,
    ) -> Self {
        let data = options.data_version.as_deref().unwrap_or(UNVERSIONED);
        Self {
            document,
            template_version: options
                .template_version
                .clone()
                .unwrap_or_else(|| UNVERSIONED.into()),
            data_version: if options.protected() {
                format!("{data}{ENCRYPTED}")
            } else {
                data.into()
            },
            produced_by,
            produced_at,
        }
    }
    /// Provenance without private paths or credentials.
    pub fn to_map(&self) -> Value {
        json!({"artifact_id":self.document.id,"template_version":self.template_version,
            "data_version":self.data_version,"bytes":self.document.byte_size,
            "sha256":self.document.sha256,"produced_by":self.produced_by,
            "produced_at":self.produced_at})
    }
    pub(crate) fn encrypted_copy(
        &self,
        document: Metadata,
        produced_by: i64,
        produced_at: String,
    ) -> Self {
        Self {
            document,
            template_version: self.template_version.clone(),
            data_version: format!("{}{ENCRYPTED}", self.data_version),
            produced_by,
            produced_at,
        }
    }
}

pub(crate) struct Tool {
    pub binary: String,
    pub timeout: Duration,
    pub max_bytes: usize,
}

/// Owns the random 0700 directory and the tool's process group.
struct Run {
    dir: PathBuf,
    pid: Option<u32>,
}
impl Drop for Run {
    fn drop(&mut self) {
        if let Some(pid) = self.pid {
            // SAFETY: the child leads its own group (`process_group(0)`) and has
            // not been reaped by this guard, so the group id cannot be reused.
            unsafe { libc::kill(-(pid as i32), libc::SIGKILL) };
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
fn private_file(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// Encrypts `bytes` into a new PDF; the input is never modified.
pub(crate) async fn encrypt(
    tool: &Tool,
    bytes: &[u8],
    user: &Secret,
    owner: Option<&Secret>,
) -> Result<Vec<u8>> {
    pdf::validate(bytes)?;
    let binary = engine::executable(&tool.binary).map_err(|_| Error::PdfProtectionUnavailable)?;
    let dir = std::env::temp_dir().join(format!("bilibi-pdf-{}", uuid::Uuid::new_v4()));
    std::fs::DirBuilder::new().mode(0o700).create(&dir)?;
    let mut run = Run { dir, pid: None };
    let (source, job, output) = (
        run.dir.join("source.pdf"),
        run.dir.join("qpdf.json"),
        run.dir.join("output.pdf"),
    );
    private_file(&source, bytes)?;
    let owner = owner.unwrap_or(user);
    let job_json = serde_json::to_vec(&json!({
        "inputFile":source,"outputFile":output,
        "encrypt":{"userPassword":user.0,"ownerPassword":owner.0,"256bit":{}},
    }))
    .map_err(|_| Error::PdfProtectionFailed)?;
    private_file(&job, &job_json)?;
    let mut child = Command::new(binary)
        .arg(format!("--job-json-file={}", job.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()
        .map_err(|_| Error::PdfProtectionUnavailable)?;
    run.pid = child.id();
    let status = timeout(tool.timeout, child.wait())
        .await
        .map_err(|_| Error::PdfTimeout)?
        .map_err(|_| Error::PdfProtectionFailed)?;
    // Reaped: the group id may now be reused, so the guard must not signal it.
    run.pid = None;
    if !status.success() {
        return Err(Error::PdfProtectionFailed);
    }
    let encrypted = read_output(&output, tool.max_bytes)?;
    pdf::validate(&encrypted).map_err(|_| Error::PdfProtectionFailed)?;
    Ok(encrypted)
}
fn read_output(path: &Path, max: usize) -> Result<Vec<u8>> {
    use std::io::Read;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| Error::PdfProtectionFailed)?;
    let stat = file.metadata()?;
    if !stat.is_file() || stat.len() == 0 || stat.len() > max as u64 {
        return Err(Error::PdfProtectionFailed);
    }
    let mut bytes = Vec::with_capacity(stat.len() as usize);
    file.take(max as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > max {
        return Err(Error::PdfProtectionFailed);
    }
    Ok(bytes)
}
