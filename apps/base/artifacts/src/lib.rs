//! Private business documents. Call outside an owner's transaction: durable
//! reservations/tombstones must commit independently of filesystem work.
mod contributions;
mod engine;
mod install;
pub mod pdf;
mod protect;
mod queued;
mod render;
mod retention;
mod storage;

pub use protect::{PdfArtifact, PdfOptions, Secret};
pub use queued::{
    validate_args as validate_render_job, CompletionFuture, PdfMode, PdfRenderHandler, PdfRequest,
    QueuedPdfOwner, Rendered, RequestFuture, RENDER_WORKER,
};
pub use retention::{PurgeHold, Retention, RetentionEvent};

use bilibi_database::{Change, Db, MutationBoundary, MutationTransaction};
use bilibi_module_registry::Service;
use bilibi_party_ids::OrganisationId;
use bilibi_queue::Queue;
use bilibi_settings::Settings;
use bilibi_tenancy::{Authentication, ProofClient, ProofFuture, Scope};
pub use render::{HtmlFuture, HtmlOwner, Served, RENDER_HEADERS, RENDER_ROUTE};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{any::type_name, future::Future, pin::Pin, sync::Arc};
use tokio_postgres::{Client, GenericClient, Row};

pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(transparent)]
    Database(#[from] bilibi_database::Error),
    #[error("Database unavailable")]
    Sql(#[from] tokio_postgres::Error),
    #[error("Storage unavailable; reservation or tombstone retained for recovery")]
    Storage(#[from] std::io::Error),
    #[error("Storage root must be an existing private 0700 absolute directory")]
    UnsafeRoot,
    #[error("The storage folder {0} does not exist. Check the drive is connected, or change Private document folder in Settings.")]
    MissingRoot(String),
    #[error("Configure storage and a positive retention period first")]
    Policy,
    #[error("Invalid artifact input")]
    Invalid,
    #[error("Invalid PDF document structure")]
    InvalidDocument,
    #[error("Text contains characters the built-in PDF font cannot show")]
    UnsupportedText,
    #[error("Not a structurally valid PDF")]
    InvalidPdf,
    #[error("PDF engine unavailable; configure a Chromium executable in Settings")]
    PdfEngineUnavailable,
    #[error("PDF rendering failed")]
    PdfFailed,
    #[error("PDF protection unavailable; configure a qpdf executable in Settings")]
    PdfProtectionUnavailable,
    #[error("PDF protection failed")]
    PdfProtectionFailed,
    #[error("A PDF password must be 1-127 bytes of printable text")]
    InvalidPassword,
    #[error("PDF rendering timed out")]
    PdfTimeout,
    #[error("Configure pdf.render_origin before printing through a signed render URL")]
    RenderOrigin,
    #[error("Too many pending render requests")]
    RenderTokensFull,
    #[error("Artifact not found")]
    NotFound,
    #[error("Artifact integrity failure")]
    Integrity,
    #[error("Physical cleanup pending")]
    CleanupPending,
    #[error("Artifact is under legal hold")]
    LegalHold,
    #[error("Artifact is retained until its retain-until time")]
    Retained,
    #[error("Retention needs a future time and a bounded reason")]
    InvalidRetention,
    #[error("Retain-until must move later")]
    RetentionNotExtended,
    #[error("Artifact is not retained")]
    NotRetained,
    #[error("Artifact is already on legal hold")]
    AlreadyOnHold,
    #[error("Artifact is not on legal hold")]
    NotOnHold,
    #[error("Private bytes are still present")]
    BytesPresent,
}
impl Error {
    /// Stable, content-free label kept as a purge attempt's last error.
    pub fn code(&self) -> &'static str {
        match self {
            Self::Database(bilibi_database::Error::Forbidden) => "forbidden",
            Self::Database(_) | Self::Sql(_) => "database",
            Self::Storage(_) | Self::CleanupPending => "storage",
            Self::UnsafeRoot => "unsafe_root",
            Self::MissingRoot(_) => "missing_root",
            Self::Policy => "policy",
            Self::Invalid | Self::InvalidDocument | Self::UnsupportedText | Self::InvalidPdf => {
                "invalid"
            }
            Self::NotFound => "not_found",
            Self::Integrity => "integrity",
            Self::LegalHold => "legal_hold",
            Self::Retained => "retained",
            Self::InvalidRetention => "invalid_retention",
            Self::RetentionNotExtended => "retention_not_extended",
            Self::NotRetained => "not_retained",
            Self::AlreadyOnHold => "already_on_hold",
            Self::NotOnHold => "not_on_hold",
            Self::BytesPresent => "bytes_present",
            Self::PdfEngineUnavailable => "pdf_engine_unavailable",
            Self::PdfFailed => "pdf_failed",
            Self::PdfProtectionUnavailable => "pdf_protection_unavailable",
            Self::PdfProtectionFailed => "pdf_protection_failed",
            Self::InvalidPassword => "invalid_password",
            Self::PdfTimeout => "pdf_timeout",
            Self::RenderOrigin => "render_origin",
            Self::RenderTokensFull => "render_tokens_full",
        }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Operation {
    Create,
    Read,
    Delete,
    Purge,
    Retain,
}
#[derive(Clone, Debug)]
pub struct Reference {
    pub subject: String,
    pub kind: String,
}
/// Select a concrete adapter in trusted server code, never from request input.
/// Check live company membership/state and current subject permissions on every
/// callback. Purge has no reference and requires company maintenance authority.
/// Base binds both this stable owner ID and the concrete Rust adapter type.
pub trait Owner: Send + Sync + 'static {
    fn owner_id(&self) -> &'static str;
    fn authorize<'a>(
        &'a self,
        client: &'a dyn ProofClient,
        scope: &'a Scope,
        company: OrganisationId,
        operation: Operation,
        reference: Option<&'a Reference>,
    ) -> ProofFuture<'a, ()>;
}
pub type RenderFuture<'a> = Pin<Box<dyn Future<Output = Result<Vec<u8>>> + Send + 'a>>;
/// Implement on an owner that generates PDFs. Base authorizes `Create` before
/// calling `render_pdf`, which selects only the data the scope's actor may see
/// from the caller's `data` and its own records, and returns complete PDF bytes
/// (use `pdf::render` for text and tables). Never log document contents.
pub trait PdfOwner: Owner {
    fn render_pdf<'a>(
        &'a self,
        scope: &'a Scope,
        company: OrganisationId,
        reference: &'a Reference,
        data: &'a Value,
    ) -> RenderFuture<'a>;
}
/// No storage location or raw database row escapes the facade.
#[derive(Clone, Debug)]
pub struct Metadata {
    pub id: String,
    pub reference: Reference,
    pub content_type: String,
    pub byte_size: i64,
    pub sha256: String,
    /// Set for an ordinary document; a records-grade one has none.
    pub expires_at: Option<String>,
    pub retain_until: Option<String>,
    pub legal_hold_at: Option<String>,
}
/// Intentionally has no Debug implementation: document contents are private.
pub struct Document {
    pub metadata: Metadata,
    pub bytes: Vec<u8>,
}
struct Stored {
    key: i64,
    metadata: Metadata,
    root: String,
    ready: bool,
    deleted: bool,
    purged: bool,
    expired: bool,
    /// Retain-until still lies in the future.
    retained: bool,
    attempts: i32,
    last_error: Option<String>,
    attempted_at: Option<String>,
    held_at: Option<String>,
}
impl Stored {
    fn on_hold(&self) -> bool {
        self.metadata.legal_hold_at.is_some()
    }
    /// A hold or an unreached retain-until refuses deletion and purge.
    fn refusal(&self) -> Option<Error> {
        if self.on_hold() {
            Some(Error::LegalHold)
        } else if self.retained {
            Some(Error::Retained)
        } else {
            None
        }
    }
}
const COLUMNS: &str = "id,public_id,subject,kind,content_type,byte_size,sha256,storage_root,expires_at::text,retain_until::text,legal_hold_at::text,ready_at IS NOT NULL AS ready,deleted_at IS NOT NULL AS deleted,purged_at IS NOT NULL AS purged,COALESCE(expires_at <= clock_timestamp(),false) AS expired,COALESCE(retain_until > clock_timestamp(),false) AS retained,purge_attempts,purge_last_error,purge_attempted_at::text,purge_held_at::text";
fn stored(row: Row) -> Stored {
    Stored {
        key: row.get("id"),
        metadata: Metadata {
            id: row.get("public_id"),
            reference: Reference {
                subject: row.get("subject"),
                kind: row.get("kind"),
            },
            content_type: row.get("content_type"),
            byte_size: row.get("byte_size"),
            sha256: row.get("sha256"),
            expires_at: row.get("expires_at"),
            retain_until: row.get("retain_until"),
            legal_hold_at: row.get("legal_hold_at"),
        },
        root: row.get("storage_root"),
        ready: row.get("ready"),
        deleted: row.get("deleted"),
        purged: row.get("purged"),
        expired: row.get("expired"),
        retained: row.get("retained"),
        attempts: row.get("purge_attempts"),
        last_error: row.get("purge_last_error"),
        attempted_at: row.get("purge_attempted_at"),
        held_at: row.get("purge_held_at"),
    }
}
fn evidence(row: &Stored) -> Value {
    // Deliberately exclude subject, kind, content, MIME, digest and path.
    json!({"artifact_id":row.metadata.id,"ready":row.ready,"deleted":row.deleted,"purged":row.purged,
        "retain_until":row.metadata.retain_until,"legal_hold":row.on_hold(),"purge_held":row.held_at.is_some()})
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn bounded(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

pub struct Artifacts {
    boundary: Arc<MutationBoundary>,
    settings: Arc<Settings>,
    authentication: Arc<Authentication>,
    tokens: render::Tokens,
    queue: Arc<Queue>,
    db: Db,
}
impl Artifacts {
    pub fn for_owner<'a, O: Owner>(
        &'a self,
        scope: &'a Scope,
        company: OrganisationId,
        owner: &'a O,
    ) -> Owned<'a, O> {
        Owned {
            store: self,
            scope,
            company,
            owner,
        }
    }
}
/// Scoped facade; the concrete adapter cannot be swapped by a client string.
pub struct Owned<'a, O: Owner> {
    store: &'a Artifacts,
    scope: &'a Scope,
    company: OrganisationId,
    owner: &'a O,
}
const PDF: &str = "application/pdf";
impl<O: PdfOwner> Owned<'_, O> {
    /// Authorize, render through the owner, validate the output and store it as
    /// a private `application/pdf` artifact. Same call rules as `put`.
    pub async fn generate_pdf(
        &self,
        client: &mut Client,
        reference: Reference,
        data: &Value,
    ) -> Result<Metadata> {
        if !data.is_object() {
            return Err(Error::Invalid);
        }
        self.authorize(&*client, Operation::Create, Some(&reference))
            .await?;
        let bytes = self
            .owner
            .render_pdf(self.scope, self.company, &reference, data)
            .await?;
        self.store_pdf(client, reference, &bytes).await
    }
}
impl<O: Owner> Owned<'_, O> {
    /// Store PDF bytes made elsewhere (for example by the HTML engine) after
    /// the same structural validation; `put` rechecks authority.
    pub async fn store_pdf(
        &self,
        client: &mut Client,
        reference: Reference,
        bytes: &[u8],
    ) -> Result<Metadata> {
        pdf::validate(bytes)?;
        self.put(client, reference, bytes, PDF).await
    }
}
impl Artifacts {
    pub(crate) async fn protect_tool(&self, client: &Client) -> Result<protect::Tool> {
        let get = |key: &'static str| async move {
            self.settings
                .get_global(client, key)
                .await
                .map_err(Error::from)
        };
        let seconds = get("pdf.qpdf.timeout_seconds").await?.as_u64();
        let max = get("artifacts.max_bytes").await?.as_u64();
        Ok(protect::Tool {
            binary: get("pdf.qpdf.binary")
                .await?
                .as_str()
                .ok_or(Error::PdfProtectionUnavailable)?
                .to_owned(),
            timeout: std::time::Duration::from_secs(
                seconds
                    .filter(|s| (1..=600).contains(s))
                    .ok_or(Error::Policy)?,
            ),
            max_bytes: max.filter(|max| *max > 0).ok_or(Error::Policy)? as usize,
        })
    }
    /// Whether the configured qpdf executable exists, so a caller can offer
    /// password protection. Protected renders fail closed when it does not.
    pub async fn protection_available(&self, client: &Client) -> bool {
        match self.protect_tool(client).await {
            Ok(tool) => engine::executable(&tool.binary).is_ok(),
            Err(_) => false,
        }
    }
}
impl<O: Owner> Owned<'_, O> {
    /// The PDF writer seam for bytes made elsewhere: validates, authorizes,
    /// optionally encrypts with AES-256 and stores through `put`. Encryption
    /// happens before publication, so a protected request never stores an
    /// unprotected copy. Passwords reach only the tool's private job file.
    pub async fn persist_pdf(
        &self,
        client: &mut Client,
        reference: Reference,
        bytes: &[u8],
        options: &PdfOptions,
    ) -> Result<PdfArtifact> {
        options.validate()?;
        pdf::validate(bytes)?;
        self.authorize(&*client, Operation::Create, Some(&reference))
            .await?;
        let metadata = match options.passwords() {
            None => self.put(client, reference, bytes, PDF).await?,
            Some((user, owner)) => {
                let tool = self.store.protect_tool(&*client).await?;
                let encrypted = protect::encrypt(&tool, bytes, user, owner).await?;
                self.put(client, reference, &encrypted, PDF).await?
            }
        };
        let produced_at = self.clock(&*client).await?;
        Ok(PdfArtifact::new(
            metadata,
            options,
            self.scope.user(),
            produced_at,
        ))
    }
    /// Stores an AES-256 protected copy of an existing PDF, leaving the source
    /// unchanged. Reads (and audits) the source, then writes like `persist_pdf`.
    pub async fn protect_with_password(
        &self,
        client: &mut Client,
        source: &PdfArtifact,
        user: &Secret,
        owner: Option<&Secret>,
    ) -> Result<PdfArtifact> {
        let document = self.read(client, &source.document.id).await?;
        if document.metadata.content_type != PDF {
            return Err(Error::InvalidPdf);
        }
        let reference = document.metadata.reference.clone();
        self.authorize(&*client, Operation::Create, Some(&reference))
            .await?;
        let tool = self.store.protect_tool(&*client).await?;
        let encrypted = protect::encrypt(&tool, &document.bytes, user, owner).await?;
        let metadata = self.put(client, reference, &encrypted, PDF).await?;
        let produced_at = self.clock(&*client).await?;
        Ok(source.encrypted_copy(metadata, self.scope.user(), produced_at))
    }
    async fn clock(&self, client: &Client) -> Result<String> {
        Ok(client
            .query_one(
                "SELECT to_char(clock_timestamp() AT TIME ZONE 'UTC','YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"')",
                &[],
            )
            .await?
            .get(0))
    }
}
#[derive(Debug, Default)]
pub struct Recovery {
    pub purged: Vec<String>,
    pub failed: Vec<(String, Error)>,
}
impl<O: Owner> Owned<'_, O> {
    async fn authorize(
        &self,
        client: &(impl GenericClient + Sync),
        operation: Operation,
        reference: Option<&Reference>,
    ) -> Result<()> {
        if !bounded(self.owner.owner_id(), 255) || type_name::<O>().len() > 1024 {
            return Err(Error::Invalid);
        }
        let live = self
            .store
            .authentication
            .reprove(client, self.scope)
            .await?;
        if live.actor().person() != self.scope.actor().person()
            || live.actor().organisation() != self.scope.actor().organisation()
            || live.actor().affiliation() != self.scope.actor().affiliation()
            || live
                .actor()
                .organisation()
                .is_some_and(|id| id != self.company)
        {
            return Err(bilibi_database::Error::Forbidden.into());
        }
        self.owner
            .authorize(client, &live, self.company, operation, reference)
            .await?;
        Ok(())
    }
    async fn fetch(&self, tx: &MutationTransaction<'_>, id: &str) -> Result<Stored> {
        self.find(tx.sql(), id, " FOR UPDATE").await
    }
    /// `lock` is a module-chosen suffix, never client text.
    async fn find(
        &self,
        sql: &(impl GenericClient + Sync),
        id: &str,
        lock: &str,
    ) -> Result<Stored> {
        sql.query_opt(&format!("SELECT {COLUMNS} FROM base_artifacts WHERE public_id=$1 AND tenant_id=$2 AND company_id=$3 AND owner_id=$4 AND owner_adapter=$5{lock}"), &[&id,&self.scope.tenant(),&self.company.get(),&self.owner.owner_id(),&type_name::<O>()]).await?.map(stored).ok_or(Error::NotFound)
    }
    async fn begin<'c>(
        &self,
        client: &'c mut Client,
        action: &str,
    ) -> Result<MutationTransaction<'c>> {
        Ok(self
            .store
            .boundary
            .begin(client, self.scope.mutation_context(action))
            .await?)
    }
    async fn audit(&self, tx: &MutationTransaction<'_>, row: &Stored, action: &str) -> Result<()> {
        self.audit_with(tx, row, action, json!({})).await
    }
    /// `details` are content-free facts such as times, counts and reason codes.
    async fn audit_with(
        &self,
        tx: &MutationTransaction<'_>,
        row: &Stored,
        action: &str,
        details: Value,
    ) -> Result<()> {
        let mut payload = json!({"artifact_id":row.metadata.id,"company_id":self.company.get(),"owner_id":self.owner.owner_id()});
        if let (Some(payload), Some(details)) = (payload.as_object_mut(), details.as_object()) {
            payload.extend(details.clone());
        }
        bilibi_audit::record_action(
            tx.sql(),
            &bilibi_audit::Attribution::from_context(tx.context())?,
            None,
            action,
            payload,
            false,
        )
        .await?;
        Ok(())
    }
    async fn transition(
        &self,
        tx: &mut MutationTransaction<'_>,
        row: &Stored,
        column: &str,
    ) -> Result<Stored> {
        // column is chosen only by this module; never accept an SQL identifier.
        let query = match column {
            "ready" => "UPDATE base_artifacts SET ready_at=clock_timestamp() WHERE id=$1",
            "deleted" => "UPDATE base_artifacts SET deleted_at=clock_timestamp() WHERE id=$1",
            "purged" => "UPDATE base_artifacts SET purged_at=clock_timestamp() WHERE id=$1",
            _ => return Err(Error::Invalid),
        };
        tx.sql().execute(query, &[&row.key]).await?;
        let next = self.fetch(tx, &row.metadata.id).await?;
        tx.record(Change {
            kind: "base/artifacts.document".into(),
            id: row.key,
            old: evidence(row),
            new: evidence(&next),
        })
        .await?;
        Ok(next)
    }
    /// Reserve metadata durably, write private bytes exclusively, then publish
    /// under the row lock. A failure/ambiguous commit leaves tracked metadata;
    /// recovery never removes bytes belonging to a ready document.
    pub async fn put(
        &self,
        client: &mut Client,
        reference: Reference,
        bytes: &[u8],
        content_type: &str,
    ) -> Result<Metadata> {
        self.store(client, reference, bytes, content_type, None, None)
            .await
    }
    /// Like `put`, for a document wanted only briefly (a prepared export): it
    /// expires after `days`, never later than `artifacts.retention_days`
    /// would, so an owner can shorten a document's life but not extend it.
    pub async fn put_expiring(
        &self,
        client: &mut Client,
        reference: Reference,
        bytes: &[u8],
        content_type: &str,
        days: i64,
    ) -> Result<Metadata> {
        if days <= 0 {
            return Err(Error::Invalid);
        }
        self.store(client, reference, bytes, content_type, None, Some(days))
            .await
    }
    /// Like `put`, but the document is records-grade from publication: it has a
    /// retain-until (and optionally a legal hold) instead of an expiry. The
    /// owner must also authorize `Operation::Retain`. Retention is applied in
    /// the publication transaction, so a failed upload stays an ordinary
    /// reservation that recovery can tombstone.
    pub async fn put_retained(
        &self,
        client: &mut Client,
        reference: Reference,
        bytes: &[u8],
        content_type: &str,
        retention: Retention,
    ) -> Result<Metadata> {
        retention.validate()?;
        self.store(
            client,
            reference,
            bytes,
            content_type,
            Some(retention),
            None,
        )
        .await
    }
    async fn store(
        &self,
        client: &mut Client,
        reference: Reference,
        bytes: &[u8],
        content_type: &str,
        retention: Option<Retention>,
        expiry_days: Option<i64>,
    ) -> Result<Metadata> {
        if !bounded(&reference.subject, 255)
            || !bounded(&reference.kind, 100)
            || !bounded(content_type, 255)
            || bytes.is_empty()
            || bytes.len() > storage::MAX_BYTES
        {
            return Err(Error::Invalid);
        }
        let mut tx = self.begin(client, "artifacts.reserve").await?;
        self.authorize(tx.sql(), Operation::Create, Some(&reference))
            .await?;
        if retention.is_some() {
            self.authorize(tx.sql(), Operation::Retain, Some(&reference))
                .await?;
        }
        let root = self
            .store
            .settings
            .get_global(tx.sql(), "artifacts.storage_root")
            .await?
            .as_str()
            .ok_or(Error::Policy)?
            .to_owned();
        let days = self
            .store
            .settings
            .get_global(tx.sql(), "artifacts.retention_days")
            .await?
            .as_i64()
            .filter(|d| *d > 0 && *d <= 36500)
            .ok_or(Error::Policy)?;
        let days = expiry_days.map_or(days, |shorter| shorter.min(days));
        let max = self
            .store
            .settings
            .get_global(tx.sql(), "artifacts.max_bytes")
            .await?
            .as_u64()
            .ok_or(Error::Policy)?;
        if bytes.len() as u64 > max {
            return Err(Error::Invalid);
        }
        storage::io(|| storage::prepare_root(&root))?;
        let id = uuid::Uuid::new_v4().to_string();
        let size = bytes.len() as i64;
        let sha256 = digest(bytes);
        let row = tx.sql().query_one(&format!("INSERT INTO base_artifacts(public_id,tenant_id,company_id,owner_id,owner_adapter,subject,kind,content_type,byte_size,sha256,storage_root,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,clock_timestamp()+$12::bigint * interval '1 day') RETURNING {COLUMNS}"), &[&id,&self.scope.tenant(),&self.company.get(),&self.owner.owner_id(),&type_name::<O>(),&reference.subject,&reference.kind,&content_type,&size,&sha256,&root,&days]).await?;
        let row = stored(row);
        tx.record(Change {
            kind: "base/artifacts.document".into(),
            id: row.key,
            old: Value::Null,
            new: evidence(&row),
        })
        .await?;
        self.audit(&tx, &row, "artifacts.reserve").await?;
        tx.commit().await?;
        let mut tx = self.begin(client, "artifacts.publish").await?;
        let row = self.fetch(&tx, &id).await?;
        self.authorize(tx.sql(), Operation::Create, Some(&row.metadata.reference))
            .await?;
        if row.deleted || row.ready || row.expired {
            return Err(Error::NotFound);
        }
        // Keep this IO synchronous: cancellation must not leave an in-flight
        // writer running after the DB lock is dropped and recovery purges it.
        storage::io(|| storage::write(&row.root, &id, bytes))?;
        self.authorize(tx.sql(), Operation::Create, Some(&row.metadata.reference))
            .await?;
        let mut row = self.transition(&mut tx, &row, "ready").await?;
        if let Some(retention) = &retention {
            self.authorize(tx.sql(), Operation::Retain, Some(&row.metadata.reference))
                .await?;
            row = self.retain_published(&mut tx, row, retention).await?;
        }
        self.audit(&tx, &row, "artifacts.publish").await?;
        tx.commit().await?;
        Ok(row.metadata)
    }
    /// Lock, reauthorize, verify byte count/digest, and commit read evidence
    /// before returning any bytes. Never cache this result for another caller.
    pub async fn read(&self, client: &mut Client, id: &str) -> Result<Document> {
        let tx = self.begin(client, "artifacts.read").await?;
        let row = self.fetch(&tx, id).await?;
        self.authorize(tx.sql(), Operation::Read, Some(&row.metadata.reference))
            .await?;
        if !row.ready || row.deleted || row.expired {
            return Err(Error::NotFound);
        }
        let bytes = storage::io(|| storage::read(&row.root, id, row.metadata.byte_size as usize))?;
        if digest(&bytes) != row.metadata.sha256 {
            return Err(Error::Integrity);
        }
        self.audit(&tx, &row, "artifacts.read").await?;
        tx.commit().await?;
        Ok(Document {
            metadata: row.metadata,
            bytes,
        })
    }
    /// A tombstone commits before unlink. Failed unlink/commit is retried
    /// idempotently; a missing file still requires successful audit to purge.
    pub async fn delete(&self, client: &mut Client, id: &str) -> Result<()> {
        let mut tx = self.begin(client, "artifacts.delete").await?;
        let row = self.fetch(&tx, id).await?;
        self.authorize(tx.sql(), Operation::Delete, Some(&row.metadata.reference))
            .await?;
        if !row.deleted {
            if let Some(refusal) = row.refusal() {
                // The refusal itself is evidence: commit it, then refuse.
                self.audit_with(
                    &tx,
                    &row,
                    "artifacts.delete_refused",
                    json!({"reason":refusal.code()}),
                )
                .await?;
                tx.commit().await?;
                return Err(refusal);
            }
            let row = self.transition(&mut tx, &row, "deleted").await?;
            self.audit(&tx, &row, "artifacts.delete").await?;
        }
        tx.commit().await?;
        self.cleanup(client, id).await
    }
    async fn cleanup(&self, client: &mut Client, id: &str) -> Result<()> {
        let mut tx = self.begin(client, "artifacts.purge").await?;
        let row = self.fetch(&tx, id).await?;
        self.authorize(tx.sql(), Operation::Delete, Some(&row.metadata.reference))
            .await?;
        if !row.deleted {
            return Err(Error::Invalid);
        }
        if !row.purged {
            storage::io(|| storage::delete(&row.root, id))?;
            let row = self.transition(&mut tx, &row, "purged").await?;
            self.audit(&tx, &row, "artifacts.purge").await?;
        }
        tx.commit().await?;
        Ok(())
    }
    /// Owner-scoped bounded maintenance. Reconciles tombstones and reservations
    /// older than one hour; ready documents are left to `purge_expired`'s
    /// retention policy. Documents whose purge an operator must resolve
    /// (`list_purge_holds`) are skipped. Candidate locking rechecks state so a
    /// delayed writer cannot be purged.
    pub async fn recover(&self, client: &mut Client, limit: i64) -> Result<Recovery> {
        if !(1..=1000).contains(&limit) {
            return Err(Error::Invalid);
        }
        self.authorize(&*client, Operation::Purge, None).await?;
        let rows = client.query("SELECT public_id FROM base_artifacts WHERE tenant_id=$1 AND company_id=$2 AND owner_id=$3 AND owner_adapter=$4 AND purged_at IS NULL AND purge_held_at IS NULL AND (deleted_at IS NOT NULL OR (ready_at IS NULL AND created_at < clock_timestamp()-interval '1 hour')) ORDER BY created_at,id LIMIT $5", &[&self.scope.tenant(),&self.company.get(),&self.owner.owner_id(),&type_name::<O>(),&limit]).await?;
        let mut report = Recovery::default();
        for candidate in rows {
            let id: String = candidate.get(0);
            match self.recover_one(client, &id).await {
                Ok(true) => report.purged.push(id),
                Ok(false) => {}
                Err(error) => report.failed.push((id, error)),
            }
        }
        Ok(report)
    }
    async fn recover_one(&self, client: &mut Client, id: &str) -> Result<bool> {
        let mut tx = self.begin(client, "artifacts.recover").await?;
        let row = self.fetch(&tx, id).await?;
        self.authorize(tx.sql(), Operation::Purge, None).await?;
        self.authorize(tx.sql(), Operation::Delete, Some(&row.metadata.reference))
            .await?;
        if row.purged || (row.ready && !row.deleted) {
            return Ok(false);
        }
        if !row.deleted {
            let row = self.transition(&mut tx, &row, "deleted").await?;
            self.audit(&tx, &row, "artifacts.delete").await?;
        }
        tx.commit().await?;
        self.cleanup(client, id).await?;
        Ok(true)
    }
}
inventory::submit! {
    bilibi_module_registry::Module {
        id:"base/artifacts",layer:bilibi_module_registry::Layer::Base,
        dependencies:&["base/audit","base/authz","base/database","base/queue","base/settings","base/tenancy","base/party_ids"],
        migrations:&[("base-artifacts-001",include_str!("../migrations/001-artifacts.sql")),("base-artifacts-002",include_str!("../migrations/002-retention.sql"))],
        services:&[Service {id:"base/artifacts.install-policy",factory:|_| Ok(Arc::new(bilibi_database::seeds::SeedService(Arc::new(install::InstallPolicy))))},Service {id:"base/artifacts.store",factory:|services| Ok(Arc::new(Artifacts {
            boundary:services.get("base/database.mutations")?, settings:services.get("base/settings.store")?, authentication:services.get("base/tenancy.authentication")?,
            queue:services.get("base/queue.store")?, db:services.get::<Db>("base/database.pool")?.as_ref().clone(),
            tokens:render::Tokens::new().map_err(|error| bilibi_module_registry::RegistryError::Contribution(error.to_string()))?,
        }))}],
    }
}
