//! The tenant's files that are not clean (issue 141, ADR 12): what the
//! quarantine page and the `base.media.quarantine.*` agent operations read
//! and act on. Rescan queues a new scan. Purge
//! commits the request with the `base/media-purge` job, which removes a
//! quarantined or failed file's bytes once that commit stands and records
//! that it did; the row and its audit stay as evidence. Rescan and purge
//! recheck `admin.media.quarantine.manage` inside the caller's transaction.
use crate::{
    scan::{verdict, ScanState},
    storage, stored, Error, Media, Result, COLUMNS,
};
use bilibi_authz::{authorize_key, authorize_proof};
use bilibi_database::{Change, MutationTransaction};
use bilibi_queue::{Execution, Handler, HandlerFuture, Outcome};
use bilibi_tenancy::{ReadClient, Scope};
use serde_json::{json, Value};
use std::sync::Arc;

pub const VIEW: &str = "admin.media.quarantine.view";
pub const MANAGE: &str = "admin.media.quarantine.manage";
/// The system principal purged bytes are removed as. Media holds [`REMOVE`]
/// for it in every tenant (`StandingGrant`); a tenant that denies it keeps
/// purged files' bytes on disk, unreadable.
pub(crate) const PURGER: &str = "media-purger";
pub(crate) const REMOVE: &str = "media.file.purge";
pub(crate) const WORKER: &str = "base/media-purge";
pub(crate) const HANDLER: &str = "base/media.purge";
/// Queue's backoff (2, 4, … 2,048 seconds) retries a removal for about an
/// hour; after that the file stays purging until a manager purges it again.
pub(crate) const MAX_ATTEMPTS: i32 = 12;
/// Most rows one read returns, newest first.
pub const MAX_LISTED: i64 = 1_000;
const LISTED: &str = "a.id,a.original_filename,a.mime_type,a.file_size,a.scan_state,a.scanner,a.scanner_version,a.scan_signature,a.scan_reason,floor(extract(epoch FROM a.scanned_at))::bigint AS scanned_at,floor(extract(epoch FROM a.purge_requested_at))::bigint AS purge_requested_at,floor(extract(epoch FROM a.bytes_purged_at))::bigint AS purged_at,floor(extract(epoch FROM a.created_at))::bigint AS created_at,coalesce(t.uploaded_by_id,a.created_by_id) AS uploaded_by_id,t.subject_type,t.subject_id";
const FROM: &str = "FROM base_media_assets a LEFT JOIN base_media_attachments t ON t.media_asset_id=a.id AND t.tenant_id=a.tenant_id WHERE a.tenant_id=$1 AND a.disk<>'external'";
const MATCHING: &str =
    " AND a.scan_state=ANY($2) AND ($3::text IS NULL OR a.original_filename ILIKE $3)";

/// One stored file that is not clean, as a quarantine manager sees it: never
/// its disk, key or bytes.
#[derive(Clone, Debug)]
pub struct QuarantinedFile {
    pub id: i64,
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
    pub state: ScanState,
    pub scanner: Option<String>,
    pub scanner_version: Option<String>,
    pub signature: Option<String>,
    /// Why a file is `failed` or `unscanned`: `too_large`,
    /// `scanner_unavailable`, `file_unavailable` or `scan_error`.
    pub reason: Option<String>,
    pub scanned_at: Option<i64>,
    /// When a manager purged it; its bytes are removed after that commits.
    pub purge_requested_at: Option<i64>,
    /// When its bytes were removed.
    pub purged_at: Option<i64>,
    pub created_at: i64,
    /// The account that stored it, when known.
    pub uploaded_by_id: Option<i64>,
    /// The record a private attachment belongs to (`subject type`, `id`).
    pub subject: Option<(String, String)>,
}
impl QuarantinedFile {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "filename": self.original_filename, "mime_type": self.mime_type,
            "file_size": self.file_size, "scan_state": self.state.as_str(), "scanner": self.scanner,
            "scanner_version": self.scanner_version, "signature": self.signature, "reason": self.reason,
            "scanned_at": self.scanned_at, "purge_requested_at": self.purge_requested_at,
            "purged_at": self.purged_at, "created_at": self.created_at,
            "uploaded_by_id": self.uploaded_by_id,
            "subject": self.subject.as_ref().map(|(kind, id)| json!({"type": kind, "id": id})),
        })
    }
}

/// One read of the quarantine: the states, part of a file name (any case;
/// empty matches every file) and a window, newest first.
#[derive(Clone, Copy, Debug)]
pub struct QuarantineQuery<'a> {
    pub states: &'a [ScanState],
    pub search: &'a str,
    pub offset: i64,
    /// 1–1,000.
    pub limit: i64,
}
/// The files in a query's window, and how many match it in all.
#[derive(Clone, Debug)]
pub struct QuarantinePage {
    pub files: Vec<QuarantinedFile>,
    pub total: i64,
}

fn listed(row: &tokio_postgres::Row) -> QuarantinedFile {
    QuarantinedFile {
        id: row.get("id"),
        original_filename: row.get("original_filename"),
        mime_type: row.get("mime_type"),
        file_size: row.get("file_size"),
        state: ScanState::parse(row.get("scan_state")),
        scanner: row.get("scanner"),
        scanner_version: row.get("scanner_version"),
        signature: row.get("scan_signature"),
        reason: row.get("scan_reason"),
        scanned_at: row.get("scanned_at"),
        purge_requested_at: row.get("purge_requested_at"),
        purged_at: row.get("purged_at"),
        created_at: row.get("created_at"),
        uploaded_by_id: row.get("uploaded_by_id"),
        subject: row
            .get::<_, Option<String>>("subject_type")
            .zip(row.get::<_, Option<String>>("subject_id")),
    }
}
/// `%text%` for ILIKE, the text's own wildcards escaped; `None` for no text.
fn containing(text: &str) -> Option<String> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let mut pattern = String::with_capacity(text.len() + 2);
    pattern.push('%');
    for character in text.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    Some(pattern)
}

impl Media {
    /// This tenant's stored files in the query's states whose name contains
    /// its search, newest first, within its window, and how many match in
    /// all. Needs `admin.media.quarantine.view`. Reads on the caller's
    /// client: an agent operation passes the one its door hands it.
    pub async fn quarantine(
        &self,
        client: &dyn ReadClient,
        scope: &Scope,
        query: QuarantineQuery<'_>,
    ) -> Result<QuarantinePage> {
        authorize_proof(client, scope, VIEW).await?;
        if !(1..=MAX_LISTED).contains(&query.limit) || query.offset < 0 || query.states.is_empty() {
            return Err(Error::InvalidRules);
        }
        let states: Vec<&str> = query.states.iter().map(|state| state.as_str()).collect();
        let search = containing(query.search);
        let total = client
            .query_opt(
                &format!("SELECT count(*) {FROM}{MATCHING}"),
                &[&scope.tenant(), &states, &search],
            )
            .await?
            .map_or(0, |row| row.get(0));
        let rows = client
            .query(
                &format!("SELECT {LISTED} {FROM}{MATCHING} ORDER BY a.id DESC LIMIT $4 OFFSET $5"),
                &[
                    &scope.tenant(),
                    &states,
                    &search,
                    &query.limit,
                    &query.offset,
                ],
            )
            .await?;
        Ok(QuarantinePage {
            files: rows.iter().map(listed).collect(),
            total,
        })
    }
    /// One stored file of this tenant, whatever its state, for a review
    /// before rescan or purge. Needs `admin.media.quarantine.view`. Reads on
    /// the caller's client: a plan's preview passes the transaction it holds,
    /// so concurrent plans cannot exhaust the pool waiting on each other.
    pub async fn quarantined_file(
        &self,
        client: &dyn ReadClient,
        scope: &Scope,
        id: i64,
    ) -> Result<QuarantinedFile> {
        authorize_proof(client, scope, VIEW).await?;
        client
            .query_opt(
                &format!("SELECT {LISTED} {FROM} AND a.id=$2"),
                &[&scope.tenant(), &id],
            )
            .await?
            .map(|row| listed(&row))
            .ok_or(Error::NotFound)
    }

    /// Scan a file again: one that failed, was quarantined (a scanner can be
    /// wrong), was stored unscanned or is stuck pending. Needs
    /// `admin.media.quarantine.manage` and a bound scanner; joins the
    /// caller's transaction, so the row and its job commit together. The new
    /// scan generation means a job still in flight can no longer decide.
    pub async fn rescan(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<Value> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        if !self.scanning() {
            return Err(Error::NoScanner);
        }
        let row = self.lock_stored(tx, scope.tenant(), id).await?;
        if row.purge_requested {
            return Err(Error::Purged);
        }
        if row.asset.scan_state == ScanState::Clean {
            return Err(Error::AlreadyClean);
        }
        let old = verdict(&row);
        let tenant = scope.tenant();
        let updated = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE base_media_assets SET scan_state='pending',scan_generation=scan_generation+1,scanner=NULL,scanner_version=NULL,scan_signature=NULL,scan_reason=NULL,scanned_at=NULL,updated_at=now() WHERE id=$1 AND tenant_id=$2 RETURNING {COLUMNS}"),
                            &[&id, &tenant],
                        )
                        .await?;
                    let row = stored(&row);
                    let mut new = verdict(&row);
                    new["rescan"] = json!(true);
                    let change = Change { kind: "base/media/asset".into(), id, old, new };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        self.enqueue_scan(tx, tenant, id, updated.generation)
            .await?;
        Ok(verdict(&updated))
    }

    /// Purge a quarantined or failed file: nobody may open or rescan it
    /// again, and the `base/media-purge` job removes its bytes once this
    /// commits. The row, its verdict and the audit trail stay as evidence (a
    /// submitted attachment keeps its binding). Joins the caller's
    /// transaction and touches no file, so a rollback leaves the row and the
    /// bytes as they were. A repeat after the removal is a no-op; while the
    /// removal is still outstanding (a storage outage outlasted the job's
    /// attempts) a repeat queues it again.
    pub async fn purge(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<Value> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let row = self.lock_stored(tx, scope.tenant(), id).await?;
        let tenant = scope.tenant();
        let removal = json!({"asset_id": id});
        if row.purged {
            return Ok(verdict(&row));
        }
        if row.purge_requested {
            self.enqueue_as(tx, tenant, PURGER, WORKER, removal).await?;
            return Ok(verdict(&row));
        }
        if !matches!(
            row.asset.scan_state,
            ScanState::Infected | ScanState::Failed
        ) {
            return Err(Error::NotPurgeable);
        }
        let old = verdict(&row);
        let updated = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE base_media_assets SET purge_requested_at=now(),updated_at=now() WHERE id=$1 AND tenant_id=$2 RETURNING {COLUMNS}"),
                            &[&id, &tenant],
                        )
                        .await?;
                    let row = stored(&row);
                    let change = Change { kind: "base/media/asset".into(), id, old, new: verdict(&row) };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        self.enqueue_as(tx, tenant, PURGER, WORKER, removal).await?;
        Ok(verdict(&updated))
    }

    /// One attempt of the purge job: remove the bytes of a file whose purge
    /// committed, then record the removal, under the job's fenced lease and
    /// the row's lock. A failed removal or record retries, and bytes already
    /// gone count as removed, so a retry after a lost record finishes it.
    async fn remove_purged(&self, execution: &Execution, id: i64) -> Result<Outcome> {
        let tenant = execution.tenant();
        let mut client = self.db.get().await.map_err(bilibi_database::Error::from)?;
        let roots = self.disks(&**client).await?;
        // `begin` fences the lease and re-proves the principal's grant.
        let mut tx = execution.begin(&mut client, "media.purge.removal").await?;
        let row = self.lock_stored(&mut tx, tenant, id).await?;
        if row.purged || !row.purge_requested {
            tx.rollback().await?;
            return Ok(Outcome::Completed);
        }
        if storage::delete(&roots, &row.disk, &row.key).await.is_err() {
            tx.rollback().await?;
            return Ok(Outcome::Retry("storage_unavailable"));
        }
        let old = verdict(&row);
        tx.mutate(move |sql| {
            Box::pin(async move {
                let row = sql
                    .query_one(
                        &format!("UPDATE base_media_assets SET bytes_purged_at=now(),updated_at=now() WHERE id=$1 AND tenant_id=$2 RETURNING {COLUMNS}"),
                        &[&id, &tenant],
                    )
                    .await?;
                let change = Change { kind: "base/media/asset".into(), id, old, new: verdict(&stored(&row)) };
                Ok(((), vec![change]))
            })
        })
        .await?;
        tx.commit().await?;
        Ok(Outcome::Completed)
    }

    async fn lock_stored(
        &self,
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        id: i64,
    ) -> Result<crate::Stored> {
        tx.sql()
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_media_assets WHERE id=$1 AND tenant_id=$2 AND disk<>'external' FOR UPDATE"),
                &[&id, &tenant],
            )
            .await?
            .map(|row| stored(&row))
            .ok_or(Error::NotFound)
    }
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

/// The `base/media-purge` job's bridge to Media.
pub(crate) struct PurgeJob {
    pub media: Arc<Media>,
}
impl Handler for PurgeJob {
    fn perform<'a>(&'a self, execution: &'a Execution, args: Value) -> HandlerFuture<'a> {
        Box::pin(async move {
            let Some(id) = args.get("asset_id").and_then(Value::as_i64) else {
                return Outcome::Cancel("invalid_asset_reference");
            };
            match self.media.remove_purged(execution, id).await {
                Ok(outcome) => outcome,
                Err(Error::NotFound) => Outcome::Cancel("asset_unavailable"),
                Err(_) => Outcome::Retry("purge_store_unavailable"),
            }
        })
    }
}
