//! Shared PostgreSQL pool, errors, migration runner, and transactional mutation capture.
pub mod page_write;
pub mod seeds;
pub mod timing;
mod transaction;
pub use transaction::{
    Capture, CaptureFuture, Change, Context, MutationBoundary, MutationTransaction, Principal,
};

use bilibi_module_registry::Module;
use deadpool_postgres::{Manager, Pool};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use tokio_postgres::NoTls;

pub use deadpool_postgres::Pool as Db;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Sign in again")]
    Unauthorized,
    #[error("Access denied")]
    Forbidden,
    #[error("Not found")]
    NotFound,
    #[error("Invalid input")]
    Invalid,
    /// The request was understood and refused for a reason the person can act
    /// on (a form value over its limit). The message is shown as it stands.
    #[error("{0}")]
    Refused(String),
    #[error("The record changed. Reload before saving")]
    Conflict,
    #[error("Database unavailable")]
    Database(#[source] Traced<tokio_postgres::Error>),
    #[error("Database unavailable")]
    Pool(#[source] Traced<deadpool_postgres::PoolError>),
    #[error(transparent)]
    Registry(#[from] bilibi_module_registry::RegistryError),
    #[error("Applied production seed definition changed: {0}; publish a new version")]
    SeedDefinition(String),
    #[error("Applied migration changed: {0}; restore the applied file and publish a new version")]
    MigrationDefinition(String),
    #[error("Migration {version} was applied by {recorded}, but {module} now declares it")]
    MigrationOwner {
        version: String,
        recorded: String,
        module: String,
    },
    #[error("Internal error")]
    Internal,
    /// The operation needs a fresh check of the human account holder
    /// (`base/step_up`): every channel maps it to its own answer (a browser
    /// to the confirm prompt, an agent to `step_up_required`).
    #[error("Confirm with your authenticator code to continue")]
    StepUpRequired {
        /// The declared operation key.
        operation: &'static str,
        /// `required`, `code_required`, `invalid_code`, `locked`,
        /// `not_enrolled` or `impersonating`.
        reason: &'static str,
    },
}

/// A failure from below this crate and, in debug builds only, the stack where
/// it first surfaced: the page that explains an internal error to a developer
/// can then show the query's caller instead of the handler that propagated it.
pub struct Traced<E> {
    error: E,
    origin: Option<std::backtrace::Backtrace>,
}

impl<E> Traced<E> {
    fn new(error: E) -> Self {
        Self {
            error,
            origin: cfg!(debug_assertions).then(std::backtrace::Backtrace::force_capture),
        }
    }
}

/// Debug shows the failure alone; the stack is read through `Error::origin`.
impl<E: std::fmt::Debug> std::fmt::Debug for Traced<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl<E: std::fmt::Display> std::fmt::Display for Traced<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.error.fmt(f)
    }
}

impl<E: std::error::Error + 'static> std::error::Error for Traced<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Self::Database(Traced::new(error))
    }
}

impl From<deadpool_postgres::PoolError> for Error {
    fn from(error: deadpool_postgres::PoolError) -> Self {
        Self::Pool(Traced::new(error))
    }
}

impl Error {
    /// A refusal the person can act on; see [`Error::Refused`].
    pub fn refused(reason: impl std::fmt::Display) -> Self {
        Self::Refused(reason.to_string())
    }
    /// Where a database failure first surfaced; `None` in release builds and
    /// for errors that did not come from the database.
    pub fn origin(&self) -> Option<&std::backtrace::Backtrace> {
        match self {
            Self::Database(traced) => traced.origin.as_ref(),
            Self::Pool(traced) => traced.origin.as_ref(),
            _ => None,
        }
    }
    pub fn is_retryable_lock_contention(&self) -> bool {
        matches!(
            self,
            Self::Database(traced)
                if matches!(
                    traced.error.code(),
                    Some(&tokio_postgres::error::SqlState::LOCK_NOT_AVAILABLE)
                        | Some(&tokio_postgres::error::SqlState::T_R_DEADLOCK_DETECTED)
                )
        )
    }
}

pub fn pool(url: &str) -> Result<Db> {
    let config = url.parse().map_err(|_| Error::Invalid)?;
    Pool::builder(Manager::new(config, NoTls))
        .max_size(16)
        .build()
        .map_err(|_| Error::Internal)
}

/// What a migration run did: the versions it applied, and recorded versions no
/// registered module declares (an unmounted module's), which stay valid.
#[derive(Debug, Default)]
pub struct Migrated {
    pub applied: Vec<&'static str>,
    pub unmounted: Vec<(String, String)>,
}

/// The host supplies modules in dependency order. A ledger and advisory
/// transaction lock make concurrent starts safe; DDL is atomic.
///
/// Each applied version records its owner module and the SHA-256 of its SQL. A
/// version whose file changed, or that another module now claims, is refused:
/// publish a new version instead. Versions recorded by a module that is no
/// longer mounted are kept and accepted, so unmounting and remounting a Domain
/// never replays or loses its schema. Rows written before the ledger kept
/// provenance adopt the owner and hash of the module that declares them.
pub async fn migrate(db: &Db, modules: &[&'static Module]) -> Result<Migrated> {
    let mut client = db.get().await?;
    let tx = client.transaction().await?;
    tx.execute("SELECT pg_advisory_xact_lock(841317)", &[])
        .await?;
    tx.batch_execute(
        "CREATE TABLE IF NOT EXISTS bilibi_migrations (version text PRIMARY KEY, applied_at timestamptz NOT NULL DEFAULT now());
         ALTER TABLE bilibi_migrations ADD COLUMN IF NOT EXISTS module_id text, ADD COLUMN IF NOT EXISTS sha256 text",
    )
    .await?;
    let mut migrated = Migrated::default();
    let mut declared = BTreeSet::new();
    for module in modules {
        for (version, sql) in module.migrations {
            declared.insert(*version);
            let digest = sha256(sql);
            let recorded = tx
                .query_opt(
                    "SELECT module_id, sha256 FROM bilibi_migrations WHERE version=$1",
                    &[version],
                )
                .await?;
            match recorded {
                None => {
                    tx.batch_execute(sql).await?;
                    tx.execute(
                        "INSERT INTO bilibi_migrations(version, module_id, sha256) VALUES ($1, $2, $3)",
                        &[version, &module.id, &digest],
                    )
                    .await?;
                    migrated.applied.push(version);
                }
                Some(row) => match (
                    row.get::<_, Option<String>>(0),
                    row.get::<_, Option<String>>(1),
                ) {
                    (None, None) => {
                        tx.execute(
                            "UPDATE bilibi_migrations SET module_id=$2, sha256=$3 WHERE version=$1",
                            &[version, &module.id, &digest],
                        )
                        .await?;
                    }
                    (Some(owner), _) if owner != module.id => {
                        return Err(Error::MigrationOwner {
                            version: (*version).into(),
                            recorded: owner,
                            module: module.id.into(),
                        })
                    }
                    (_, hash) if hash.as_deref() != Some(digest.as_str()) => {
                        return Err(Error::MigrationDefinition(format!(
                            "{version} of {}",
                            module.id
                        )))
                    }
                    _ => {}
                },
            }
        }
    }
    for row in tx
        .query(
            "SELECT version, coalesce(module_id, '') FROM bilibi_migrations ORDER BY applied_at, version",
            &[],
        )
        .await?
    {
        let version: String = row.get(0);
        if !declared.contains(version.as_str()) {
            migrated.unmounted.push((version, row.get(1)));
        }
    }
    tx.commit().await?;
    Ok(migrated)
}

fn sha256(sql: &str) -> String {
    Sha256::digest(sql.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

// Frozen historical bootstrap: future tenancy/session/authz/audit migrations
// belong to their owning module descriptors, never a replay of this version.
pub const MIGRATION: (&str, &str) = ("001-base", include_str!("../migrations/001.sql"));

inventory::submit! {
    bilibi_module_registry::Module {
        id: "base/database", layer: bilibi_module_registry::Layer::Base,
        dependencies: &["base/module_registry"], migrations: &[MIGRATION, ("004-database-boundary", include_str!("../migrations/004-boundary.sql"))],
        services: &[bilibi_module_registry::Service {id:"base/database.mutations", factory: |_| Ok(std::sync::Arc::new(MutationBoundary::default()))}, bilibi_module_registry::Service {id:"base/database.pool", factory: |services| services.get::<Db>("bootstrap.database").map(|value|value as bilibi_module_registry::Value) }],
    }
}
inventory::submit! {
    bilibi_module_registry::Ready { module: "base/database", check: |services| services.get::<MutationBoundary>("base/database.mutations")?.ready() }
}
