//! Paste-ready connection prompt (Bilimbi connections_live.ex equivalent).
//! Public installation details only; apps discover their own OAuth identity.
use bilibi_system::Instance;

pub fn prompt(instance: &Instance) -> String {
    format!(
        "Connect yourself to my Bilibi, a business application, so you can work in it for me. You will have my permissions and nothing more.\n\n\
This Bilibi: {}. {}. {}\n\
MCP server URL: {}/mcp\n\n\
Add this server using your app's OAuth connection. If you cannot add it yourself, tell me how to add that address in your MCP or connector settings.\n\n\
Signing in is mine to do. When the browser opens, tell me and wait. Do not ask for my Bilibi password or put it in chat. I will check the app name, return address and connection end date before approving. Then call a Bilibi tool to check it works and tell me the result.",
        instance.label, instance.role.label(), instance.role.consequence(), instance.origin
    )
}
