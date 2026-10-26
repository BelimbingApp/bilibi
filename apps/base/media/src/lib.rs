//! Media: private asset storage, content-checked uploads, signed streams, the
//! trusted subject-owner seam and private subject attachments. Owners call this API after authorizing their
//! own workflow; it is not a browser endpoint. Read `README.md` before use.
//!
//! Bytes live on operator-mapped private disks and metadata in
//! `base_media_assets`. Filesystem work and database work cannot share a
//! rollback, so every writing call that touches bytes owns its transaction and
//! takes a plain connection: the type system keeps callers from wrapping a file
//! write in a transaction they might roll back. `register_*` only records
//! metadata for bytes an owner already produced, and composes into the owner's
//! `MutationTransaction`.
mod attachments;
mod contributions;
mod owner;
mod response;
mod signing;
mod storage;
mod upload;

pub use attachments::{
    Attachment, AttachmentErasure, AttachmentState, AttachmentSubject, Cleanup, ErasedAttachment,
    Removal, ASSET_KIND, ATTACHMENT_KIND,
};
pub use owner::{Subject, SubjectAction, SubjectOwner, Subjects, SUBJECTS};
pub use response::{Delivery, StreamFile};
pub use signing::Signer;
pub use storage::EXTERNAL;
pub use upload::{Rules, Upload};

use bilibi_database::{Change, MutationBoundary, MutationTransaction};
use bilibi_module_registry::{Catalog, Service};
use bilibi_settings::Settings;
use bilibi_tenancy::Scope;
use serde_json::{json, Value};
use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};
use tokio_postgres::{Client, GenericClient};

pub type Result<T> = std::result::Result<T, Error>;

/// Reasons never carry paths, keys or file contents.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("The storage key is unsafe")]
    UnsafeStorageKey,
    #[error("The storage disk is not configured")]
    StorageNotConfigured,
    #[error("The storage root must be an absolute, private (0700), symlink-free directory outside served trees")]
    UnsafeStorageRoot,
    #[error("The storage key already exists")]
    StorageKeyExists,
    #[error("The stored file is unavailable")]
    StorageUnavailable,
    #[error("The file is empty or larger than allowed")]
    InvalidSize,
    #[error("This file type is not allowed")]
    DisallowedType,
    #[error("File type detection is unavailable on this host")]
    MimeDetectionUnavailable,
    #[error("The upload could not be inspected")]
    InvalidUpload,
    #[error("The upload rules are invalid")]
    InvalidRules,
    #[error("Private attachment files cannot be used through the generic asset API")]
    PrivateAsset,
    #[error("The asset is not stored by this application")]
    SourceNotStored,
    #[error("The external link is not a valid HTTP(S) URL")]
    InvalidExternalUrl,
    #[error("Derivative kinds must be named and are never \"original\"")]
    InvalidDerivativeKind,
    #[error("The asset metadata is invalid")]
    InvalidAsset,
    #[error("The link is invalid")]
    InvalidToken,
    #[error("The link has expired")]
    ExpiredToken,
    #[error("Signed links are unavailable: set BILIBI_MEDIA_SIGNING_KEY")]
    SigningUnavailable,
    #[error("Not found")]
    NotFound,
    #[error("The record was removed, but some files could not be deleted; reconcile the disk")]
    CleanupFailed,
    #[error("No registered owner for this subject")]
    UnknownSubject,
    #[error("One or more attachments are unavailable")]
    AttachmentUnavailable,
    #[error("The attachments belong to a different subject")]
    WrongSubject,
    #[error("Submitted attachments cannot be changed or removed")]
    ImmutableAttachment,
    #[error("Only the uploader may discard a draft")]
    NotUploader,
    #[error(transparent)]
    Database(#[from] bilibi_database::Error),
    #[error("Database unavailable")]
    Postgres(#[from] tokio_postgres::Error),
}

/// Public view of an asset: no disk and no storage key.
#[derive(Clone, Debug)]
pub struct Asset {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
    pub metadata: Option<Value>,
    pub created_at: i64,
    pub updated_at: i64,
}
/// Attributes an owner supplies when registering bytes it already produced.
/// `private` marks the bytes of a private attachment: only the attachment
/// lifecycle (`attachments.rs`) should set it, and it hides the whole tree from
/// generic access.
#[derive(Clone, Debug, Default)]
pub struct NewAsset {
    pub original_filename: Option<String>,
    pub mime_type: Option<String>,
    pub file_size: Option<i64>,
    pub metadata: Option<Value>,
    pub private: bool,
}

/// Where bytes live: a configured disk (or the `external` sentinel) and a key.
#[derive(Clone, Copy, Debug)]
pub struct Placement<'a> {
    pub disk: &'a str,
    pub key: &'a str,
}
/// Names the original a derivative belongs to.
#[derive(Clone, Copy, Debug)]
pub struct Derivative<'a> {
    pub parent_id: i64,
    pub kind: &'a str,
}

struct Stored {
    asset: Asset,
    tenant: i64,
    disk: String,
    key: String,
    private: bool,
}
const COLUMNS: &str = "id,tenant_id,parent_id,disk,storage_key,original_filename,mime_type,file_size,kind,private,metadata,floor(extract(epoch FROM created_at))::bigint AS created_at,floor(extract(epoch FROM updated_at))::bigint AS updated_at";
fn stored(row: &tokio_postgres::Row) -> Stored {
    Stored {
        asset: Asset {
            id: row.get("id"),
            parent_id: row.get("parent_id"),
            kind: row.get("kind"),
            original_filename: row.get("original_filename"),
            mime_type: row.get("mime_type"),
            file_size: row.get("file_size"),
            metadata: row.get("metadata"),
            created_at: row.get("created_at"),
            updated_at: row.get("updated_at"),
        },
        tenant: row.get("tenant_id"),
        disk: row.get("disk"),
        key: row.get("storage_key"),
        private: row.get("private"),
    }
}
/// Audit facts name the asset, never its bytes, key or free-form metadata.
fn audit(row: &Stored) -> Value {
    json!({"kind":row.asset.kind,"parent_id":row.asset.parent_id,"disk":row.disk,"original_filename":row.asset.original_filename,"mime_type":row.asset.mime_type,"file_size":row.asset.file_size,"private":row.private})
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

pub struct Media {
    settings: Arc<Settings>,
    boundary: Arc<MutationBoundary>,
    signer: Signer,
    subjects: Subjects,
}
impl Media {
    pub fn new(
        settings: Arc<Settings>,
        boundary: Arc<MutationBoundary>,
        signer: Signer,
        subjects: Subjects,
    ) -> Self {
        Self {
            settings,
            boundary,
            signer,
            subjects,
        }
    }

    async fn setting(&self, client: &(impl GenericClient + Sync), key: &str) -> Result<Value> {
        Ok(self.settings.get_global(client, key).await?)
    }
    async fn disks(&self, client: &(impl GenericClient + Sync)) -> Result<Value> {
        self.setting(client, "media.disks").await
    }
    async fn limits(&self, client: &(impl GenericClient + Sync)) -> Result<(u64, Vec<String>)> {
        let max = self
            .setting(client, "media.max_bytes")
            .await?
            .as_u64()
            .ok_or(Error::InvalidRules)?;
        let types = self
            .setting(client, "media.attachment_allowed_types")
            .await?
            .as_array()
            .map(|types| {
                types
                    .iter()
                    .filter_map(|kind| kind.as_str().map(str::to_owned))
                    .collect()
            })
            .ok_or(Error::InvalidRules)?;
        Ok((max, types))
    }

    /// Row lookup scoped to the tenant: another tenant's id is simply absent.
    async fn fetch(
        &self,
        client: &(impl GenericClient + Sync),
        tenant: i64,
        id: i64,
    ) -> Result<Stored> {
        client
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_media_assets WHERE id=$1 AND tenant_id=$2"),
                &[&id, &tenant],
            )
            .await?
            .map(|row| stored(&row))
            .ok_or(Error::NotFound)
    }
    /// True when this asset, any ancestor or any descendant is private. A scoped
    /// miss would expose attachment bytes, so the walk never filters by tenant;
    /// composite foreign keys keep a tree inside one tenant.
    async fn private_tree(&self, client: &(impl GenericClient + Sync), id: i64) -> Result<bool> {
        let row = client
            .query_one(
                "WITH RECURSIVE down AS (SELECT id,private FROM base_media_assets WHERE id=$1 UNION ALL SELECT a.id,a.private FROM base_media_assets a JOIN down d ON a.parent_id=d.id), up AS (SELECT id,parent_id,private FROM base_media_assets WHERE id=$1 UNION ALL SELECT a.id,a.parent_id,a.private FROM base_media_assets a JOIN up u ON a.id=u.parent_id) SELECT coalesce((SELECT bool_or(private) FROM down),false) OR coalesce((SELECT bool_or(private) FROM up),false)",
                &[&id],
            )
            .await?;
        Ok(row.get(0))
    }
    async fn public(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<Stored> {
        let row = self.fetch(client, scope.tenant(), id).await?;
        if self.private_tree(client, id).await? {
            return Err(Error::PrivateAsset);
        }
        Ok(row)
    }

    pub async fn get_asset(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<Asset> {
        Ok(self.fetch(client, scope.tenant(), id).await?.asset)
    }
    pub async fn list_derivatives(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<Vec<Asset>> {
        self.public(client, scope, id).await?;
        let rows = client
            .query(
                &format!("SELECT {COLUMNS} FROM base_media_assets WHERE parent_id=$1 AND tenant_id=$2 ORDER BY id"),
                &[&id, &scope.tenant()],
            )
            .await?;
        Ok(rows.iter().map(|row| stored(row).asset).collect())
    }

    /// Validate where bytes live without touching them. The external sentinel
    /// is only a key; every other disk must resolve to a safe private root.
    async fn place(
        &self,
        client: &(impl GenericClient + Sync),
        disk: &str,
        key: &str,
    ) -> Result<()> {
        if disk.is_empty() || disk.len() > 255 {
            return Err(Error::StorageNotConfigured);
        }
        if disk == EXTERNAL {
            return storage::validate_key(key);
        }
        storage::location(&self.disks(client).await?, disk, key)
            .await
            .map(|_| ())
    }
    fn check_attributes(new: &NewAsset) -> Result<()> {
        let short = |text: &Option<String>| text.as_ref().is_none_or(|t| t.chars().count() <= 255);
        if !short(&new.original_filename)
            || !short(&new.mime_type)
            || new.file_size.is_some_and(|size| size < 0)
            || new
                .metadata
                .as_ref()
                .is_some_and(|m| m.to_string().len() > 65_536)
        {
            return Err(Error::InvalidAsset);
        }
        Ok(())
    }
    fn matches(tx: &MutationTransaction<'_>, scope: &Scope) -> Result<()> {
        if tx.context().tenant != scope.tenant()
            || tx.context().actor.kind != "user"
            || tx.context().actor.id != scope.user().to_string()
        {
            return Err(bilibi_database::Error::Forbidden.into());
        }
        Ok(())
    }

    /// Record an original whose bytes an authorized owner already placed on
    /// `disk`. Joins the owner's transaction; touches no file.
    pub async fn register_original(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        place: Placement<'_>,
        new: NewAsset,
    ) -> Result<Asset> {
        self.register(tx, scope, None, "original", place, new).await
    }
    pub async fn register_derivative(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        of: Derivative<'_>,
        place: Placement<'_>,
        new: NewAsset,
    ) -> Result<Asset> {
        let Derivative { parent_id, kind } = of;
        if kind.is_empty() || kind.chars().count() > 255 || kind == "original" {
            return Err(Error::InvalidDerivativeKind);
        }
        // The parent lock serializes this insert against a tree delete.
        let parent = tx
            .sql()
            .query_opt(
                "SELECT id FROM base_media_assets WHERE id=$1 AND tenant_id=$2 FOR UPDATE",
                &[&parent_id, &scope.tenant()],
            )
            .await?;
        if parent.is_none() {
            return Err(Error::NotFound);
        }
        if self.private_tree(tx.sql(), parent_id).await? {
            return Err(Error::PrivateAsset);
        }
        self.register(tx, scope, Some(parent_id), kind, place, new)
            .await
    }
    async fn register(
        &self,
        tx: &mut MutationTransaction<'_>,
        scope: &Scope,
        parent: Option<i64>,
        kind: &str,
        place: Placement<'_>,
        new: NewAsset,
    ) -> Result<Asset> {
        let Placement { disk, key } = place;
        Self::matches(tx, scope)?;
        Self::check_attributes(&new)?;
        self.place(tx.sql(), disk, key).await?;
        let taken = tx
            .sql()
            .query_opt(
                "SELECT id FROM base_media_assets WHERE disk=$1 AND storage_key=$2 AND disk<>'external'",
                &[&disk, &key],
            )
            .await?;
        if taken.is_some() {
            return Err(Error::StorageKeyExists);
        }
        let (tenant, disk, key, kind) = (
            scope.tenant(),
            disk.to_owned(),
            key.to_owned(),
            kind.to_owned(),
        );
        let row = tx
            .mutate(move |sql| {
                Box::pin(async move {
                    let row = sql
                        .query_one(
                            &format!("INSERT INTO base_media_assets(tenant_id,parent_id,disk,storage_key,original_filename,mime_type,file_size,kind,private,metadata) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) RETURNING {COLUMNS}"),
                            &[&tenant, &parent, &disk, &key, &new.original_filename, &new.mime_type, &new.file_size, &kind, &new.private, &new.metadata],
                        )
                        .await?;
                    let row = stored(&row);
                    let change = Change { kind: "base/media/asset".into(), id: row.asset.id, old: json!({}), new: audit(&row) };
                    Ok((row, vec![change]))
                })
            })
            .await?;
        Ok(row.asset)
    }

    /// Inspect (size, libmagic type, active-content refusal), store under
    /// `tenants/{tenant}/{directory}/{uuid}` and register. A failed
    /// registration removes the new file. Requires a plain connection.
    pub async fn put_upload(
        &self,
        client: &mut Client,
        scope: &Scope,
        disk: &str,
        directory: &str,
        file: Upload,
        metadata: Option<Value>,
    ) -> Result<Asset> {
        storage::validate_key(directory)?;
        if disk == EXTERNAL {
            return Err(Error::SourceNotStored);
        }
        let (max_bytes, types) = self.limits(&*client).await?;
        let inspected = upload::inspect(
            &file,
            false,
            &Rules::default(),
            upload::Limits {
                max_bytes,
                attachment_types: &types,
            },
        )
        .await?;
        let key = format!(
            "tenants/{}/{directory}/{}",
            scope.tenant(),
            uuid::Uuid::new_v4()
        );
        let new = NewAsset {
            original_filename: Some(inspected.filename),
            mime_type: Some(inspected.mime_type),
            file_size: Some(inspected.size),
            metadata,
            private: false,
        };
        let place = Placement { disk, key: &key };
        self.store(client, scope, place, &file.bytes, None, new)
            .await
    }
    /// Store bytes an owner produced as a derivative of `of.parent_id`. The type
    /// is detected from the bytes; the owner's claim is not trusted.
    pub async fn put_derivative_bytes(
        &self,
        client: &mut Client,
        scope: &Scope,
        of: Derivative<'_>,
        place: Placement<'_>,
        bytes: Vec<u8>,
        metadata: Option<Value>,
    ) -> Result<Asset> {
        if of.kind.is_empty() || of.kind.chars().count() > 255 || of.kind == "original" {
            return Err(Error::InvalidDerivativeKind);
        }
        if place.disk == EXTERNAL {
            return Err(Error::SourceNotStored);
        }
        self.public(&*client, scope, of.parent_id).await?;
        let (max_bytes, types) = self.limits(&*client).await?;
        let upload = Upload {
            filename: place.key.rsplit('/').next().unwrap_or("derivative").into(),
            bytes,
        };
        let limits = upload::Limits {
            max_bytes,
            attachment_types: &types,
        };
        let inspected = upload::inspect(&upload, false, &Rules::default(), limits).await?;
        let new = NewAsset {
            original_filename: None,
            mime_type: Some(inspected.mime_type),
            file_size: Some(inspected.size),
            metadata,
            private: false,
        };
        self.store(client, scope, place, &upload.bytes, Some(of), new)
            .await
    }
    /// Shared write order: bytes first, then the audited registration in its own
    /// transaction, deleting the bytes again if anything after them fails.
    async fn store(
        &self,
        client: &mut Client,
        scope: &Scope,
        place: Placement<'_>,
        bytes: &[u8],
        derivative_of: Option<Derivative<'_>>,
        new: NewAsset,
    ) -> Result<Asset> {
        let roots = self.disks(&*client).await?;
        storage::write(&roots, place.disk, place.key, bytes).await?;
        let result = async {
            let action = if derivative_of.is_some() {
                "media.derive"
            } else {
                "media.upload"
            };
            let mut tx = self
                .boundary
                .begin(client, scope.mutation_context(action))
                .await?;
            let asset = match derivative_of {
                None => self.register_original(&mut tx, scope, place, new).await?,
                Some(of) => {
                    self.register_derivative(&mut tx, scope, of, place, new)
                        .await?
                }
            };
            tx.commit().await?;
            Ok(asset)
        }
        .await;
        if result.is_err() {
            let _ = storage::delete(&roots, place.disk, place.key).await;
        }
        result
    }

    /// Delete an asset and its derivatives. Rows commit first so a rollback can
    /// never strand live references; a file that then fails to delete reports
    /// `CleanupFailed` and needs operator reconciliation.
    pub async fn delete_asset(&self, client: &mut Client, scope: &Scope, id: i64) -> Result<()> {
        let mut tx = self
            .boundary
            .begin(client, scope.mutation_context("media.delete"))
            .await?;
        let root = tx
            .sql()
            .query_opt(
                &format!("SELECT {COLUMNS} FROM base_media_assets WHERE id=$1 AND tenant_id=$2 FOR UPDATE"),
                &[&id, &scope.tenant()],
            )
            .await?
            .map(|row| stored(&row))
            .ok_or(Error::NotFound)?;
        if self.private_tree(tx.sql(), id).await? {
            return Err(Error::PrivateAsset);
        }
        // Lock each level before discovering its children, like a derivative insert.
        let mut tree = vec![root];
        let mut frontier = vec![id];
        for depth in 0.. {
            let rows = tx
                .sql()
                .query(
                    &format!("SELECT {COLUMNS} FROM base_media_assets WHERE parent_id=ANY($1) AND tenant_id=$2 ORDER BY id FOR UPDATE"),
                    &[&frontier, &scope.tenant()],
                )
                .await?;
            if rows.is_empty() {
                break;
            }
            if depth >= 32 {
                return Err(Error::InvalidAsset);
            }
            frontier = rows.iter().map(|row| row.get("id")).collect();
            tree.extend(rows.iter().map(stored));
        }
        let ids: Vec<i64> = tree.iter().map(|row| row.asset.id).collect();
        let changes: Vec<Change> = tree
            .iter()
            .rev()
            .map(|row| Change {
                kind: "base/media/asset".into(),
                id: row.asset.id,
                old: audit(row),
                new: json!({}),
            })
            .collect();
        tx.mutate(move |sql| {
            Box::pin(async move {
                sql.execute("DELETE FROM base_media_assets WHERE id=ANY($1)", &[&ids])
                    .await?;
                Ok(((), changes))
            })
        })
        .await?;
        tx.commit().await?;
        let roots = self.disks(&*client).await?;
        let mut failed = false;
        for row in tree.iter().rev() {
            failed |= storage::delete(&roots, &row.disk, &row.key).await.is_err();
        }
        if failed {
            return Err(Error::CleanupFailed);
        }
        Ok(())
    }

    /// Safe HTTP(S) link for an `external` asset, else a signed stream path.
    pub async fn display_url(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<String> {
        let row = self.public(client, scope, id).await?;
        if row.disk == EXTERNAL {
            let url = row
                .asset
                .metadata
                .as_ref()
                .and_then(|m| m.get("public_url"))
                .and_then(Value::as_str);
            return url
                .filter(|url| safe_external_url(url))
                .map(str::to_owned)
                .ok_or(Error::InvalidExternalUrl);
        }
        self.stream_url(client, scope, id).await
    }
    /// Relative path with an expiring token. It authorizes this one public
    /// asset for its issued lifetime and is separate from private downloads.
    pub async fn stream_url(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        id: i64,
    ) -> Result<String> {
        let row = self.public(client, scope, id).await?;
        if row.disk == EXTERNAL {
            return Err(Error::SourceNotStored);
        }
        let ttl = self
            .setting(client, "media.stream_ttl_seconds")
            .await?
            .as_u64()
            .ok_or(Error::InvalidRules)?;
        let token = self.signer.sign(id, row.tenant, now() + ttl)?;
        Ok(format!("/media/assets/{id}/stream?token={token}"))
    }
    /// The unauthenticated edge. The token alone proves permission; the row,
    /// the tenant it carries and the private-tree rule are rechecked here.
    pub async fn read_signed_asset(
        &self,
        client: &(impl GenericClient + Sync),
        id: i64,
        token: &str,
    ) -> Result<StreamFile> {
        let claims = self.signer.verify(id, token, now())?;
        let row = self.fetch(client, claims.tenant, id).await?;
        if self.private_tree(client, id).await? {
            return Err(Error::PrivateAsset);
        }
        if row.disk == EXTERNAL {
            return Err(Error::SourceNotStored);
        }
        let bytes = storage::read(&self.disks(client).await?, &row.disk, &row.key).await?;
        Ok(StreamFile {
            bytes,
            filename: row
                .asset
                .original_filename
                .unwrap_or_else(|| row.key.rsplit('/').next().unwrap_or("download").into()),
            mime_type: row.asset.mime_type,
        })
    }

    /// Ask the registered owner of `subject` whether this scope may `action` it.
    pub async fn authorize_subject(
        &self,
        client: &(impl GenericClient + Sync),
        scope: &Scope,
        subject: &str,
        action: SubjectAction,
        subject_id: &str,
    ) -> Result<()> {
        self.subjects
            .authorize(client, scope, subject, action, subject_id)
            .await
    }
}

fn safe_external_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let Some(rest) = lower
        .strip_prefix("https://")
        .or_else(|| lower.strip_prefix("http://"))
    else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    url.len() <= 2048
        && !authority.is_empty()
        && !authority.contains('@')
        && url.bytes().all(|b| b.is_ascii_graphic())
}

// Erasure clears the evidence of both records (ADR 05); only the asset's
// filename is personal.
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_personal_data::RECORD_KINDS, "base/media.attachment",
bilibi_module_registry::Provenance { module: "base/media", source: file!(), line: line!() },
&bilibi_personal_data::RecordKind { kind: ATTACHMENT_KIND, fields: &[], subject_identifier: false, person: false }) }
inventory::submit! { bilibi_module_registry::Contribution::new(bilibi_personal_data::RECORD_KINDS, "base/media.asset",
bilibi_module_registry::Provenance { module: "base/media", source: file!(), line: line!() },
&bilibi_personal_data::RecordKind { kind: ASSET_KIND, fields: &["original_filename"], subject_identifier: false, person: false }) }

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/media", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/audit", "base/authz", "base/database", "base/personal_data", "base/settings", "base/tenancy"],
        migrations: &[
            ("base-media-001", include_str!("../migrations/001-media.sql")),
            ("base-media-002", include_str!("../migrations/002-attachments.sql")),
            ("base-media-003-erasure", include_str!("../migrations/003-erasure.sql")),
        ],
        services: &[Service { id: "base/media.store", factory: |services| {
            let modules = bilibi_module_registry::discover()?;
            let catalog = Catalog::discover(&modules)?;
            Ok(Arc::new(Media::new(
                services.get("base/settings.store")?,
                services.get("base/database.mutations")?,
                Signer::from_hex(std::env::var("BILIBI_MEDIA_SIGNING_KEY").ok().as_deref()),
                Subjects::from_catalog(&catalog, &modules)?,
            )))
        }}],
    }
}
