//! The ChatGPT plugin download (Bilimbi `web/plugin_controller.ex`). The
//! settings page lives with the person's connections in `core/agent_access`.
//! Downloading grants no access. `AgentPlugins` keeps the self-service gate
//! and re-checks the client at download.
use crate::{AgentPlugins, PluginError};
use bilibi_database::{Error, Result};
use bilibi_tenancy::Scope;
use bilibi_ui::download::{Download, DownloadFuture, DownloadRequest, FileBody};
use std::{collections::HashMap, sync::Arc};

pub const FILES: &str = "core/agent_plugins.files";
/// Client IDs are bounded by the setting's validator.
const CLIENT_ID_LIMIT: usize = 255;

#[derive(Clone)]
pub struct Pages {
    pub plugins: Arc<AgentPlugins>,
}

/// Only `client_id`, bounded; anything else is a malformed address.
fn requested(query: &HashMap<String, String>) -> Result<Option<String>> {
    if query.keys().any(|key| key != "client_id") {
        return Err(Error::Invalid);
    }
    match query.get("client_id") {
        Some(id) if id.len() > CLIENT_ID_LIMIT => Err(Error::Invalid),
        Some(id) if id.is_empty() => Ok(None),
        id => Ok(id.cloned()),
    }
}

impl Download<Scope, Error> for Pages {
    fn serve<'a>(&'a self, request: DownloadRequest<'a, Scope>) -> DownloadFuture<'a, Error> {
        Box::pin(async move {
            let client_id = requested(request.query)?.ok_or(Error::NotFound)?;
            match self
                .plugins
                .download(request.scope, request.operation, &client_id)
                .await
            {
                Ok(archive) => Ok(FileBody::attachment(
                    archive.bytes,
                    "application/zip",
                    &archive.filename,
                )),
                Err(PluginError::PersonRequired) => Err(Error::Forbidden),
                Err(PluginError::InvalidClient | PluginError::UnknownHarness) => {
                    Err(Error::NotFound)
                }
                Err(PluginError::HttpsRequired) => Err(Error::Conflict),
                Err(PluginError::Other(error)) => Err(error),
            }
        })
    }
}
