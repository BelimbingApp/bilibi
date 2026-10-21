//! The ChatGPT plugin download (Bilimbi `web/plugin_controller.ex`). The
//! settings page lives with the person's connections in `core/agent_access`.
//! Downloading grants no access. `AgentPlugins` keeps the self-service gate
//! at download.
use crate::{AgentPlugins, PluginError};
use bilibi_database::Error;
use bilibi_tenancy::Scope;
use bilibi_ui::download::{Download, DownloadFuture, DownloadRequest, FileBody};
use std::sync::Arc;

pub const FILES: &str = "core/agent_plugins.files";
#[derive(Clone)]
pub struct Pages {
    pub plugins: Arc<AgentPlugins>,
}

impl Download<Scope, Error> for Pages {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            if !request.query.is_empty() {
                return Err(Error::Invalid);
            }
            match self
                .plugins
                .download(request.scope, request.operation)
                .await
            {
                Ok(archive) => Ok(FileBody::attachment(
                    archive.bytes,
                    "application/zip",
                    &archive.filename,
                )),
                Err(PluginError::PersonRequired) => Err(Error::Forbidden),
                Err(PluginError::UnknownHarness) => Err(Error::NotFound),
                Err(PluginError::HttpsRequired) => Err(Error::Conflict),
                Err(PluginError::Other(error)) => Err(error),
            }
        })
    }
}
