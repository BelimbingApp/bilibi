//! Synchronous, bounded IO inside a locked DB transaction. No await may release
//! that lock while an uncancellable background write is still creating bytes.
//! Roots and their parents are controlled by trusted operators.
use crate::{Error, Result};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Component, Path, PathBuf},
};

/// `block_in_place` yields the worker to Tokio while retaining synchronous
/// completion/cancellation semantics. Single-thread runtimes perform bounded IO
/// inline; callers of those runtimes should dedicate them to document work.
pub(crate) fn io<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    if tokio::runtime::Handle::try_current()
        .is_ok_and(|handle| handle.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
    {
        tokio::task::block_in_place(work)
    } else {
        work()
    }
}

pub(crate) const MAX_BYTES: usize = 536_870_912;
/// Only a new document creates missing folders. Reads and cleanup must never
/// conceal a missing volume by creating an empty replacement.
pub(crate) fn prepare_root(root: &str) -> Result<()> {
    check_root(root, true)
}
pub(crate) fn validate_root(root: &str) -> Result<()> {
    check_root(root, false)
}
fn check_root(root: &str, create: bool) -> Result<()> {
    let path = Path::new(root);
    if root == "/" || !path.is_absolute() || root.len() > 4096 {
        return Err(Error::UnsafeRoot);
    }
    let mut current = PathBuf::new();
    for part in path.components() {
        match part {
            Component::RootDir => {}
            Component::Normal(name)
                if !["public", "static", "assets"].iter().any(|p| name == *p) => {}
            _ => return Err(Error::UnsafeRoot),
        }
        current.push(part);
        if create {
            match fs::symlink_metadata(&current) {
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match fs::DirBuilder::new().mode(0o700).create(&current) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error.into()),
                    }
                }
                Err(error) => return Err(error.into()),
                Ok(_) => {}
            }
        }
        if !fs::symlink_metadata(&current).is_ok_and(|stat| stat.is_dir()) {
            return Err(Error::UnsafeRoot);
        }
    }
    if root != current.to_string_lossy()
        || !fs::symlink_metadata(path).is_ok_and(|stat| stat.mode() & 0o777 == 0o700)
    {
        return Err(Error::UnsafeRoot);
    }
    Ok(())
}
fn path(root: &str, id: &str) -> Result<PathBuf> {
    validate_root(root)?;
    let uuid = uuid::Uuid::parse_str(id).map_err(|_| Error::Invalid)?;
    if uuid.to_string() != id || uuid.get_version_num() != 4 {
        return Err(Error::Invalid);
    }
    Ok(Path::new(root).join(id))
}
fn sync_root(root: &str) -> Result<()> {
    File::open(root)?.sync_all()?;
    Ok(())
}
pub(crate) fn write(root: &str, id: &str, bytes: &[u8]) -> Result<()> {
    let path = path(root, id)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    sync_root(root)
}
pub(crate) fn read(root: &str, id: &str, size: usize) -> Result<Vec<u8>> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path(root, id)?)?;
    let stat = file.metadata()?;
    if !stat.is_file()
        || stat.mode() & 0o777 != 0o600
        || stat.len() != size as u64
        || size > MAX_BYTES
    {
        return Err(Error::Integrity);
    }
    let mut bytes = Vec::with_capacity(size);
    file.take(size as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() != size {
        return Err(Error::Integrity);
    }
    Ok(bytes)
}
pub(crate) fn delete(root: &str, id: &str) -> Result<()> {
    let path = path(root, id)?;
    match fs::symlink_metadata(&path) {
        Ok(stat) if stat.is_file() => fs::remove_file(path)?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return Err(Error::CleanupPending),
    }
    // Also sync a previously unlinked entry after an ambiguous DB commit.
    sync_root(root)
}
/// Succeeds only when no entry (not even a dangling link) remains at the path.
pub(crate) fn absent(root: &str, id: &str) -> Result<()> {
    match fs::symlink_metadata(path(root, id)?) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
        Ok(_) => Err(Error::BytesPresent),
    }
}
