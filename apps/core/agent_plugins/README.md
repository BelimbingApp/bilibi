# Agent plugins

Builds the downloadable plugin package that connects an agent harness (ChatGPT
first) to this installation's `/mcp` edge. Ported from Bilimbi
`apps/core/agent_access` (`plugin_package.ex`, `web/plugin_live.ex`,
`web/plugin_controller.ex`, `mix bilimbi.mcp.plugin`), split so the harness
format is a thin adapter over one harness-neutral builder.

## Layers

One answer: the plugin is thin; MCP, `/api/v1`, OAuth and the agent registry
are the base layer and stay where they are.

| Layer | Owner | Harness-specific? |
| --- | --- | --- |
| Operations, guides, plans, MCP protocol | `base/agent_api` | No |
| `/api/v1`, `/mcp`, OAuth endpoints | `apps/web` | No |
| OAuth public clients, grants, connections | `core/agent_access` | No |
| Package builder, archive, page, download, exporter | `core/agent_plugins` (`src/package.rs`, `src/archive.rs`, `src/web.rs`) | No |
| Manifest format, file layout, page copy | one adapter per harness (`src/chatgpt.rs`) | Yes |

## Package builder (`src/package.rs`)

- `Connection`: the canonical origin (`APP_ORIGIN`, never the Host header),
  `{origin}/mcp`, `{origin}/oauth/authorize`, `{origin}/oauth/token`, the
  `bilibi` scope, PKCE S256 and the registered public client ID. No secret.
- Refuses an origin that is not bare `https://host[:port]` and a client ID that
  is not in `agent_access.oauth_clients`.
- `Skill`s are generated, not hand-written per harness:
  - `bilibi`: the MCP workflow over the six generic tools.
  - `bilibi-setup`: connection check and OAuth linking.
  - one skill per guide in the `base/agent_api` registry: its key, title,
    summary and keywords, telling the agent to read the guide through the
    `guide` tool. The guide body never enters the package; reading it still
    checks the person's reach live.
- The same `Package` feeds the browser download and the CLI exporter.

## Harness adapters (`src/harness.rs`)

- A `Harness` names itself (`key`, `name`), its setup page and download paths,
  the archive's file name, the page copy (install, public, LAN/VPN steps and
  guide links) and `files(&Package)`: the manifest, MCP configuration and skill
  files in that harness's layout.
- Adapters know formats only. They never read settings, scopes or the registry.
- ChatGPT (`src/chatgpt.rs`): `bilibi/plugin.json` with the `com.openai`
  interface, `bilibi/mcp.json` with `com.openai` OAuth (`mode: provided`,
  `tokenEndpointAuthMethod: none`), `bilibi/skills/<name>/SKILL.md`.

### How to add a harness

1. Add `src/<harness>.rs` implementing `Harness`: manifest, MCP configuration,
   skill layout and page copy. Nothing else.
2. Add it to `HARNESSES` in `src/harness.rs`.
3. Submit its `Route` and `Download` contributions in `src/contributions.rs`
   (operation = the harness key).
4. Link the page from the account settings nav (`core/user`
   `settings_entries`) if people should find it there.
5. Run the app; open the page, download, unzip and check the files.

The builder, archive, page, download and exporter do not change.

## Page and download

- `/settings/<harness>-plugin` (`/settings/chatgpt-plugin`): a live page named
  for the harness; linked from account settings and Agent connections.
- Shows the canonical connection details with copy buttons (`ClipboardCopy`
  hook, success and failure feedback), the registered-client selection
  (auto-selected when only one is registered), and the adapter's public HTTPS
  and private LAN/VPN steps. An HTTP origin or no registered client explains
  what the administrator must fix and offers no download.
- `/settings/<harness>-plugin/download?client_id=…` (a `Download` contribution)
  answers the ZIP built in memory. It re-checks the client registration and
  the HTTPS origin on every request: 403 for impersonation or an agent facet,
  404 for an unregistered client, 409 for an HTTP origin.
- Neither the page nor the download creates or approves a grant. The archive
  holds public configuration and skills only: no token, credential, record or
  executable.

## Exporter

```sh
bilibi-web plugin-export --harness chatgpt --client <registered-id> --output <new-directory>
```

- Same builder as the download, from `APP_ORIGIN`; refuses an existing output
  directory. It writes a local package; it does not publish or register it.
- It is an exporter, not a client. A general Bilibi CLI client over `/api/v1`
  does not exist yet.
