//! Malware scanning (issue 141, ADR 08). No Bilimbi counterpart: Bilimbi
//! stores uploads unscanned. An Extension binds the installation's one
//! `Scanner`; while one is bound every file Media stores starts `pending`, and
//! the `base/media-scan` job, run as the `media-scanner` system principal,
//! records the verdict. Only a clean file, or an unscanned one while
//! `media.scan.policy` allows it, is ever read back: `pending`, `infected` and
//! `failed` bytes stay on disk, unreadable. A scanner outage never makes a
//! file readable; it retries with Queue's backoff, then marks the file
//! `failed` for a quarantine manager to rescan.
use crate::{quarantine::MANAGE, storage, stored, Error, Media, Result, Stored, COLUMNS, EXTERNAL};
use bilibi_authz::SystemScope;
use bilibi_database::{Change, MutationTransaction};
use bilibi_notifications::{Notice, MAX_RECIPIENTS};
use bilibi_queue::execution::{Execution, Handler, HandlerFuture, Outcome};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, sync::Arc, time::Duration};
use tokio_postgres::GenericClient;

/// The system principal scans run as. Media holds [`SCAN`] for it in every
/// tenant (`StandingGrant`); a tenant administrator may deny it, and that
/// tenant's files then stay `pending`.
pub const PRINCIPAL: &str = "media-scanner";
pub const SCAN: &str = "media.file.scan";
pub(crate) const WORKER: &str = "base/media-scan";
pub(crate) const HANDLER: &str = "base/media.scan";
/// Queue's backoff (2, 4, … 256 seconds) gives a scanner about eight minutes
/// to come back before its files are marked `failed`.
pub(crate) const MAX_ATTEMPTS: i32 = 8;
pub(crate) const POLICY: &str = "media.scan.policy";
pub(crate) const TIMEOUT: &str = "media.scan.timeout_seconds";

/// What a scanner says about one file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Verdict {
    Clean,
    /// `signature` names what was found, as the scanner reports it.
    Infected {
        signature: String,
    },
    /// No answer now (the daemon is down, the service refused); the job retries.
    Unavailable,
}
/// All a scanner learns besides the bytes: never the tenant, the record, the
/// filename or who uploaded it.
#[derive(Clone, Copy, Debug)]
pub struct ScanFacts<'a> {
    /// The type libmagic detected when the file was stored.
    pub mime_type: Option<&'a str>,
    pub size: u64,
}
pub type ScanFuture<'a> = Pin<Box<dyn Future<Output = Verdict> + Send + 'a>>;
/// Implemented by the Extension that adapts a scanner (a ClamAV daemon, a
/// vendor service) and bound once with `Media::bind_scanner` from its service
/// factory. `scan` runs outside every transaction and is cut off after
/// `media.scan.timeout_seconds`; do network IO through `base/integration` or
/// a local socket, with the connection settings and secrets your own.
pub trait Scanner: Send + Sync {
    /// Kept with each verdict as evidence: 1–64 printable characters.
    fn name(&self) -> &str;
    /// The engine and signature version now; it may change between calls.
    fn version(&self) -> String;
    /// The largest file it accepts. A larger file is never sent: it is
    /// `failed` under the `required` policy and `unscanned` otherwise.
    fn max_bytes(&self) -> Option<u64> {
        None
    }
    fn scan<'a>(&'a self, bytes: &'a [u8], facts: ScanFacts<'a>) -> ScanFuture<'a>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScanState {
    Unscanned,
    Pending,
    Clean,
    Infected,
    Failed,
}
impl ScanState {
    /// An unknown stored value reads as `failed`: it is never served.
    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "unscanned" => Self::Unscanned,
            "pending" => Self::Pending,
            "clean" => Self::Clean,
            "infected" => Self::Infected,
            _ => Self::Failed,
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unscanned => "unscanned",
            Self::Pending => "pending",
            Self::Clean => "clean",
            Self::Infected => "infected",
            Self::Failed => "failed",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Policy {
    /// Scan when a scanner is installed; without one, store and serve as before.
    WhenAvailable,
    /// Refuse uploads without a scanner, and never serve an unscanned file.
    Required,
}

/// Scanner names and versions are evidence shown to people: printable and bounded.
fn label(text: &str) -> Option<String> {
    let text = text.trim();
    (!text.is_empty() && text.chars().count() <= 64 && !text.chars().any(char::is_control))
        .then(|| text.to_owned())
}
fn signature(text: &str) -> String {
    let cleaned: String = text.chars().filter(|c| !c.is_control()).take(255).collect();
    match cleaned.trim() {
        "" => "unnamed".into(),
        trimmed => trimmed.to_owned(),
    }
}
/// What the scan facts say, for audit and agents: never filename, key or bytes.
pub(crate) fn verdict(row: &Stored) -> Value {
    json!({"scan_state":row.asset.scan_state.as_str(),"scanner":row.scanner,"scanner_version":row.scanner_version,"signature":row.signature,"reason":row.reason,"purged":row.purged})
}

pub(crate) fn arguments(args: Value) -> bilibi_database::Result<Value> {
    match args.as_object() {
        Some(o)
            if o.len() == 1
                && o.get("asset_id")
                    .and_then(Value::as_i64)
                    .is_some_and(|id| id > 0) =>
        {
            Ok(args)
        }
        _ => Err(bilibi_database::Error::Invalid),
    }
}

/// How one attempt ended before anything is written.
enum Finding {
    Clean,
    Infected(String),
    TooLarge,
    NoAnswer(&'static str),
}

impl Media {
    /// Bind the installation's scanner, once, from the Extension's service
    /// factory. A second scanner, or a name that is not 1–64 printable
    /// characters, refuses boot.
    pub fn bind_scanner(&self, scanner: Arc<dyn Scanner>) -> bilibi_module_registry::Result<()> {
        if label(scanner.name()).is_none() {
            return Err(bilibi_module_registry::RegistryError::Service(
                "invalid media scanner name".into(),
            ));
        }
        self.scanner.set(scanner).map_err(|_| {
            bilibi_module_registry::RegistryError::Service(
                "a media scanner is already bound; an installation has one".into(),
            )
        })
    }
    /// Whether a scanner is bound, for an owner's page ("Checking file…").
    pub fn scanning(&self) -> bool {
        self.scanner.get().is_some()
    }

    pub(crate) async fn policy(&self, client: &(impl GenericClient + Sync)) -> Result<Policy> {
        match self.setting(client, POLICY).await?.as_str() {
            Some("required") => Ok(Policy::Required),
            Some("when_available") => Ok(Policy::WhenAvailable),
            _ => Err(Error::InvalidRules),
        }
    }
    /// The state a new file starts in. Linked (`external`) files are never
    /// stored here, so there is nothing to scan.
    pub(crate) async fn initial_state(
        &self,
        client: &(impl GenericClient + Sync),
        disk: &str,
    ) -> Result<ScanState> {
        if disk == EXTERNAL {
            return Ok(ScanState::Unscanned);
        }
        if self.scanning() {
            return Ok(ScanState::Pending);
        }
        match self.policy(client).await? {
            Policy::Required => Err(Error::ScanningRequired),
            Policy::WhenAvailable => Ok(ScanState::Unscanned),
        }
    }
    /// The one rule every read of stored bytes passes: streams, links,
    /// derivatives and attachment downloads.
    pub(crate) async fn readable(
        &self,
        client: &(impl GenericClient + Sync),
        row: &Stored,
    ) -> Result<()> {
        if row.purged {
            return Err(Error::Quarantined);
        }
        match row.asset.scan_state {
            ScanState::Clean => Ok(()),
            ScanState::Unscanned => match self.policy(client).await? {
                Policy::Required => Err(Error::NotScanned),
                Policy::WhenAvailable => Ok(()),
            },
            ScanState::Pending => Err(Error::ScanPending),
            ScanState::Infected => Err(Error::Quarantined),
            ScanState::Failed => Err(Error::ScanFailed),
        }
    }

    /// Queue the scan in the transaction that stores or rescans the file, so
    /// the row never commits without its job. The job runs as `media-scanner`;
    /// a refusal here poisons the caller's transaction.
    pub(crate) async fn enqueue_scan(
        &self,
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        id: i64,
    ) -> Result<()> {
        let enqueued = async {
            let system = SystemScope::rehydrate(tx.sql(), tenant, Some(PRINCIPAL)).await?;
            self.queue
                .enqueue_system_within(tx, &system, WORKER, json!({"asset_id": id}))
                .await
        }
        .await;
        if let Err(error) = enqueued {
            tx.mutate(move |_| Box::pin(async move { Err::<((), Vec<Change>), _>(error) }))
                .await?;
        }
        Ok(())
    }

    /// One attempt of the job: read the bytes, ask the scanner with no
    /// connection held, then record the verdict under the job's fenced lease.
    async fn scan(&self, execution: &Execution, id: i64) -> Result<Outcome> {
        let tenant = execution.tenant();
        let client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let row = client
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_media_assets WHERE id=$1 AND tenant_id=$2"),
                &[&id, &tenant],
            )
            .await?
            .map(|row| stored(&row))
            .ok_or(Error::NotFound)?;
        // Decided already (an earlier attempt, a rescan's newer job): idempotent.
        if row.asset.scan_state != ScanState::Pending {
            return Ok(Outcome::Completed);
        }
        let policy = self.policy(&**client).await?;
        let timeout = self
            .setting(&**client, TIMEOUT)
            .await?
            .as_u64()
            .ok_or(Error::InvalidRules)?;
        let roots = self.disks(&**client).await?;
        drop(client);
        let scanner = self.scanner.get().cloned();
        let (finding, name, version) = match &scanner {
            None => (Finding::NoAnswer("scanner_unavailable"), None, None),
            Some(scanner) => {
                let size = row.asset.file_size.unwrap_or_default().max(0) as u64;
                let finding = if scanner.max_bytes().is_some_and(|max| size > max) {
                    Finding::TooLarge
                } else {
                    match storage::read(&roots, &row.disk, &row.key).await {
                        Err(_) => Finding::NoAnswer("file_unavailable"),
                        Ok(bytes) => {
                            let facts = ScanFacts {
                                mime_type: row.asset.mime_type.as_deref(),
                                size: bytes.len() as u64,
                            };
                            match tokio::time::timeout(
                                Duration::from_secs(timeout),
                                scanner.scan(&bytes, facts),
                            )
                            .await
                            {
                                Ok(Verdict::Clean) => Finding::Clean,
                                Ok(Verdict::Infected { signature: found }) => {
                                    Finding::Infected(signature(&found))
                                }
                                Ok(Verdict::Unavailable) | Err(_) => {
                                    Finding::NoAnswer("scanner_unavailable")
                                }
                            }
                        }
                    }
                };
                (
                    finding,
                    label(scanner.name()),
                    label(&scanner.version()).or_else(|| Some("unknown".into())),
                )
            }
        };
        let (state, reason, found) = match finding {
            Finding::Clean => (ScanState::Clean, None, None),
            Finding::Infected(found) => (ScanState::Infected, None, Some(found)),
            Finding::TooLarge => match policy {
                Policy::Required => (ScanState::Failed, Some("too_large"), None),
                Policy::WhenAvailable => (ScanState::Unscanned, Some("too_large"), None),
            },
            Finding::NoAnswer(code) if execution.attempt < execution.max_attempts => {
                return Ok(Outcome::Retry(code))
            }
            Finding::NoAnswer(code) => (ScanState::Failed, Some(code), None),
        };
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        // `begin` fences the lease and re-proves the principal's grant.
        let mut tx = execution.begin(&mut client, "media.scan.verdict").await?;
        let locked = tx
            .sql()
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_media_assets WHERE id=$1 AND tenant_id=$2 FOR UPDATE"),
                &[&id, &tenant],
            )
            .await?
            .map(|row| stored(&row))
            .ok_or(Error::NotFound)?;
        if locked.asset.scan_state != ScanState::Pending {
            tx.rollback().await?;
            return Ok(Outcome::Completed);
        }
        let old = verdict(&locked);
        let (state_text, signature) = (state.as_str(), found.clone());
        let updated = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE base_media_assets SET scan_state=$2,scanner=$3,scanner_version=$4,scan_signature=$5,scan_reason=$6,scanned_at=now(),updated_at=now() WHERE id=$1 RETURNING {COLUMNS}"),
                            &[&id, &state_text, &name, &version, &signature, &reason],
                        )
                        .await?;
                    let row = stored(&row);
                    let change = Change { kind: "base/media/asset".into(), id, old, new: verdict(&row) };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        if state == ScanState::Infected {
            let scope = execution.scope().ok_or(Error::NotFound)?;
            self.notify_quarantined(&mut tx, scope, &updated).await?;
        }
        tx.commit().await?;
        Ok(Outcome::Completed)
    }

    /// Tell the uploader and the tenant's quarantine managers, in the
    /// verdict's transaction. Only live accounts are told; nobody live is
    /// nobody told, never a failed verdict.
    async fn notify_quarantined(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        row: &Stored,
    ) -> Result<()> {
        let tenant = scope.tenant();
        let uploader = match tx
            .sql()
            .query_opt(
                "SELECT uploaded_by_id FROM base_media_attachments WHERE media_asset_id=$1 AND tenant_id=$2",
                &[&row.asset.id, &tenant],
            )
            .await?
        {
            Some(attachment) => Some(attachment.get::<_, i64>(0)),
            None => row.created_by,
        };
        let mut live = Vec::new();
        let limit = i64::try_from(MAX_RECIPIENTS).unwrap_or(500);
        let managers = bilibi_authz::holders(tx.sql(), tenant, MANAGE, limit).await?;
        for user in uploader.into_iter().chain(managers) {
            if !live.contains(&user)
                && self
                    .authentication
                    .authenticate(tx.sql(), tenant, user, String::new())
                    .await
                    .is_ok()
            {
                live.push(user);
            }
        }
        let name: String = row
            .asset
            .original_filename
            .as_deref()
            .unwrap_or("A file")
            .chars()
            .take(120)
            .collect();
        let found = row.signature.as_deref().unwrap_or("unnamed");
        let (told_uploader, managers): (Vec<i64>, Vec<i64>) =
            live.into_iter().partition(|user| Some(*user) == uploader);
        if !told_uploader.is_empty() {
            let notice = Notice::new("A file you uploaded was quarantined")
                .kind("media.quarantined")
                .icon("shield-exclamation")
                .body(format!("“{name}” failed a malware check, so nobody can open it. Upload a clean copy instead."));
            self.notifications
                .notify(tx, scope, &told_uploader, &notice)
                .await?;
        }
        if !managers.is_empty() {
            let notice = Notice::new("A file was quarantined")
                .kind("media.quarantined")
                .icon("shield-exclamation")
                .url(crate::web::PATH)
                .body(format!("“{name}” failed a malware check ({found})."));
            self.notifications
                .notify(tx, scope, &managers, &notice)
                .await?;
        }
        Ok(())
    }
}

/// The `base/media-scan` job's bridge to Media.
pub(crate) struct ScanJob {
    pub media: Arc<Media>,
}
impl Handler for ScanJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args.get("asset_id").and_then(Value::as_i64) else {
                return Outcome::Cancel("invalid_asset_reference");
            };
            match self.media.scan(execution, id).await {
                Ok(outcome) => outcome,
                Err(Error::NotFound) => Outcome::Cancel("asset_unavailable"),
                Err(_) => Outcome::Retry("scan_store_unavailable"),
            }
        })
    }
}
