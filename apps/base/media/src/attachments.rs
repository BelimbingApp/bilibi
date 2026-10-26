//! Private subject attachments. A file a user uploads for a record of
//! someone else's module (an expense receipt, a ticket photo) lives as a
//! `private` asset bound to a `(subject type, subject id)` by one attachment
//! row: first a draft, then submitted evidence the owner keeps.
//!
//! The owning module never exposes the bytes itself. Every operation asks its
//! registered `SubjectOwner` for the live subject and the matching
//! `SubjectAction`, scoped to the caller's tenant, and a public id (ULID) from
//! another tenant is simply absent. Filesystem and database work cannot share a
//! rollback, so the calls that write or remove bytes take a plain connection;
//! `submit_attachments`/`retain_drafts` only change rows and compose into the
//! owner's own `MutationTransaction`.
use crate::{
    audit, storage, upload, Error, Media, NewAsset, Placement, Result, Rules, Stored, StreamFile,
    SubjectAction, Upload,
};
use bilibi_database::{Change, Context, MutationTransaction, Principal};
use bilibi_personal_data::{Disclosure, Erasure, Holds};
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::time::Duration;
use tokio_postgres::{Client, GenericClient, Row};

/// Never more than this many attachments in one submit or retain.
const MAX_BIND: usize = 200;
const CLEANUP_BATCH: i64 = 500;
const CLEANUP_PRINCIPAL: &str = "media.attachments.cleanup";

/// The record a file belongs to, named as the owner registered it
/// (`orders.order`, the order's id). Callers' ids are requests, never proof.
#[derive(Clone, Copy, Debug)]
pub struct AttachmentSubject<'a> {
    pub kind: &'a str,
    pub id: &'a str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttachmentState {
    Draft,
    Submitted,
}
impl AttachmentState {
    fn parse(text: &str) -> Result<Self> {
        match text {
            "draft" => Ok(Self::Draft),
            "submitted" => Ok(Self::Submitted),
            _ => Err(Error::InvalidAsset),
        }
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
        }
    }
}

/// What an owner may see: no disk, key or asset id. `public_id` is the ULID the
/// owner stores and later submits or downloads by.
#[derive(Clone, Debug)]
pub struct Attachment {
    pub public_id: String,
    pub subject_type: String,
    pub subject_id: String,
    pub state: AttachmentState,
    pub uploaded_by_id: i64,
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
    pub submitted_at: Option<i64>,
    pub retained_at: Option<i64>,
    pub created_at: i64,
}

/// Outcome of a cleanup run. A file that could not be removed after its rows
/// committed is counted, never hidden: the operator reconciles the disk.
#[derive(Clone, Copy, Debug, Default)]
pub struct Cleanup {
    pub removed: u64,
    pub file_failures: u64,
}

struct Bound {
    id: i64,
    asset_id: i64,
    summary: Attachment,
}
const SELECT: &str = "SELECT a.id,a.media_asset_id,a.public_id,a.subject_type,a.subject_id,a.state,a.uploaded_by_id,s.original_filename,s.mime_type,s.file_size,floor(extract(epoch FROM a.submitted_at))::bigint AS submitted_at,floor(extract(epoch FROM a.retained_at))::bigint AS retained_at,floor(extract(epoch FROM a.created_at))::bigint AS created_at FROM base_media_attachments a JOIN base_media_assets s ON s.id=a.media_asset_id AND s.tenant_id=a.tenant_id";
fn bound(row: &Row) -> Result<Bound> {
    Ok(Bound {
        id: row.get("id"),
        asset_id: row.get("media_asset_id"),
        summary: Attachment {
            public_id: row.get("public_id"),
            subject_type: row.get("subject_type"),
            subject_id: row.get("subject_id"),
            state: AttachmentState::parse(row.get("state"))?,
            uploaded_by_id: row.get("uploaded_by_id"),
            original_filename: row.get("original_filename"),
            mime_type: row.get("mime_type"),
            file_size: row.get("file_size"),
            submitted_at: row.get("submitted_at"),
            retained_at: row.get("retained_at"),
            created_at: row.get("created_at"),
        },
    })
}
/// Audit facts name the binding, never the bytes, key or filename.
fn facts(summary: &Attachment) -> Value {
    json!({"public_id":summary.public_id,"subject_type":summary.subject_type,"subject_id":summary.subject_id,"state":summary.state.as_str(),"uploaded_by_id":summary.uploaded_by_id,"submitted_at":summary.submitted_at,"retained_at":summary.retained_at})
}
/// The only accepted spelling: 26 uppercase Crockford base32 characters.
fn valid_public_id(id: &str) -> bool {
    ulid::Ulid::from_string(id).is_ok_and(|parsed| parsed.to_string() == id)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Bind {
    Submit,
    Retain,
}

impl Media {
    /// Bytes are only ever bound through the caller's own transaction context.
    /// An impersonated session is refused: the row cannot name the operator.
    fn acting_user(tx: &MutationTransaction<'_>, scope: &Scope) -> Result<()> {
        if tx.context().tenant != scope.tenant()
            || tx.context().actor.kind != "user"
            || tx.context().actor.id != scope.user().to_string()
            || tx.context().impersonator.is_some()
        {
            return Err(bilibi_database::Error::Forbidden.into());
        }
        Ok(())
    }

    /// Validate, authorize `Upload`, store the bytes privately and create a
    /// draft naming the sealed user. `rules` may narrow the operator's size and
    /// type limits for this subject, never widen them. The owner is asked again
    /// inside the registering transaction, after the bytes were read.
    pub async fn upload_attachment(
        &self,
        client: &mut Client,
        scope: &Scope,
        subject: AttachmentSubject<'_>,
        file: Upload,
        rules: Rules,
    ) -> Result<Attachment> {
        let AttachmentSubject { kind, id } = subject;
        self.subjects
            .authorize(&*client, scope, kind, SubjectAction::Upload, id)
            .await?;
        let (max_bytes, types) = self.limits(&*client).await?;
        let inspected = upload::inspect(
            &file,
            true,
            &rules,
            upload::Limits {
                max_bytes,
                attachment_types: &types,
            },
        )
        .await?;
        let disk = self
            .setting(&*client, "media.attachment_disk")
            .await?
            .as_str()
            .map(str::to_owned)
            .ok_or(Error::StorageNotConfigured)?;
        let key = format!(
            "tenants/{}/attachments/{}",
            scope.tenant(),
            uuid::Uuid::new_v4()
        );
        let roots = self.disks(&*client).await?;
        storage::write(&roots, &disk, &key, &file.bytes).await?;
        let result = async {
            let mut tx = self
                .boundary
                .begin(client, scope.mutation_context("media.attachment.upload"))
                .await?;
            self.subjects
                .authorize(tx.sql(), scope, kind, SubjectAction::Upload, id)
                .await?;
            let asset = self
                .register_original(
                    &mut tx,
                    scope,
                    Placement {
                        disk: &disk,
                        key: &key,
                    },
                    NewAsset {
                        original_filename: Some(inspected.filename),
                        mime_type: Some(inspected.mime_type),
                        file_size: Some(inspected.size),
                        metadata: None,
                        private: true,
                    },
                )
                .await?;
            let (tenant, user, subject_type, subject_id, public_id) = (
                scope.tenant(),
                scope.user(),
                kind.to_owned(),
                id.to_owned(),
                ulid::Ulid::generate().to_string(),
            );
            let summary = tx
                .mutate(move |sql| {
                    Box::pin(async move {
                        let row = sql
                            .query_one(
                                "INSERT INTO base_media_attachments(public_id,tenant_id,media_asset_id,subject_type,subject_id,state,uploaded_by_type,uploaded_by_id) VALUES($1,$2,$3,$4,$5,'draft','user',$6) RETURNING id",
                                &[&public_id, &tenant, &asset.id, &subject_type, &subject_id, &user],
                            )
                            .await?;
                        let row_id: i64 = row.get("id");
                        let summary = Attachment {
                            public_id,
                            subject_type,
                            subject_id,
                            state: AttachmentState::Draft,
                            uploaded_by_id: user,
                            original_filename: asset.original_filename,
                            mime_type: asset.mime_type,
                            file_size: asset.file_size,
                            submitted_at: None,
                            retained_at: None,
                            created_at: asset.created_at,
                        };
                        let change = Change {
                            kind: "base/media/attachment".into(),
                            id: row_id,
                            old: json!({}),
                            new: facts(&summary),
                        };
                        Ok((summary, vec![change]))
                    })
                })
                .await?;
            tx.commit().await?;
            Ok(summary)
        }
        .await;
        if result.is_err() {
            let _ = storage::delete(&roots, &disk, &key).await;
        }
        result
    }

    /// Bind drafts to the subject as submitted evidence. Repeating it for an
    /// already submitted attachment is a no-op; evidence never changes again.
    /// Joins the owner's transaction, so a rollback undoes the binding.
    pub async fn submit_attachments(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        subject: AttachmentSubject<'_>,
        public_ids: &[&str],
    ) -> Result<Vec<Attachment>> {
        self.bind(tx, scope, subject, public_ids, Bind::Submit)
            .await
    }
    /// Mark drafts as kept so abandoned-draft cleanup spares them. Submitted
    /// attachments pass through unchanged.
    pub async fn retain_drafts(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        subject: AttachmentSubject<'_>,
        public_ids: &[&str],
    ) -> Result<Vec<Attachment>> {
        self.bind(tx, scope, subject, public_ids, Bind::Retain)
            .await
    }
    async fn bind(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        subject: AttachmentSubject<'_>,
        public_ids: &[&str],
        operation: Bind,
    ) -> Result<Vec<Attachment>> {
        Self::acting_user(tx, scope)?;
        let mut ids = public_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        if ids.len() > MAX_BIND || !ids.iter().all(|id| valid_public_id(id)) {
            return Err(Error::AttachmentUnavailable);
        }
        // Stable lock order (by row id) keeps two concurrent binds deadlock-free.
        let rows = tx
            .sql()
            .query(
                &format!("{SELECT} WHERE a.tenant_id=$1 AND a.public_id=ANY($2) ORDER BY a.id FOR UPDATE OF a"),
                &[&scope.tenant(), &ids],
            )
            .await?;
        let rows: Vec<Bound> = rows.iter().map(bound).collect::<Result<_>>()?;
        if rows.len() != ids.len() {
            return Err(Error::AttachmentUnavailable);
        }
        if rows.iter().any(|row| {
            row.summary.subject_type != subject.kind || row.summary.subject_id != subject.id
        }) {
            return Err(Error::WrongSubject);
        }
        let action = match operation {
            Bind::Submit => SubjectAction::Submit,
            Bind::Retain => SubjectAction::Retain,
        };
        self.subjects
            .authorize(tx.sql(), scope, subject.kind, action, subject.id)
            .await?;
        let changing: Vec<&Bound> = rows
            .iter()
            .filter(|row| {
                row.summary.state == AttachmentState::Draft
                    && (operation == Bind::Submit || row.summary.retained_at.is_none())
            })
            .collect();
        if changing.is_empty() {
            return Ok(rows.into_iter().map(|row| row.summary).collect());
        }
        let before: Vec<(i64, Value)> = changing
            .iter()
            .map(|row| (row.id, facts(&row.summary)))
            .collect();
        let row_ids: Vec<i64> = changing.iter().map(|row| row.id).collect();
        let all: Vec<i64> = rows.iter().map(|row| row.id).collect();
        tx.mutate(move |sql| {
            Box::pin(async move {
                let statement = match operation {
                    Bind::Submit => "UPDATE base_media_attachments SET state='submitted',submitted_at=now(),updated_at=now() WHERE id=ANY($1)",
                    Bind::Retain => "UPDATE base_media_attachments SET retained_at=now(),updated_at=now() WHERE id=ANY($1)",
                };
                sql.execute(statement, &[&row_ids]).await?;
                let updated = sql
                    .query(&format!("{SELECT} WHERE a.id=ANY($1) ORDER BY a.id"), &[&all])
                    .await?;
                let updated: Vec<Bound> = updated
                    .iter()
                    .map(bound)
                    .collect::<Result<_>>()
                    .map_err(|_| bilibi_database::Error::Internal)?;
                let changes = before
                    .into_iter()
                    .filter_map(|(id, old)| {
                        updated.iter().find(|row| row.id == id).map(|row| Change {
                            kind: "base/media/attachment".into(),
                            id,
                            old,
                            new: facts(&row.summary),
                        })
                    })
                    .collect();
                Ok((updated.into_iter().map(|row| row.summary).collect(), changes))
            })
        })
        .await
        .map_err(Into::into)
    }

    /// Bytes for one attachment, after the subject's owner allows `Download` for
    /// this scope *now*. A public id from another tenant is `NotFound`. Send the
    /// result with `StreamFile::delivery(true)`: private files always download.
    pub async fn download_attachment(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        public_id: &str,
    ) -> Result<StreamFile> {
        if !valid_public_id(public_id) {
            return Err(Error::NotFound);
        }
        let row = client
            .query_opt(
                &format!("{SELECT} WHERE a.tenant_id=$1 AND a.public_id=$2"),
                &[&scope.tenant(), &public_id],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let row = bound(&row)?;
        let summary = &row.summary;
        self.subjects
            .authorize(
                client,
                scope,
                &summary.subject_type,
                SubjectAction::Download,
                &summary.subject_id,
            )
            .await?;
        let asset = self.fetch(client, scope.tenant(), row.asset_id).await?;
        if !asset.private {
            return Err(Error::InvalidAsset);
        }
        let bytes = storage::read(&self.disks(client).await?, &asset.disk, &asset.key).await?;
        Ok(StreamFile {
            bytes,
            filename: asset
                .asset
                .original_filename
                .unwrap_or_else(|| asset.key.rsplit('/').next().unwrap_or("download").into()),
            mime_type: asset.asset.mime_type,
        })
    }

    /// Remove a draft its uploader no longer wants. Only the uploader may, only
    /// while the owner still allows `Discard`, and never submitted evidence.
    pub async fn discard_draft(
        &self,
        client: &mut Client,
        scope: &Scope,
        public_id: &str,
    ) -> Result<()> {
        if !valid_public_id(public_id) {
            return Err(Error::NotFound);
        }
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("media.attachment.discard"))
            .await?;
        let row = tx
            .sql()
            .query_opt(
                &format!("{SELECT} WHERE a.tenant_id=$1 AND a.public_id=$2 FOR UPDATE OF a"),
                &[&scope.tenant(), &public_id],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let row = bound(&row)?;
        if row.summary.state != AttachmentState::Draft {
            return Err(Error::ImmutableAttachment);
        }
        if row.summary.uploaded_by_id != scope.user() {
            return Err(Error::NotUploader);
        }
        self.subjects
            .authorize(
                tx.sql(),
                scope,
                &row.summary.subject_type,
                SubjectAction::Discard,
                &row.summary.subject_id,
            )
            .await?;
        let file = self.remove_draft(&mut tx, scope.tenant(), &row).await?;
        tx.commit().await?;
        self.remove_files(client, &[file]).await
    }

    /// Delete the draft's rows and name its file. Both rows go in one statement
    /// order the foreign key allows: binding first, then its bytes' record.
    async fn remove_draft(
        &self,
        tx: &mut MutationTransaction<'_>,
        tenant: i64,
        row: &Bound,
    ) -> Result<(String, String)> {
        let asset = self.fetch(tx.sql(), tenant, row.asset_id).await?;
        let Stored { disk, key, .. } = &asset;
        let (attachment_id, asset_id) = (row.id, row.asset_id);
        let changes = vec![
            Change {
                kind: "base/media/attachment".into(),
                id: attachment_id,
                old: facts(&row.summary),
                new: json!({}),
            },
            Change {
                kind: "base/media/asset".into(),
                id: asset_id,
                old: audit(&asset),
                new: json!({}),
            },
        ];
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute(
                    "DELETE FROM base_media_attachments WHERE id=$1",
                    &[&attachment_id],
                )
                .await?;
                sql.execute("DELETE FROM base_media_assets WHERE id=$1", &[&asset_id])
                    .await?;
                Ok(((), changes))
            })
        })
        .await?;
        Ok((disk.clone(), key.clone()))
    }
    /// Rows are already committed; a file that will not go is reported so the
    /// operator can reconcile the disk, and never resurrects the draft.
    async fn remove_files(&self, client: &Client, files: &[(String, String)]) -> Result<()> {
        let roots = self.disks(client).await?;
        let mut failed = false;
        for (disk, key) in files {
            failed |= storage::delete(&roots, disk, key).await.is_err();
        }
        if failed {
            return Err(Error::CleanupFailed);
        }
        Ok(())
    }

    /// Remove one tenant's abandoned drafts: unretained, not submitted, older
    /// than `older_than`. Trusted callers only (an operator command or a
    /// worker); there is no user to authorize, so the writes are attributed to
    /// the system principal `media.attachments.cleanup` in the named tenant.
    /// Each candidate is locked and rechecked, so a draft submitted, retained or
    /// discarded meanwhile survives.
    pub async fn cleanup_drafts(
        &self,
        client: &mut Client,
        tenant: i64,
        older_than: Duration,
    ) -> Result<Cleanup> {
        if older_than.is_zero() {
            return Err(Error::InvalidRules);
        }
        let live = client
            .query_opt(
                "SELECT 1 FROM tenants WHERE id=$1 AND deleted_at IS NULL",
                &[&tenant],
            )
            .await?;
        if live.is_none() {
            return Err(Error::NotFound);
        }
        let age = older_than.as_secs_f64();
        let mut report = Cleanup::default();
        loop {
            let candidates = client
                .query(
                    "SELECT public_id FROM base_media_attachments WHERE tenant_id=$1 AND state='draft' AND retained_at IS NULL AND created_at < now()-make_interval(secs => $2) ORDER BY id LIMIT $3",
                    &[&tenant, &age, &CLEANUP_BATCH],
                )
                .await?;
            if candidates.is_empty() {
                break;
            }
            let before = report.removed;
            for candidate in &candidates {
                let public_id: String = candidate.get("public_id");
                let context = Context {
                    tenant,
                    actor: Principal {
                        kind: "system".into(),
                        id: CLEANUP_PRINCIPAL.into(),
                    },
                    impersonator: None,
                    action: "media.attachment.cleanup".into(),
                    trace_id: uuid::Uuid::new_v4().to_string(),
                    agent_self_report: None,
                    client_identity: None,
                    agent_connection: None,
                };
                let mut tx = self.boundary.begin(client, context).await?;
                let row = tx
                    .sql()
                    .query_opt(
                        &format!("{SELECT} WHERE a.tenant_id=$1 AND a.public_id=$2 AND a.state='draft' AND a.retained_at IS NULL AND a.created_at < now()-make_interval(secs => $3) FOR UPDATE OF a"),
                        &[&tenant, &public_id, &age],
                    )
                    .await?;
                let Some(row) = row else {
                    continue;
                };
                let file = self.remove_draft(&mut tx, tenant, &bound(&row)?).await?;
                tx.commit().await?;
                report.removed += 1;
                if self.remove_files(client, &[file]).await.is_err() {
                    report.file_failures += 1;
                }
            }
            // A pass that removed nothing would only see the same rows again.
            if report.removed == before {
                break;
            }
        }
        Ok(report)
    }
}

/// The audit record kinds of an attachment row and of its asset.
pub const ATTACHMENT_KIND: &str = "base/media/attachment";
pub const ASSET_KIND: &str = "base/media/asset";
/// Attachments read per page when a request lists a subject's files.
const DISCLOSE_PAGE: i64 = 500;

/// One attachment an erasure turned into a tombstone. Its row and its removed
/// asset are both audit records whose evidence the erasure clears.
#[derive(Clone, Debug)]
pub struct ErasedAttachment {
    pub public_id: String,
    /// The attachment row, kind [`ATTACHMENT_KIND`].
    pub id: i64,
    /// The removed asset row, kind [`ASSET_KIND`].
    pub asset_id: i64,
}
/// What `erase_attachments` did.
#[derive(Clone, Debug, Default)]
pub struct AttachmentErasure {
    pub erased: Vec<ErasedAttachment>,
    /// Attachments a legal hold kept: (attachment row, hold).
    pub held: Vec<(i64, i64)>,
}
/// What one `remove_erased_bytes` call did.
#[derive(Clone, Copy, Debug, Default)]
pub struct Removal {
    /// Files removed by this call; each is one audited change.
    pub removed: u64,
    /// Files still waiting: call again (another call may be removing them now).
    pub more: bool,
}

impl Media {
    /// How many live attachments `subject` has, after its owner allows
    /// `Download` for this scope now.
    pub async fn count_attachments(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        subject: AttachmentSubject<'_>,
    ) -> Result<i64> {
        self.subjects
            .authorize(
                client,
                scope,
                subject.kind,
                SubjectAction::Download,
                subject.id,
            )
            .await?;
        Ok(client
            .query_one(
                "SELECT count(*) FROM base_media_attachments WHERE tenant_id=$1 AND subject_type=$2 AND subject_id=$3 AND state<>'erased'",
                &[&scope.tenant(), &subject.kind, &subject.id],
            )
            .await?
            .get(0))
    }

    /// Every live attachment of `subject` (ADR 05), however many, for an
    /// approved access request. The grant is the authority: the subject's
    /// owner is not asked, so a retired record's files are still listed.
    pub async fn disclosed_attachments(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        grant: &Disclosure,
        subject: AttachmentSubject<'_>,
    ) -> Result<Vec<Attachment>> {
        grant.check(scope)?;
        let (mut all, mut after) = (Vec::new(), 0_i64);
        loop {
            let rows = client
                .query(
                    &format!("{SELECT} WHERE a.tenant_id=$1 AND a.subject_type=$2 AND a.subject_id=$3 AND a.id>$4 ORDER BY a.id LIMIT $5"),
                    &[&scope.tenant(), &subject.kind, &subject.id, &after, &DISCLOSE_PAGE],
                )
                .await?;
            let done = (rows.len() as i64) < DISCLOSE_PAGE;
            for row in &rows {
                let row = bound(row)?;
                after = row.id;
                all.push(row.summary);
            }
            if done {
                return Ok(all);
            }
        }
    }

    /// The bytes of one of `subject`'s live attachments, for an approved
    /// access request's bundle. Another subject's file is `NotFound`.
    pub async fn disclosed_attachment(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        grant: &Disclosure,
        subject: AttachmentSubject<'_>,
        public_id: &str,
    ) -> Result<StreamFile> {
        grant.check(scope)?;
        if !valid_public_id(public_id) {
            return Err(Error::NotFound);
        }
        let row = client
            .query_opt(
                &format!("{SELECT} WHERE a.tenant_id=$1 AND a.public_id=$2 AND a.subject_type=$3 AND a.subject_id=$4"),
                &[&scope.tenant(), &public_id, &subject.kind, &subject.id],
            )
            .await?
            .ok_or(Error::NotFound)?;
        let row = bound(&row)?;
        let asset = self.fetch(client, scope.tenant(), row.asset_id).await?;
        let bytes = storage::read(&self.disks(client).await?, &asset.disk, &asset.key).await?;
        Ok(StreamFile {
            bytes,
            filename: asset
                .asset
                .original_filename
                .unwrap_or_else(|| public_id.to_owned()),
            mime_type: asset.asset.mime_type,
        })
    }

    /// Erasure (ADR 05): turn every live attachment of `subject` that no hold
    /// names into a tombstone and remove its asset row, in the caller's
    /// erasure transaction. The grant is the authority, so no `SubjectAction`
    /// is asked; the database admits the change only through
    /// `base_media_erase_attachment`. The asset's change records its filename
    /// as `[erased]`; the caller clears the earlier evidence of both records.
    /// Bytes go afterwards, through [`Media::remove_erased_bytes`]. Repeating
    /// it finds nothing left to erase.
    pub async fn erase_attachments(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        grant: &Erasure,
        subject: AttachmentSubject<'_>,
        holds: &Holds,
    ) -> Result<AttachmentErasure> {
        grant.check(scope)?;
        if tx.context().tenant != scope.tenant() {
            return Err(Error::Database(bilibi_database::Error::Forbidden));
        }
        let rows = tx
            .sql()
            .query(
                &format!("{SELECT} WHERE a.tenant_id=$1 AND a.subject_type=$2 AND a.subject_id=$3 ORDER BY a.id FOR UPDATE OF a"),
                &[&scope.tenant(), &subject.kind, &subject.id],
            )
            .await?;
        let rows = rows.iter().map(bound).collect::<Result<Vec<_>>>()?;
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        let held = holds.records(ATTACHMENT_KIND, &ids).await?;
        let mut done = AttachmentErasure::default();
        for row in rows {
            if let Some(hold) = held.get(&row.id) {
                done.held.push((row.id, *hold));
                continue;
            }
            let asset = self.fetch(tx.sql(), scope.tenant(), row.asset_id).await?;
            let mut old_asset = audit(&asset);
            if old_asset
                .get("original_filename")
                .is_some_and(|name| !name.is_null())
            {
                old_asset["original_filename"] = json!(bilibi_personal_data::ERASED);
            }
            let (tenant, id, asset_id, user) = (scope.tenant(), row.id, row.asset_id, scope.user());
            let changes = vec![
                Change {
                    kind: ATTACHMENT_KIND.into(),
                    id,
                    old: facts(&row.summary),
                    new: json!({"public_id": row.summary.public_id, "subject_type": row.summary.subject_type,
                        "subject_id": row.summary.subject_id, "state": "erased", "uploaded_by_id": row.summary.uploaded_by_id}),
                },
                Change {
                    kind: ASSET_KIND.into(),
                    id: asset_id,
                    old: old_asset,
                    new: json!({}),
                },
            ];
            tx.mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "SELECT base_media_erase_attachment($1,$2,$3)",
                        &[&tenant, &id, &user],
                    )
                    .await?;
                    Ok(((), changes))
                })
            })
            .await?;
            done.erased.push(ErasedAttachment {
                public_id: row.summary.public_id.clone(),
                id,
                asset_id,
            });
        }
        Ok(done)
    }

    /// Removes the bytes of `subject`'s erased attachments, one file per
    /// transaction: each file is claimed under its row lock, removed, and
    /// recorded once, so concurrent calls never remove or record one file
    /// twice. A file already gone counts as removed. Removes at most one batch;
    /// `more` says files remain, and a file that will not go is
    /// `CleanupFailed`, left pending for the next call.
    pub async fn remove_erased_bytes(
        &self,
        client: &mut Client,
        scope: &Scope,
        grant: &Erasure,
        subject: AttachmentSubject<'_>,
    ) -> Result<Removal> {
        grant.check(scope)?;
        let roots = self.disks(&*client).await?;
        let mut removal = Removal::default();
        for _ in 0..CLEANUP_BATCH {
            let mut tx = self
                .boundary
                .begin(
                    client,
                    scope.mutation_context("media.attachment.bytes_removed"),
                )
                .await?;
            let Some(row) = tx
                .sql()
                .query_opt(
                    "SELECT id,erased_disk,erased_key FROM base_media_attachments WHERE tenant_id=$1 AND subject_type=$2 AND subject_id=$3 \
                     AND state='erased' AND bytes_removed_at IS NULL ORDER BY id LIMIT 1 FOR UPDATE SKIP LOCKED",
                    &[&scope.tenant(), &subject.kind, &subject.id],
                )
                .await?
            else {
                tx.rollback().await?;
                removal.more = client
                    .query_one(
                        "SELECT EXISTS (SELECT 1 FROM base_media_attachments WHERE tenant_id=$1 AND subject_type=$2 AND subject_id=$3 \
                         AND state='erased' AND bytes_removed_at IS NULL)",
                        &[&scope.tenant(), &subject.kind, &subject.id],
                    )
                    .await?
                    .get(0);
                return Ok(removal);
            };
            let (id, disk, key): (i64, String, String) = (row.get(0), row.get(1), row.get(2));
            if storage::delete(&roots, &disk, &key).await.is_err() {
                tx.rollback().await?;
                return Err(Error::CleanupFailed);
            }
            tx.mutate(move |sql| {
                Box::pin(async move {
                    sql.execute(
                        "UPDATE base_media_attachments SET bytes_removed_at=now() WHERE id=$1 AND bytes_removed_at IS NULL",
                        &[&id],
                    )
                    .await?;
                    let change = Change {
                        kind: ATTACHMENT_KIND.into(),
                        id,
                        old: json!({"bytes": "present"}),
                        new: json!({"bytes": "removed"}),
                    };
                    Ok(((), vec![change]))
                })
            })
            .await?;
            tx.commit().await?;
            removal.removed += 1;
        }
        removal.more = true;
        Ok(removal)
    }
}
