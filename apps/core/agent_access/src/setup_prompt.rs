//! Paste-ready connection prompt.
//! Public installation details only; apps discover their own OAuth identity.
use bilibi_system::Instance;

/// The MCP address of this Bilibi.
pub fn address(instance: &Instance) -> String {
    format!("{}/mcp", instance.origin)
}

/// The name a tool saves the connection under, as the server calls itself
/// (`bilibi-<slug>`), so Dev, Staging and Production never collide.
pub fn server_name(instance: &Instance) -> String {
    format!("bilibi-{}", instance.slug())
}

/// One sentence naming this Bilibi once, then what a change here does. The
/// role is spoken only once: a label that is just the role's name (`Dev` on a
/// Dev copy) is not repeated.
pub fn identity(instance: &Instance) -> String {
    let role = instance.role;
    if instance.label.eq_ignore_ascii_case(role.label()) {
        format!("This Bilibi is {}. {}", role.kind(), role.effect())
    } else {
        format!(
            "This Bilibi is called \"{}\" and is {}. {}",
            instance.label,
            role.kind(),
            role.effect()
        )
    }
}

pub fn prompt(instance: &Instance) -> String {
    let (address, name) = (address(instance), server_name(instance));
    format!(
        "Connect yourself to my Bilibi, a business application, so you can work in it for me. You will have my permissions and nothing more.\n\n\
{}\n\
MCP server URL: {address}\n\n\
How to add it:\n\
- Claude Code: run `claude mcp add --transport http {name} {address}`.\n\
- Codex: run `codex mcp add {name} --url {address}`. It prints a sign-in link. Tell me and wait.\n\
- Any other app: add the URL in its MCP or connector settings, with OAuth.\n\
If you cannot run a command or change settings yourself, tell me the exact step to do myself.\n\n\
Signing in is mine to do. When the browser opens, tell me and wait. Do not ask for my Bilibi password or put it in chat. I will check the app name and return address before approving. When I have signed in, I will switch on this connection. Then check that a Bilibi tool works and tell me the result. If your app needs a restart or the /mcp command first, tell me.",
        identity(instance)
    )
}
