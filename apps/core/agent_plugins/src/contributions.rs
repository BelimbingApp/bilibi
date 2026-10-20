//! The MCP settings page, the retired ChatGPT page address, and the ChatGPT
//! plugin download. Every signed-in person may open them; `AgentPlugins` keeps
//! the self-service gate and the client check.
use crate::web::{FILES, PAGES};
use bilibi_module_registry::{
    contributions::{Access, Download, Method, Moved, Route, DOWNLOADS, MOVED, ROUTES},
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

inventory::submit! { Contribution::new(ROUTES, "core/agent_plugins.mcp", provenance!(), &Route {
    method: Method::Get, path: "/settings/mcp", service: PAGES, operation: "mcp", access: Access::SignedIn, framed: true,
}) }
inventory::submit! { Contribution::new(MOVED, "core/agent_plugins.chatgpt", provenance!(), &Moved {
    from: "/settings/chatgpt-plugin", to: "/settings/mcp",
}) }
// ChatGPT (`chatgpt.rs`): the operation is the harness key. The ZIP is not a
// generic MCP download; `docs/mcp-setup.md` is the setup guide.
inventory::submit! { Contribution::new(DOWNLOADS, "core/agent_plugins.chatgpt.download", provenance!(), &Download {
    path: "/settings/chatgpt-plugin/download", service: FILES, operation: "chatgpt", access: Access::SignedIn,
}) }
