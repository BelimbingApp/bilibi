//! The prompt a person pastes into their AI app on the AI connections tab
//! (`web.rs`). It holds everything the agent needs, so no served guide exists
//! and an app that cannot reach this address still gets the facts. Bilimbi
//! `connections_live.ex` has no counterpart: Bilimbi shows the steps instead.
//! Public facts only: the instance, the MCP address and the registered client
//! IDs. Never a token, a secret or a callback address.
use crate::PublicClient;
use bilibi_system::Instance;

/// Client IDs the operator registers for the two apps that can set themselves
/// up (`README.md`, Connect a local agent). The command shown for each is the
/// harness's own current syntax.
const CLAUDE_CODE: &str = "bilibi-claude-code";
const CODEX: &str = "bilibi-codex";

/// The paste-ready text for this installation and its registered apps.
pub fn prompt(instance: &Instance, clients: &[PublicClient]) -> String {
    let url = format!("{}/mcp", instance.origin);
    let name = format!("bilibi-{}", instance.slug());
    let mut lines = vec![
        "Connect yourself to my Bilibi, a business application, so you can work in it for me. You will have my permissions and nothing more.".to_owned(),
        String::new(),
        "This Bilibi:".to_owned(),
        format!("- Name: {}", instance.label),
        format!(
            "- Role: {}. {}",
            instance.role.label(),
            instance.role.consequence()
        ),
        format!("- Address: {}", instance.origin),
        format!("- MCP server URL: {url}"),
        "- Sign-in: OAuth in my browser. It is a public client, so there is no client secret. Never ask for one.".to_owned(),
        String::new(),
        "Client IDs registered for this Bilibi:".to_owned(),
    ];
    lines.extend(
        clients
            .iter()
            .map(|client| format!("- {}: {}", client.name, client.id)),
    );
    lines.push(String::new());
    let has = |id: &str| clients.iter().any(|client| client.id == id);
    if has(CLAUDE_CODE) || has(CODEX) {
        lines.push(
            "If you can run commands on my computer, add the server yourself with the command for your app, from the folder I am working in:"
                .to_owned(),
        );
        if has(CLAUDE_CODE) {
            lines.push(format!(
                "- Claude Code: claude mcp add --scope project --transport http --client-id {CLAUDE_CODE} {name} {url}"
            ));
        }
        if has(CODEX) {
            lines.push(format!(
                "- Codex: codex mcp add {name} --url {url} --oauth-client-id {CODEX}"
            ));
        }
        lines.push(format!(
            "Claude Code makes me approve a server added to a project before it runs. After you add it, tell me to open Claude Code in this folder, approve {name}, and run /mcp to sign in; or run: claude mcp login {name}. Codex starts the sign-in when you add the server; to repeat it, run: codex mcp login {name}."
        ));
        lines.push(String::new());
    }
    lines.push(
        "If you cannot run commands or change your own settings, such as a chat app in a web browser, do not guess. Tell me these steps instead:"
            .to_owned(),
    );
    lines.push("1. Open your MCP or connector settings and add a custom server.".to_owned());
    lines.push(format!("2. Use this server URL: {url}"));
    lines.push(
        "3. Choose OAuth, enter the client ID that matches your app, and leave the client secret empty."
            .to_owned(),
    );
    lines.push(
        "4. A browser page opens: sign in to Bilibi, check the connection, and approve it."
            .to_owned(),
    );
    lines.push(String::new());
    lines.push(
        "Signing in is mine to do. When the browser page opens, tell me and wait. Do not ask for my Bilibi password or put it in the chat. When it is done, call a Bilibi tool to check it works and tell me the result."
            .to_owned(),
    );
    lines.join("\n")
}
