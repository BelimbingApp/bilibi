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
        "Connect to my Bilibi business application using MCP and OAuth.\n\n\
{}\n\
Server name: {name}\n\
Server URL: {address}\n\n\
I will approve sign-in in my browser and switch on the connection. Never ask for my password. Then check that a Bilibi tool works and tell me the result.",
        identity(instance)
    )
}
