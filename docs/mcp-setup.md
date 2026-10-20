# App-specific setup guides

Use **My settings → MCP** (`/settings/mcp`) to copy this server's MCP address and the app's client ID.

Add that server URL in the app's MCP settings. Sign in to Bilibi when the app asks, and approve the connection. The agent can do what you can do in Bilibi, and nothing more. End access any time in **Agent connections**.

`/settings/chatgpt-plugin` opens the same page. The page does not install an app, and it does not approve a connection.

## ChatGPT

The downloadable plugin is for ChatGPT only. The ZIP holds Bilibi's skills and connection settings. It does not set up a private tunnel, and downloading it does not install the plugin or approve a connection.

1. In ChatGPT, open Plugins and look in your company's tab for Bilibi. Install the plugin your administrator provides.
2. If your administrator gave you connection details instead, choose the plus button, then Add custom MCP server. Follow the public or LAN / VPN steps below.
3. Sign in on this Bilibi installation, and review the connection and its end date. Do not put your Bilibi password in a chat message.
4. Start a new chat, select Bilibi with @, and ask it to find a record you can open.

### Public installation

Your administrator makes this server's HTTPS address reachable from ChatGPT and registers its OAuth callback in Bilibi.

In Add custom MCP server, enter the Server URL from My settings. Choose OAuth and the registered Client ID, with no client secret.

Create the plugin, install it, then sign in to Bilibi when prompted.

OpenAI's guide: <https://developers.openai.com/plugins/deploy/connect-chatgpt>

### LAN / VPN installation

A VPN on your computer does not let ChatGPT reach Bilibi. Your administrator must set up a tunnel. This page does not do that.

1. Your administrator runs OpenAI's Secure MCP Tunnel client on a machine that can reach the Server URL, and associates the tunnel with your ChatGPT workspace.
2. In Add custom MCP server, choose Tunnel and select the company tunnel or enter the tunnel ID your administrator provides. Configure the registered OAuth client.
3. Use the LAN or VPN for Bilibi sign-in. Your administrator must verify login and token exchange as well as MCP. The tunnel does not automatically expose every OAuth endpoint.

OpenAI's guide: <https://developers.openai.com/api/docs/guides/secure-mcp-tunnels>

### Download the plugin

Give the ZIP to your ChatGPT administrator.

On an HTTPS installation, a signed-in person downloads it from `/settings/chatgpt-plugin/download?client_id=<registered-client-id>`. The file is named `bilibi-<instance>-chatgpt-plugin.zip` (for example `bilibi-staging-chatgpt-plugin.zip`).

On the server, the same package is written into a new directory:

```sh
bilibi-web plugin-export --harness chatgpt --client <registered-client-id> --output <new-directory>
```

The command refuses an existing directory, an unknown harness, an unregistered client and an HTTP origin. The archive holds public configuration and skills only: no token, credential, record or executable.

## Other apps

Claude Code and Codex connect with the server URL and a registered client ID, without this ZIP. The steps are in [Connect a local agent](../apps/core/agent_access/README.md#connect-a-local-agent).
