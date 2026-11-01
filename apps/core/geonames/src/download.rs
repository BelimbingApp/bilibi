//! ETag-aware download cache.
//!
//! A cached file with a stored ETag is revalidated with `If-None-Match`; a 304
//! keeps it. A cached file without an ETag is trusted for `ttl_days` by its
//! modification time and makes no request. A body is streamed into a
//! temporary file beside the target and renamed over it only after a 2xx, so a
//! failed download never truncates the cache. When the server is unreachable
//! or answers 429/500/502/503/504, an existing non-empty cache is used and the
//! outcome says so, with the cache's age; `force` turns that fallback off.
use bilibi_integration::{Call, Failure, Integration, Origin, Outgoing};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime},
};

/// GeoNames payloads stream to disk; this only bounds a runaway response.
const MAX_BODY_BYTES: u64 = 500 << 20;
const FALLBACK_STATUSES: [u16; 5] = [429, 500, 502, 503, 504];

pub(crate) struct Request<'a> {
    pub url: &'a str,
    pub destination: &'a Path,
    pub force: bool,
    pub ttl_days: u64,
    pub timeout: Duration,
}

/// How the file to import was obtained. `NotModified` (the server confirmed
/// the cache is current) and `Fallback` (the download failed and the old file
/// was kept) must stay distinguishable to the operator.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DownloadStatus {
    /// A 2xx body replaced the cache.
    Fetched(u16),
    /// A stored ETag was revalidated with a 304.
    NotModified,
    /// No request: an ETag-less cache younger than the TTL.
    Fresh,
    /// The request failed; the existing cache was used.
    Fallback(FallbackCause),
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FallbackCause {
    /// No HTTP answer: connect, timeout, transport or body failure.
    Unreachable(&'static str),
    HttpStatus(u16),
}

#[derive(Clone, Debug)]
pub(crate) struct Downloaded {
    pub status: DownloadStatus,
    /// The cache file's modification time when it was served from cache.
    pub cached_at: Option<SystemTime>,
}
impl Downloaded {
    pub fn cached(&self) -> bool {
        !matches!(self.status, DownloadStatus::Fetched(_))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DownloadFailure {
    #[error("HTTP status {0}")]
    HttpStatus(u16),
    #[error("request failed ({0})")]
    Request(String),
    #[error("cache file: {0}")]
    Io(String),
}
fn io(error: std::io::Error) -> DownloadFailure {
    DownloadFailure::Io(error.to_string())
}

pub(crate) fn etag_path(destination: &Path) -> PathBuf {
    sibling(destination, ".etag")
}
pub(crate) fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}
/// A process-unique suffix for temporary and backup files.
pub(crate) fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}-{nanos}", std::process::id())
}

pub(crate) async fn download(
    integration: &Integration,
    request: Request<'_>,
) -> Result<Downloaded, DownloadFailure> {
    let etag_file = etag_path(request.destination);
    if let Some(parent) = request.destination.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(io)?;
    }
    if !request.force && fresh_without_etag(request.destination, &etag_file, request.ttl_days) {
        return Ok(from_cache(request.destination, DownloadStatus::Fresh));
    }
    let stored_etag = if request.force || !request.destination.is_file() {
        None
    } else {
        read_etag(&etag_file).await
    };
    let temporary = sibling(request.destination, &format!(".download-{}", unique()));
    let mut outgoing = Outgoing::get(request.url)
        .timeout(request.timeout)
        .max_body_bytes(MAX_BODY_BYTES);
    if let Some(etag) = &stored_etag {
        outgoing = outgoing.header("If-None-Match", etag.clone());
    }
    let call = Call {
        system: "geonames".into(),
        operation: "reference.download".into(),
        owner_type: Some("core/geonames".into()),
        ..Call::default()
    };
    let mut file = tokio::fs::File::create(&temporary).await.map_err(io)?;
    let response = integration
        .stream(Origin::System, &call, outgoing, &mut file)
        .await;
    drop(file);
    let fallback = |cause| {
        (!request.force && valid_cache(request.destination))
            .then(|| from_cache(request.destination, DownloadStatus::Fallback(cause)))
    };
    let outcome = match response {
        Ok(reply) if reply.status == 304 && stored_etag.is_some() => {
            Ok(from_cache(request.destination, DownloadStatus::NotModified))
        }
        Ok(reply) if (200..300).contains(&reply.status) => {
            let etag = reply
                .headers
                .get("etag")
                .and_then(|value| value.to_str().ok())
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
            match tokio::fs::rename(&temporary, request.destination).await {
                Err(error) => Err(io(error)),
                Ok(()) => store_etag(&etag_file, etag.as_deref())
                    .await
                    .map(|()| Downloaded {
                        status: DownloadStatus::Fetched(reply.status),
                        cached_at: None,
                    }),
            }
        }
        Ok(reply) => {
            let cause = FallbackCause::HttpStatus(reply.status);
            match FALLBACK_STATUSES.contains(&reply.status) {
                true => fallback(cause).ok_or(DownloadFailure::HttpStatus(reply.status)),
                false => Err(DownloadFailure::HttpStatus(reply.status)),
            }
        }
        // Only a recorded transport failure falls back. An invalid request is
        // our bug and must not look like a working cache; an unrecorded one
        // is refused outright.
        Err(Failure::Transport { class, .. }) => fallback(FallbackCause::Unreachable(class))
            .ok_or(DownloadFailure::Request(class.into())),
        Err(other) => Err(DownloadFailure::Request(other.to_string())),
    };
    let _ = tokio::fs::remove_file(&temporary).await;
    outcome
}

fn from_cache(path: &Path, status: DownloadStatus) -> Downloaded {
    Downloaded {
        status,
        cached_at: std::fs::metadata(path).and_then(|m| m.modified()).ok(),
    }
}

fn valid_cache(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

fn fresh_without_etag(destination: &Path, etag_file: &Path, ttl_days: u64) -> bool {
    if etag_file.is_file() {
        return false;
    }
    let Some(modified) = std::fs::metadata(destination)
        .ok()
        .filter(|m| m.is_file())
        .and_then(|m| m.modified().ok())
    else {
        return false;
    };
    let ttl = Duration::from_secs(ttl_days.saturating_mul(86_400));
    SystemTime::now()
        .duration_since(modified)
        .map_or(true, |age| age <= ttl)
}

async fn read_etag(path: &Path) -> Option<String> {
    let contents = tokio::fs::read_to_string(path).await.ok()?;
    let etag = contents.trim();
    (!etag.is_empty()).then(|| etag.to_string())
}

async fn store_etag(path: &Path, etag: Option<&str>) -> Result<(), DownloadFailure> {
    match etag {
        Some(etag) => tokio::fs::write(path, etag).await.map_err(io),
        None => match tokio::fs::remove_file(path).await {
            Err(error) if error.kind() != std::io::ErrorKind::NotFound => Err(io(error)),
            _ => Ok(()),
        },
    }
}
