//! Agent plugins: the downloadable package that connects an agent harness to
//! this installation's `/mcp` edge (Bilimbi `Core.AgentAccess.plugin_clients`,
//! `plugin_files` and `plugin_download`). The settings page is AI connections
//! in `core/agent_access`. The harness-neutral builder is `package.rs`; each
//! harness is a thin adapter (`chatgpt.rs`). Downloading software never
//! creates or approves a grant: the OAuth consent in `core/agent_access`
//! still names the person and the end date.
mod archive;
mod chatgpt;
mod contributions;
mod harness;
mod package;
mod web;

pub use harness::{Harness, PackageFile};
pub use package::https_origin;

use bilibi_agent_api::Registry;
use bilibi_database::{Db, Error};
use bilibi_settings::Settings;
use bilibi_system::Instance;
use bilibi_tenancy::Scope;
use package::{Connection, Package};
use std::{path::Path, sync::Arc};

/// `core/agent_plugins.packages`, the facade.
pub const SERVICE: &str = "core/agent_plugins.packages";

#[derive(Debug, thiserror::Error)]
pub enum PluginError {
    /// Impersonation or an agent facet: only the person sets up their agent.
    #[error("person_required")]
    PersonRequired,
    #[error("unknown_harness")]
    UnknownHarness,
    /// `APP_ORIGIN` is not a bare HTTPS origin.
    #[error("https_required")]
    HttpsRequired,
    #[error(transparent)]
    Other(#[from] Error),
}
/// A built archive, ready to serve.
pub struct Archive {
    pub filename: String,
    pub bytes: Vec<u8>,
}

pub struct AgentPlugins {
    registry: Arc<Registry>,
    /// For the instance the package is named after.
    db: Db,
    settings: Arc<Settings>,
    /// `APP_ORIGIN`, the canonical address; never the Host header.
    origin: String,
}
impl AgentPlugins {
    pub fn origin(&self) -> &str {
        &self.origin
    }
    /// The keys of every harness adapter, for usage messages.
    pub fn harness_keys() -> Vec<&'static str> {
        harness::HARNESSES
            .iter()
            .map(|harness| harness.key())
            .collect()
    }
    pub fn harness(key: &str) -> Result<&'static dyn Harness, PluginError> {
        harness::find(key).ok_or(PluginError::UnknownHarness)
    }
    /// Build a package that lets the harness register itself through discovery.
    async fn package(&self) -> Result<Package, PluginError> {
        let connection = Connection::new(&self.origin)?;
        let instance = {
            let client = self.db.get().await.map_err(Error::from)?;
            Instance::resolve(&self.settings, &**client).await?
        };
        Ok(Package::build(connection, &self.registry, &instance))
    }
    /// The person's download. Recheck self-service and the HTTPS origin.
    pub async fn download(&self, scope: &Scope, harness: &str) -> Result<Archive, PluginError> {
        let harness = Self::harness(harness)?;
        if scope.agent().is_some() || scope.impersonator().is_some() {
            return Err(PluginError::PersonRequired);
        }
        let package = self.package().await?;
        let files = harness.files(&package);
        let bytes = archive::zip(&package.name, &files).map_err(|_| Error::Internal)?;
        Ok(Archive {
            filename: harness.archive_name(&package),
            bytes,
        })
    }
    /// The administrative exporter (Bilimbi `mix bilimbi.mcp.plugin`): global,
    /// non-secret configuration written into a new directory.
    pub async fn export(&self, harness: &str, output: &Path) -> Result<&'static str, ExportError> {
        let harness = Self::harness(harness)?;
        let package = self.package().await?;
        let files = harness.files(&package);
        if output.exists() {
            return Err(ExportError::OutputExists);
        }
        archive::write_directory(output, &files).map_err(ExportError::Write)?;
        Ok(harness.name())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error("output directory already exists; choose a new directory")]
    OutputExists,
    #[error("could not write the package: {0}")]
    Write(std::io::Error),
}

/// The exporter's usage line, shown by the host for a malformed command.
pub const USAGE: &str =
    "usage: bilibi-web plugin-export --harness <harness> --output <new-directory>";

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/agent_plugins", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/database", "base/tenancy", "base/ui", "base/agent_api", "base/settings", "base/system", "core/agent_access"],
        migrations: &[],
        services: &[
            bilibi_module_registry::Service { id: SERVICE, factory: |services| Ok(Arc::new(AgentPlugins {
                registry: services.get("base/agent_api.registry")?,
                db: services.get::<Db>("base/database.pool")?.as_ref().clone(),
                settings: services.get("base/settings.store")?,
                origin: std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),
            })) },
            bilibi_module_registry::Service { id: web::FILES, factory: |services| Ok(Arc::new(bilibi_ui::download::DownloadService(Arc::new(web::Pages {
                plugins: services.get(SERVICE)?,
            })))) },
        ],
    }
}
