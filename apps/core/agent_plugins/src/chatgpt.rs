//! ChatGPT, the first harness (Bilimbi `plugins/bilimbi/plugin.json`, the
//! `mcp.json` that `PluginPackage` writes, and the copy of `PluginLive`). Only
//! the manifest format, file layout and setup copy live here.
use crate::{
    harness::{GuideLink, Harness, PackageFile, PageCopy},
    package::Package,
};
use serde_json::{json, Value};

pub struct ChatGpt;

const COPY: PageCopy = PageCopy {
    install: &[
        "In ChatGPT, open Plugins and look in your company's tab for Bilibi. Install the plugin your administrator provides.",
        "If your administrator gave you connection details instead, choose the plus button, then Add custom MCP server. Follow the public or LAN/VPN instructions below.",
        "Sign in on this Bilibi installation, review the connection and its end date. ChatGPT never needs your Bilibi password in a message.",
        "Start a new chat, select Bilibi with @, and ask it to find a record you can access.",
    ],
    download_note: "The ZIP contains Bilibi's skills and this server's public connection configuration. Give it to your workspace administrator for a company marketplace, or install it through a supported local marketplace. For a private connection, configure Tunnel separately in ChatGPT; the ZIP does not create a tunnel.",
    public: &[
        "Your administrator makes this server's HTTPS address reachable from ChatGPT and registers its OAuth callback in Bilibi.",
        "In Add custom MCP server, enter the Server URL above. Choose OAuth and the registered Client ID, with no client secret.",
        "Create the plugin, install it, then sign in to Bilibi when prompted.",
    ],
    public_guide: GuideLink {
        label: "OpenAI connection guide",
        href: "https://developers.openai.com/plugins/deploy/connect-chatgpt",
    },
    private: &[
        "Your administrator runs OpenAI's Secure MCP Tunnel client on a machine that can reach the Server URL above, and associates the tunnel with your ChatGPT workspace.",
        "In Add custom MCP server, choose Tunnel and select the company tunnel or enter the tunnel ID your administrator provides. Configure the registered OAuth client.",
        "Use the LAN or VPN for Bilibi sign-in. Your administrator must verify login and token exchange as well as MCP; the tunnel does not automatically expose every OAuth endpoint.",
    ],
    private_note: "Your computer's VPN alone does not give hosted ChatGPT access. The administrator-configured tunnel uses outbound HTTPS. This page does not create or test a tunnel.",
    private_guide: GuideLink {
        label: "OpenAI private connection guide",
        href: "https://developers.openai.com/api/docs/guides/secure-mcp-tunnels",
    },
};

fn pretty(value: &Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

impl Harness for ChatGpt {
    fn key(&self) -> &'static str {
        "chatgpt"
    }
    fn name(&self) -> &'static str {
        "ChatGPT"
    }
    fn title(&self) -> &'static str {
        "Plugin for ChatGPT"
    }
    fn path(&self) -> &'static str {
        "/settings/chatgpt-plugin"
    }
    fn archive_name(&self) -> &'static str {
        "bilibi-chatgpt-plugin.zip"
    }
    fn copy(&self) -> &'static PageCopy {
        &COPY
    }
    fn files(&self, package: &Package) -> Vec<PackageFile> {
        let connection = &package.connection;
        let manifest = json!({
            "$schema": "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json",
            "name": package.name,
            "version": package.version,
            "description": package.description,
            "extensions": {
                "com.openai": {
                    "onboardingSkill": format!("./skills/{}/SKILL.md", package.setup_skill()),
                    "interface": {
                        "displayName": package.display_name,
                        "shortDescription": package.short_description,
                        "defaultPrompt": [package.default_prompt]
                    }
                }
            }
        });
        let mcp = json!({
            "$schema": "https://agent-plugins.org/schemas/1.0.0/mcp.schema.json",
            "mcpServers": {
                (package.name): {
                    "type": "streamable-http",
                    "url": connection.server_url(),
                    "extensions": {
                        "com.openai": {
                            "auth": {
                                "type": "oauth",
                                "client": {
                                    "mode": "provided",
                                    "clientId": connection.client_id,
                                    "tokenEndpointAuthMethod": "none"
                                },
                                "authorizationUrl": connection.authorization_url(),
                                "tokenUrl": connection.token_url(),
                                "baseScopes": [connection.scope()]
                            }
                        }
                    }
                }
            }
        });
        let mut files = vec![
            PackageFile::new("plugin.json", pretty(&manifest)),
            PackageFile::new("mcp.json", pretty(&mcp)),
        ];
        files.extend(package.skills.iter().map(|skill| {
            PackageFile::new(format!("skills/{}/SKILL.md", skill.name), skill.markdown())
        }));
        files
    }
}
