//! Resumable uploads (issue 141, second phase): the private staging file a
//! live upload's chunks wait in, so a dropped connection, or a restarted
//! server, resumes where it stopped. No Bilimbi counterpart: LiveView's
//! uploads end with the socket.
//!
//! A staging file lives on the disk `media.upload.staging_disk` names, at
//! `staging/tenants/{tenant}/accounts/{account}/{key}` with mode 0600. The key
//! is the SHA-256 of what the upload is bound to: the impersonating operator,
//! the page service, the file input, the announced size and the file's own
//! SHA-256. Only the same person choosing the same file again in the same
//! input finds it. Its length is the offset the browser resumes from, and its
//! bytes are handed over only once they hash to the announced SHA-256.
//!
//! An account keeps at most [`MAX_STAGED`] staging files; opening another
//! removes its least recently written, and any of its files nobody wrote to
//! for `media.upload.staging_hours`. The `base/media-staging-sweep` schedule
//! removes those of every account. Staging files are transport, not records:
//! nothing here is audited, and what the owner then does with the bytes is
//! audited as usual.
use crate::{storage, Error, Media, Result};
use bilibi_database::Db;
use bilibi_module_registry::{Contribution, Provenance};
use bilibi_queue::{
    execution::{Execution, Handler, HandlerBinding, HandlerFuture, Outcome, HANDLERS},
    Worker, WORKERS,
};
use bilibi_schedule::{worker, Definition, Misfire, Overlap, Schedule, DEFINITIONS, EXECUTE};
use bilibi_tenancy::Scope;
use bilibi_ui::upload::MAX_FILE_SIZE;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime},
};
use tokio_postgres::GenericClient;

pub const DISK: &str = "media.upload.staging_disk";
pub const HOURS: &str = "media.upload.staging_hours";
/// Staging files one account may keep at once.
const MAX_STAGED: usize = 20;
pub(crate) const WORKER: &str = "base/media-staging-sweep";
pub(crate) const HANDLER: &str = "base/media.staging_sweep";
const ROOT: &str = "staging/tenants";
// `Media::staging` clamps `media.max_bytes` between the two: crossed, they
// fail the build rather than every announcement.
const _: () = assert!(MAX_FILE_SIZE <= storage::MAX_READ);
macro_rules! provenance {
    () => {
        Provenance {
            module: "base/media",
            source: file!(),
            line: line!(),
        }
    };
}

/// Where this installation stages resumable uploads, resolved once for an
/// announcement: the safe staging root, the largest file it takes and how
/// long an unfinished one is kept. Staging through it needs no connection.
#[derive(Clone, Debug)]
pub struct Staging {
    root: PathBuf,
    limit: u64,
    keep: Duration,
}

/// What a staged upload is bound to, besides the session's tenant and
/// account: the page or panel service holding the file input, the input's
/// name, the size the browser announced and the SHA-256 of the whole file.
#[derive(Clone, Copy, Debug)]
pub struct StagingRequest<'a> {
    pub owner: &'a str,
    pub upload: &'a str,
    pub size: u64,
    pub sha256: [u8; 32],
}

/// One open staging file. Chunks are written in order; [`StagedUpload::finish`]
/// hands the bytes over once every byte is there and they hash as announced.
#[derive(Debug)]
pub struct StagedUpload {
    path: PathBuf,
    file: Option<fs::File>,
    size: u64,
    expected: [u8; 32],
    hasher: Sha256,
    received: u64,
    /// The bytes once failed their hash and the file started again; a second
    /// mismatch is the browser's, and refuses.
    restarted: bool,
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Error::StorageUnavailable)?
}
fn unavailable(_: std::io::Error) -> Error {
    Error::StorageUnavailable
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Media {
    /// Where this installation stages resumable uploads: `None` while
    /// `media.upload.staging_disk` names no configured, safe disk, and such
    /// inputs then hold their bytes in the live state as every other input
    /// does. Read it once per announcement and drop the connection before
    /// staging.
    pub async fn staging(&self, client: &(impl GenericClient + Sync)) -> Result<Option<Staging>> {
        let disk = self.setting(client, DISK).await?;
        let disk = disk.as_str().ok_or(Error::InvalidRules)?;
        let root = match storage::location(&self.disks(client).await?, disk, ROOT).await {
            Ok(root) => root,
            Err(Error::StorageNotConfigured | Error::UnsafeStorageRoot) => return Ok(None),
            Err(error) => return Err(error),
        };
        let (max, _) = self.limits(client).await?;
        let hours = self
            .setting(client, HOURS)
            .await?
            .as_u64()
            .ok_or(Error::InvalidRules)?;
        Ok(Some(Staging {
            root,
            // Never below what an upload held in memory may reach, so a
            // resumable input takes every file an ordinary one would.
            limit: max.clamp(MAX_FILE_SIZE, storage::MAX_READ),
            keep: Duration::from_secs(hours * 3_600),
        }))
    }

    /// The schedule's sweep: removes every staging file nobody wrote to for
    /// `media.upload.staging_hours`, in every tenant. A disk that is not
    /// configured has nothing to sweep.
    pub(crate) async fn sweep_staging(
        &self,
        client: &(impl GenericClient + Sync),
    ) -> Result<usize> {
        let Some(staging) = self.staging(client).await? else {
            return Ok(0);
        };
        let cutoff = SystemTime::now() - staging.keep;
        blocking(move || {
            let mut removed = 0;
            for tenant in directories(&staging.root)? {
                for account in directories(&tenant.join("accounts"))? {
                    for (path, modified) in files(&account)? {
                        if modified < cutoff && fs::remove_file(&path).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
            Ok(removed)
        })
        .await
    }
}

impl Staging {
    /// The most bytes one staged file may have: `media.max_bytes`, never
    /// below the 64 MiB of an upload held in memory ([`MAX_FILE_SIZE`]) and
    /// never above the 512 MiB Media reads back. The host refuses a larger
    /// file by this same figure (`UploadSpec::limit`), so a file it places
    /// is never refused here for its size.
    pub fn limit(&self) -> u64 {
        self.limit
    }

    /// Opens, or creates, the staging file of this upload for the scope's
    /// account. A file left by an earlier connection is picked up where it
    /// ends: its bytes are hashed again so the final check covers them.
    pub async fn stage(&self, scope: &Scope, request: StagingRequest<'_>) -> Result<StagedUpload> {
        if request.size > self.limit
            || request.owner.is_empty()
            || request.owner.len() > 255
            || request.upload.is_empty()
            || request.upload.len() > 64
        {
            return Err(Error::InvalidUpload);
        }
        let mut binding = Sha256::new();
        for part in [
            "bilibi-upload-staging/v1".as_bytes(),
            &scope.tenant().to_be_bytes(),
            &scope.user().to_be_bytes(),
            &scope
                .impersonator()
                .map_or(0, |operator| operator.user())
                .to_be_bytes(),
            request.owner.as_bytes(),
            request.upload.as_bytes(),
            &request.size.to_be_bytes(),
            &request.sha256,
        ] {
            binding.update((part.len() as u64).to_be_bytes());
            binding.update(part);
        }
        let directory = self
            .root
            .join(scope.tenant().to_string())
            .join("accounts")
            .join(scope.user().to_string());
        storage::make_directories(&directory).await?;
        let path = directory.join(hex(&binding.finalize()));
        let (size, expected) = (request.size, request.sha256);
        let cutoff = SystemTime::now() - self.keep;
        blocking(move || {
            evict(&directory, &path, cutoff)?;
            let mut file = fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW)
                .open(&path)
                .map_err(unavailable)?;
            let stat = file.metadata().map_err(unavailable)?;
            if !stat.is_file() {
                return Err(Error::UnsafeStorageKey);
            }
            let mut received = stat.len();
            if received > size {
                file.set_len(0).map_err(unavailable)?;
                received = 0;
            }
            // Picking the file up counts as writing to it, so the sweep
            // spares a file someone came back for.
            file.set_modified(SystemTime::now()).map_err(unavailable)?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0; 1 << 20];
            let mut left = received;
            file.seek(SeekFrom::Start(0)).map_err(unavailable)?;
            while left > 0 {
                let take = left.min(buffer.len() as u64) as usize;
                file.read_exact(&mut buffer[..take]).map_err(unavailable)?;
                hasher.update(&buffer[..take]);
                left -= take as u64;
            }
            Ok(StagedUpload {
                path,
                file: Some(file),
                size,
                expected,
                hasher,
                received,
                restarted: false,
            })
        })
        .await
    }
}

/// The real directories inside `path`; none when it does not exist. Links and
/// anything else are passed over, never followed.
fn directories(path: &Path) -> Result<Vec<PathBuf>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unavailable(error)),
    };
    Ok(entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect())
}
/// The regular files inside `path` with when they were last written.
fn files(path: &Path) -> Result<Vec<(PathBuf, SystemTime)>> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(unavailable(error)),
    };
    Ok(entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .filter_map(|entry| {
            let modified = entry.metadata().ok()?.modified().ok()?;
            Some((entry.path(), modified))
        })
        .collect())
}
/// Makes room for `keep` in its account: the account's other staging files
/// nobody wrote to since `cutoff` go, then the least recently written until
/// it holds fewer than [`MAX_STAGED`].
fn evict(directory: &Path, keep: &Path, cutoff: SystemTime) -> Result<()> {
    let mut others: Vec<_> = files(directory)?
        .into_iter()
        .filter(|(path, _)| path != keep)
        .filter(|(path, modified)| *modified >= cutoff || fs::remove_file(path).is_err())
        .collect();
    if others.len() < MAX_STAGED {
        return Ok(());
    }
    others.sort_by_key(|(_, modified)| *modified);
    for (path, _) in others.iter().take(others.len() + 1 - MAX_STAGED) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

impl StagedUpload {
    /// Whether both stage into one file: the same person chose the same file
    /// in the same input twice.
    pub fn same_file(&self, other: &StagedUpload) -> bool {
        self.path == other.path
    }

    /// Bytes already staged: where the browser continues.
    pub fn received(&self) -> u64 {
        self.received
    }

    /// Writes one chunk where the staged bytes end and returns the bytes now
    /// staged. A chunk anywhere else, or past the announced size, refuses.
    pub async fn write(&mut self, offset: u64, bytes: Vec<u8>) -> Result<u64> {
        if offset != self.received || offset + bytes.len() as u64 > self.size {
            return Err(Error::InvalidUpload);
        }
        let mut file = self.file.take().ok_or(Error::StorageUnavailable)?;
        let (file, written) = tokio::task::spawn_blocking(move || {
            let written = file
                .seek(SeekFrom::Start(offset))
                .and_then(|_| file.write_all(&bytes))
                .map(|()| bytes);
            (file, written)
        })
        .await
        .map_err(|_| Error::StorageUnavailable)?;
        self.file = Some(file);
        let bytes = written.map_err(unavailable)?;
        self.hasher.update(&bytes);
        self.received += bytes.len() as u64;
        Ok(self.received)
    }

    /// The staged bytes, once all of them are there and they hash to the
    /// announced SHA-256. The first mismatch empties the file and answers
    /// `Ok(None)`: the browser sends the file again from the start. A second
    /// one refuses (`InvalidUpload`). The file stays until [`Self::remove`],
    /// so a reconnect before the owner takes the bytes finds them complete.
    pub async fn finish(&mut self) -> Result<Option<Vec<u8>>> {
        if self.received != self.size {
            return Err(Error::InvalidUpload);
        }
        if self.hasher.clone().finalize().as_slice() != self.expected {
            if self.restarted {
                return Err(Error::InvalidUpload);
            }
            let file = self.file.as_ref().ok_or(Error::StorageUnavailable)?;
            file.set_len(0).map_err(unavailable)?;
            self.hasher = Sha256::new();
            self.received = 0;
            self.restarted = true;
            return Ok(None);
        }
        let mut file = self.file.take().ok_or(Error::StorageUnavailable)?;
        let size = self.size;
        let (file, read) = tokio::task::spawn_blocking(move || {
            let mut bytes = Vec::with_capacity(size as usize);
            let read = file
                .seek(SeekFrom::Start(0))
                .and_then(|_| (&mut file).take(size + 1).read_to_end(&mut bytes))
                .map(|_| bytes);
            (file, read)
        })
        .await
        .map_err(|_| Error::StorageUnavailable)?;
        self.file = Some(file);
        let bytes = read.map_err(unavailable)?;
        if bytes.len() as u64 != size {
            return Err(Error::StorageUnavailable);
        }
        Ok(Some(bytes))
    }

    /// Removes the staging file: the owner took the bytes, or the person
    /// removed the file. Already gone counts as removed.
    pub async fn remove(mut self) {
        self.file = None;
        let path = self.path.clone();
        let _ = tokio::task::spawn_blocking(move || fs::remove_file(path)).await;
    }
}

fn no_arguments(args: Value) -> bilibi_database::Result<Value> {
    match args.as_object() {
        Some(object) if object.is_empty() => Ok(args),
        _ => Err(bilibi_database::Error::Invalid),
    }
}
inventory::submit! { Contribution::new(WORKERS, WORKER, provenance!(), &Worker { max_attempts: 3, validate: |args| worker::validate(args, no_arguments) }) }
inventory::submit! { Contribution::new(HANDLERS, WORKER, provenance!(), &HandlerBinding { service: HANDLER, capability: EXECUTE }) }
inventory::submit! { Contribution::new(DEFINITIONS, "base/media-staging-sweep", provenance!(), &Definition {
    name: "Remove expired upload staging files", expression: "23 * * * *", timezone: "Etc/UTC",
    task_name: "Base Media staging sweep", worker: WORKER, args: || json!({}),
    overlap: Overlap::Forbid, misfire: Misfire::Coalesce, owner_route: None,
}) }

pub(crate) struct Sweep {
    pub schedule: Arc<Schedule>,
    pub media: Arc<Media>,
    pub db: Db,
}
impl Handler for Sweep {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(self.schedule.run(execution, args, |_| async {
            let swept = async {
                let client = self.db.get().await.map_err(|_| Error::StorageUnavailable)?;
                self.media.sweep_staging(&**client).await
            }
            .await;
            match swept {
                Ok(_) => Outcome::Completed,
                Err(_) => Outcome::Retry("staging_unavailable"),
            }
        }))
    }
}
