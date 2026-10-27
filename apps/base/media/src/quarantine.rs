//! The tenant's files that are not clean (issue 141, ADR 08): what the
//! quarantine page and the `base.media.quarantine.*` agent operations read
//! and act on. No Bilimbi counterpart. Rescan queues a new scan; purge
//! removes a quarantined or failed file's bytes and keeps its row and audit
//! as evidence. Both recheck `admin.media.quarantine.manage` inside the
//! caller's transaction.
use crate::{
    scan::{verdict, ScanState},
    storage, stored, Error, Media, Result, COLUMNS,
};
use bilibi_authz::authorize_key;
use bilibi_database::{Change, MutationTransaction};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use tokio_postgres::GenericClient;

pub const VIEW: &str = "admin.media.quarantine.view";
pub const MANAGE: &str = "admin.media.quarantine.manage";
/// Most rows one read returns, newest first.
pub const MAX_LISTED: i64 = 1_000;
const SELECT: &str = "SELECT a.id,a.original_filename,a.mime_type,a.file_size,a.scan_state,a.scanner,a.scanner_version,a.scan_signature,a.scan_reason,floor(extract(epoch FROM a.scanned_at))::bigint AS scanned_at,floor(extract(epoch FROM a.bytes_purged_at))::bigint AS purged_at,floor(extract(epoch FROM a.created_at))::bigint AS created_at,coalesce(t.uploaded_by_id,a.created_by_id) AS uploaded_by_id,t.subject_type,t.subject_id FROM base_media_assets a LEFT JOIN base_media_attachments t ON t.media_asset_id=a.id AND t.tenant_id=a.tenant_id WHERE a.tenant_id=$1 AND a.disk<>'external'";

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
    /// `scanner_unavailable` or `file_unavailable`.
    pub reason: Option<String>,
    pub scanned_at: Option<i64>,
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
            "scanned_at": self.scanned_at, "purged_at": self.purged_at, "created_at": self.created_at,
            "uploaded_by_id": self.uploaded_by_id,
            "subject": self.subject.as_ref().map(|(kind, id)| json!({"type": kind, "id": id})),
        })
    }
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
        purged_at: row.get("purged_at"),
        created_at: row.get("created_at"),
        uploaded_by_id: row.get("uploaded_by_id"),
        subject: row
            .get::<_, Option<String>>("subject_type")
            .zip(row.get::<_, Option<String>>("subject_id")),
    }
}

impl Media {
    /// This tenant's stored files in `states`, newest first, at most `limit`
    /// (1–1,000). Needs `admin.media.quarantine.view`.
    pub async fn quarantine(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        states: &[ScanState],
        limit: i64,
    ) -> Result<Vec<QuarantinedFile>> {
        authorize_key(client, scope, VIEW).await?;
        if !(1..=MAX_LISTED).contains(&limit) || states.is_empty() {
            return Err(Error::InvalidRules);
        }
        let states: Vec<&str> = states.iter().map(|state| state.as_str()).collect();
        let rows = client
            .query(
                &format!("{SELECT} AND a.scan_state=ANY($2) ORDER BY a.id DESC LIMIT $3"),
                &[&scope.tenant(), &states, &limit],
            )
            .await?;
        Ok(rows.iter().map(listed).collect())
    }
    /// One stored file of this tenant, whatever its state, for a review
    /// before rescan or purge. Needs `admin.media.quarantine.view`.
    pub async fn quarantined_file(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<QuarantinedFile> {
        authorize_key(client, scope, VIEW).await?;
        client
            .query_opt(&format!("{SELECT} AND a.id=$2"), &[&scope.tenant(), &id])
            .await?
            .map(|row| listed(&row))
            .ok_or(Error::NotFound)
    }

    /// Scan a file again: one that failed, was quarantined (a scanner can be
    /// wrong), was stored unscanned or is stuck pending. Needs
    /// `admin.media.quarantine.manage` and a bound scanner; joins the
    /// caller's transaction, so the row and its job commit together.
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
        if row.purged {
            return Err(Error::Purged);
        }
        if row.asset.scan_state == ScanState::Clean {
            return Err(Error::AlreadyClean);
        }
        let old = verdict(&row);
        let updated = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE base_media_assets SET scan_state='pending',scanner=NULL,scanner_version=NULL,scan_signature=NULL,scan_reason=NULL,scanned_at=NULL,updated_at=now() WHERE id=$1 RETURNING {COLUMNS}"),
                            &[&id],
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
        self.enqueue_scan(tx, scope.tenant(), id).await?;
        Ok(verdict(&updated))
    }

    /// Remove a quarantined or failed file's bytes for good. The row, its
    /// verdict and the audit trail stay as evidence (a submitted attachment
    /// keeps its binding), and a repeat is a no-op. The bytes go while the
    /// row is locked, before the commit: they are never read again, so a
    /// rollback only leaves a row a repeat purge completes.
    pub async fn purge(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        id: i64,
    ) -> Result<Value> {
        authorize_key(tx.sql(), scope, MANAGE).await?;
        let row = self.lock_stored(tx, scope.tenant(), id).await?;
        if row.purged {
            return Ok(verdict(&row));
        }
        if !matches!(
            row.asset.scan_state,
            ScanState::Infected | ScanState::Failed
        ) {
            return Err(Error::NotPurgeable);
        }
        let roots = self.disks(tx.sql()).await?;
        storage::delete(&roots, &row.disk, &row.key)
            .await
            .map_err(|_| Error::CleanupFailed)?;
        let old = verdict(&row);
        let updated = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("UPDATE base_media_assets SET bytes_purged_at=now(),updated_at=now() WHERE id=$1 RETURNING {COLUMNS}"),
                            &[&id],
                        )
                        .await?;
                    let row = stored(&row);
                    let change = Change { kind: "base/media/asset".into(), id, old, new: verdict(&row) };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        Ok(verdict(&updated))
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
