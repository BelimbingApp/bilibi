//! The ChatGPT plugin download. The settings page is AI connections in
//! `core/agent_access`. Every signed-in person may download; `AgentPlugins`
//! keeps the self-service gate and the client check.
use crate::web::FILES;
use bilibi_module_registry::{
    contributions::{Access, Download, DOWNLOADS},
    Contribution, Provenance,
};
macro_rules! provenance {
    () => {
        Provenance {
            module: "core/agent_plugins",
            source: file!(),
            line: line!(),
        }
    };
}

// ChatGPT (`chatgpt.rs`): the operation is the harness key. The ZIP is not a
// generic MCP download; `docs/mcp-setup.md` is the setup guide.
inventory::submit! { Contribution::new(DOWNLOADS, "core/agent_plugins.chatgpt.download", provenance!(), &Download {
    path: "/settings/chatgpt-plugin/download", service: FILES, operation: "chatgpt", access: Access::SignedIn,
}) }
