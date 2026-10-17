//! The setup page and download of each harness. Every signed-in person may
//! open them; `AgentPlugins` keeps the self-service gate and the client check.
use crate::web::{FILES, PAGES};
use bilibi_module_registry::{
    contributions::{Access, Download, Method, Route, DOWNLOADS, ROUTES},
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

// ChatGPT (`chatgpt.rs`): the operation is the harness key, the paths its `path()`.
inventory::submit! { Contribution::new(ROUTES, "core/agent_plugins.chatgpt", provenance!(), &Route {
    method: Method::Get, path: "/settings/chatgpt-plugin", service: PAGES, operation: "chatgpt", access: Access::SignedIn, framed: true,
}) }
inventory::submit! { Contribution::new(DOWNLOADS, "core/agent_plugins.chatgpt.download", provenance!(), &Download {
    path: "/settings/chatgpt-plugin/download", service: FILES, operation: "chatgpt", access: Access::SignedIn,
}) }
