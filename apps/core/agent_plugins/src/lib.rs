//! Agent plugins: the downloadable package that connects an agent harness to
//! this installation's `/mcp` edge (Bilimbi `Core.AgentAccess.plugin_clients`,
//! `plugin_files` and `plugin_download`). The harness-neutral builder is
//! `package.rs`; each harness is a thin adapter (`chatgpt.rs`). Downloading
//! software never creates or approves a grant: the OAuth consent in
//! `core/agent_access` still names the person and the end date.
mod archive;
mod chatgpt;
mod contributions;
mod harness;
mod package;
mod web;

pub use harness::{Harness, PackageFile};
pub use package::https_origin;

use bilibi_agent_access::{AgentAccess, OAuthError, PublicClient};
use bilibi_agent_api::Registry;
use bilibi_database::Error;
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
    /// The client ID is not in `agent_access.oauth_clients`.
    #[error("invalid_client")]
    InvalidClient,
    /// `APP_ORIGIN` is not a bare HTTPS origin.
    #[error("https_required")]
    HttpsRequired,
    #[error(transparent)]
    Other(#[from] Error),
}
impl From<OAuthError> for PluginError {
    fn from(error: OAuthError) -> Self {
        match error {
            OAuthError::Other(error) => Self::Other(error),
            _ => Self::PersonRequired,
        }
    }
}

/// A built archive, ready to serve.
pub struct Archive {
    pub filename: &'static str,
    pub bytes: Vec<u8>,
}

pub struct AgentPlugins {
    access: Arc<AgentAccess>,
    registry: Arc<Registry>,
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
    /// Registered public clients for the person's own setup page.
    pub async fn clients(&self, scope: &Scope) -> Result<Vec<PublicClient>, PluginError> {
        Ok(self.access.clients_for(scope).await?)
    }
    fn files(
        &self,
        harness: &dyn Harness,
        client_id: &str,
        clients: &[PublicClient],
    ) -> Result<Vec<PackageFile>, PluginError> {
        let connection = Connection::new(&self.origin, client_id, clients)?;
        Ok(harness.files(&Package::build(connection, &self.registry)))
    }
    /// The person's download. The client registration and the HTTPS origin
    /// are checked again here, whatever the page showed.
    pub async fn download(
        &self,
        scope: &Scope,
        harness: &str,
        client_id: &str,
    ) -> Result<Archive, PluginError> {
        let harness = Self::harness(harness)?;
        let clients = self.clients(scope).await?;
        let files = self.files(harness, client_id, &clients)?;
        let bytes = archive::zip(package::NAME, &files).map_err(|_| Error::Internal)?;
        Ok(Archive {
            filename: harness.archive_name(),
            bytes,
        })
    }
    /// The administrative exporter (Bilimbi `mix bilimbi.mcp.plugin`): global,
    /// non-secret configuration written into a new directory.
    pub async fn export(
        &self,
        harness: &str,
        client_id: &str,
        output: &Path,
    ) -> Result<&'static str, ExportError> {
        let harness = Self::harness(harness)?;
        let clients = self
            .access
            .public_clients()
            .await
            .map_err(PluginError::from)?;
        let files = self.files(harness, client_id, &clients)?;
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
    "usage: bilibi-web plugin-export --harness <harness> --client <registered-client-id> --output <new-directory>";

inventory::submit! {
    bilibi_module_registry::Module {
        id: "core/agent_plugins", layer: bilibi_module_registry::Layer::Core,
        dependencies: &["base/database", "base/tenancy", "base/ui", "base/agent_api", "core/agent_access"],
        migrations: &[],
        services: &[
            bilibi_module_registry::Service { id: SERVICE, factory: |services| Ok(Arc::new(AgentPlugins {
                access: services.get("core/agent_access.grants")?,
                registry: services.get("base/agent_api.registry")?,
                origin: std::env::var("APP_ORIGIN").unwrap_or_else(|_| "http://localhost:3000".into()).trim_end_matches('/').to_owned(),
            })) },
            bilibi_module_registry::Service { id: web::PAGES, factory: |services| Ok(Arc::new(bilibi_ui::page::PageService(Arc::new(web::Pages {
                plugins: services.get(SERVICE)?,
            })))) },
            bilibi_module_registry::Service { id: web::FILES, factory: |services| Ok(Arc::new(bilibi_ui::download::DownloadService(Arc::new(web::Pages {
                plugins: services.get(SERVICE)?,
            })))) },
        ],
    }
}
