//! ChatGPT, the first harness (Bilimbi `plugins/bilimbi/plugin.json` and the
//! `mcp.json` that `PluginPackage` writes). Only the manifest format and file
//! layout live here. Install steps are in `docs/mcp-setup.md`, not on the
//! settings page.
use crate::{
    harness::{Harness, PackageFile},
    package::Package,
};
use serde_json::{json, Value};

pub struct ChatGpt;

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
    fn archive_name(&self, package: &Package) -> String {
        format!("{}-chatgpt-plugin.zip", package.name)
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
                (package.name.as_str()): {
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
