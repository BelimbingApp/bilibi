//! Private local disks. Operators map durable disk names to absolute 0700
//! directories; keys come from the module, never from a browser. Nothing here
//! follows a symlink, escapes a root, or treats the `external` sentinel as a
//! filesystem location.
use crate::{Error, Result};
use serde_json::Value;
use std::{
    os::unix::fs::MetadataExt,
    path::{Component, Path, PathBuf},
};
use tokio::{
    fs::{self, OpenOptions},
    io::{AsyncReadExt, AsyncWriteExt},
};

pub const EXTERNAL: &str = "external";
/// Streams are buffered, so adopted files above this refuse rather than exhaust memory.
pub const MAX_READ: u64 = 536_870_912;
const RESERVED_ROOT_PARTS: [&str; 3] = ["static", "public", "assets"];

pub fn validate_key(key: &str) -> Result<()> {
    if !key.is_empty()
        && key.len() <= 255
        && !key.starts_with('/')
        && !key.contains(['\\', '\0'])
        && !key.chars().any(char::is_control)
        && key.split('/').all(|part| !["", ".", ".."].contains(&part))
    {
        Ok(())
    } else {
        Err(Error::UnsafeStorageKey)
    }
}

/// Resolve `disk`/`key` against the operator's `media.disks` mapping.
pub async fn location(roots: &Value, disk: &str, key: &str) -> Result<PathBuf> {
    let root = roots
        .as_object()
        .and_then(|roots| roots.get(disk))
        .and_then(Value::as_str)
        .ok_or(Error::StorageNotConfigured)?;
    validate_root(root).await?;
    validate_key(key)?;
    Ok(Path::new(root).join(key))
}

/// Absolute, normalized, symlink-free, private (0700), and not under a served tree.
pub async fn validate_root(root: &str) -> Result<()> {
    let path = Path::new(root);
    let mut parts = path.components();
    let normalized = parts.next() == Some(Component::RootDir)
        && parts.all(|part| matches!(part, Component::Normal(name) if !RESERVED_ROOT_PARTS.iter().any(|reserved| name == *reserved)))
        && path.to_str().is_some_and(|text| {
            text == "/"
                || text
                    == path
                        .components()
                        .skip(1)
                        .fold(String::new(), |all, part| {
                            format!("{all}/{}", part.as_os_str().to_string_lossy())
                        })
        });
    if !normalized || root == "/" {
        return Err(Error::UnsafeStorageRoot);
    }
    directory_chain(path)
        .await
        .map_err(|_| Error::UnsafeStorageRoot)?;
    let stat = fs::symlink_metadata(path)
        .await
        .map_err(|_| Error::UnsafeStorageRoot)?;
    if stat.mode() & 0o777 != 0o700 {
        return Err(Error::UnsafeStorageRoot);
    }
    Ok(())
}

/// Every existing component from `/` must be a real directory, never a symlink.
async fn directory_chain(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        let stat = fs::symlink_metadata(&current)
            .await
            .map_err(|_| Error::UnsafeStorageKey)?;
        if !stat.is_dir() {
            return Err(Error::UnsafeStorageKey);
        }
    }
    Ok(())
}

/// Create missing directories below `path` with 0700, checking each component.
pub async fn make_directories(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for part in path.components() {
        current.push(part);
        match fs::symlink_metadata(&current).await {
            Ok(stat) if stat.is_dir() => {}
            Ok(_) => return Err(Error::UnsafeStorageKey),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                match fs::DirBuilder::new().mode(0o700).create(&current).await {
                    Ok(()) => {}
                    // A concurrent writer created it first; re-check it is a directory.
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if !fs::symlink_metadata(&current)
                            .await
                            .is_ok_and(|stat| stat.is_dir())
                        {
                            return Err(Error::UnsafeStorageKey);
                        }
                    }
                    Err(_) => return Err(Error::StorageUnavailable),
                }
            }
            Err(_) => return Err(Error::StorageUnavailable),
        }
    }
    Ok(())
}

/// Exclusive, 0600, synced. A failed write removes the partial file.
pub async fn write(roots: &Value, disk: &str, key: &str, bytes: &[u8]) -> Result<()> {
    let path = location(roots, disk, key).await?;
    make_directories(path.parent().ok_or(Error::UnsafeStorageKey)?).await?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .await
        .map_err(|error| match error.kind() {
            std::io::ErrorKind::AlreadyExists => Error::StorageKeyExists,
            _ => Error::StorageUnavailable,
        })?;
    let result = async {
        file.write_all(bytes).await?;
        file.sync_all().await
    }
    .await;
    drop(file);
    if result.is_err() {
        let _ = fs::remove_file(&path).await;
        return Err(Error::StorageUnavailable);
    }
    Ok(())
}

pub async fn read(roots: &Value, disk: &str, key: &str) -> Result<Vec<u8>> {
    let unavailable = |_| Error::StorageUnavailable;
    let path = location(roots, disk, key).await.map_err(unavailable)?;
    directory_chain(path.parent().ok_or(Error::StorageUnavailable)?)
        .await
        .map_err(unavailable)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)
        .await
        .map_err(|_| Error::StorageUnavailable)?;
    let stat = file
        .metadata()
        .await
        .map_err(|_| Error::StorageUnavailable)?;
    if !stat.is_file() || stat.len() > MAX_READ {
        return Err(Error::StorageUnavailable);
    }
    let mut bytes = Vec::with_capacity(stat.len() as usize);
    file.take(MAX_READ + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| Error::StorageUnavailable)?;
    if bytes.len() as u64 != stat.len() {
        return Err(Error::StorageUnavailable);
    }
    Ok(bytes)
}

/// Idempotent: a missing file or directory is already deleted. A symlink or
/// anything but a regular file refuses.
pub async fn delete(roots: &Value, disk: &str, key: &str) -> Result<()> {
    if disk == EXTERNAL {
        return Ok(());
    }
    let path = location(roots, disk, key).await?;
    match directory_chain(path.parent().ok_or(Error::UnsafeStorageKey)?).await {
        Ok(()) => {}
        Err(_) if !fs::try_exists(path.parent().unwrap()).await.unwrap_or(true) => return Ok(()),
        Err(error) => return Err(error),
    }
    match fs::symlink_metadata(&path).await {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Ok(stat) if stat.is_file() => fs::remove_file(&path)
            .await
            .map_err(|_| Error::StorageUnavailable),
        _ => Err(Error::UnsafeStorageKey),
    }
}
