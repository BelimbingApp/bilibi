# Agent plugins

Builds the downloadable plugin package that connects an agent harness (ChatGPT
first) to this installation's `/mcp` edge.
It is split so the harness format is a thin adapter over one harness-neutral builder.
The settings page is AI connections in `core/agent_access` and does not name a
harness. Its prompt is how an agent connects itself; this ZIP is only for a
ChatGPT administrator.

## Layers

One answer: the plugin is thin; MCP, `/api/v1`, OAuth and the agent registry
are the base layer and stay where they are.

| Layer | Owner | Harness-specific? |
| --- | --- | --- |
| Operations, guides, plans, MCP protocol | `base/agent_api` | No |
| `/api/v1`, `/mcp`, OAuth endpoints | `apps/web` | No |
| OAuth registrations, grants, connections | `core/agent_access` | No |
| Package builder, archive, download, exporter | `core/agent_plugins` (`src/package.rs`, `src/archive.rs`, `src/web.rs`) | No |
| AI connections settings page | `core/agent_access` | No |
| Manifest format and file layout | one adapter per harness (`src/chatgpt.rs`) | Yes |

## Package builder (`src/package.rs`)

- The package is named after the instance (`bilibi_system::Instance::resolve`):
  `bilibi-staging` and "Bilibi Staging" for the instance labelled Staging,
  `bilibi-dev-laptop` for "Dev (laptop)". The MCP server entry, the workflow
  skill, every skill prefix and the archive
  (`bilibi-staging-chatgpt-plugin.zip`) carry the same name, so a person who
  connects Dev, Staging and Production to one harness gets three plugins that
  do not collide and each say which server they reach.
- `Connection`: the canonical origin (`APP_ORIGIN`, never the Host header),
  `{origin}/mcp`, `{origin}/oauth/authorize`, `{origin}/oauth/token`, the
  `bilibi` scope, PKCE S256 and automatic public client registration. No secret.
- Refuses an origin that is not bare `https://host[:port]`.
- `Skill`s are generated, not hand-written per harness:
  - `bilibi-<instance>`: the MCP workflow over the six generic tools and every registered app tool, opening with the instance it reaches, and the self-report rule (ADR 01): send `harness/model-version-effort` in the `SelfReport::HEADER` header or MCP `_meta` (`SelfReport::MCP_META`), ask the person using the harness for an unknown part, never the installation owner, and never guess.
  - `bilibi-<instance>-setup`: connection check and OAuth linking, naming
    the instance.
  - one skill per MCP App in the registry (`bilibi-<instance>-software` for
    the Software card): the app's own `skill` text, then which instance this
    connection reaches and whether it builds. See
    `apps/base/agent_api/README.md`, "MCP Apps".
  - one skill per guide in the `base/agent_api` registry: its key, title,
    summary and keywords, telling the agent to read the guide through the
    `guide` tool. The guide body never enters the package; reading it still
    checks the person's reach live.
- The workflow opens with `bilibi_agent_api::BUSINESS_CONTEXT`, the same business context and discovery rule sent by MCP initialization. Change that source to keep both connections in step. The shared `REVIEW_INSTRUCTIONS` opens `review_entries` for editable single and batch write reviews in MCP Apps hosts, with complete text reviews elsewhere, and supplies the instance banner and explicit approval rule to MCP initialization, plan/commit descriptions and the plugin workflow. `INSTANCE_INSTRUCTIONS` and `Instance::agent_notice` also require the label, role and address in the first answer and every plan review and confirmation.
- The same `Package` feeds the browser download and the CLI exporter.

## Harness adapters (`src/harness.rs`)

- A `Harness` names itself (`key`, `name`), the archive's file name for a package (`archive_name(&Package)`) and `files(&Package)`: the manifest, MCP configuration and skill files in that harness's layout.
- Adapters know formats only. They never read settings, scopes or the registry, and they do not own a settings tab.
- ChatGPT (`src/chatgpt.rs`): `plugin.json` with the `com.openai` interface and onboarding skill, `mcp.json` with `com.openai` OAuth (`mode: dcr`, `tokenEndpointAuthMethod: none`), `skills/<name>/SKILL.md`. The ZIP puts them under the package name (`bilibi-staging/`); the exporter writes them at the folder root.

### How to add a harness

1. Add `src/<harness>.rs` implementing `Harness`: names, archive name, and the manifest, MCP configuration and skill layout. Nothing else.
2. Add it to `HARNESSES` in `src/harness.rs`.
3. Submit its `Download` contribution in `src/contributions.rs` (operation = the harness key). Do not add a settings tab.
4. Run the exporter, unzip and check the files.

The builder, archive and exporter do not change.

## Download

- AI connections (`/settings/ai-connections`) is owned by `core/agent_access`. This module does not serve a settings page.
- The ChatGPT ZIP stays at `/settings/chatgpt-plugin/download` (a `Download` contribution). AI connections and the AI status-bar panel link it with a Download plugin button, shown only when the person's own session and an HTTPS `APP_ORIGIN` would pass the checks below (`bilibi_agent_access::PLUGIN_DOWNLOAD`, `https_origin`). It checks self-service access and the HTTPS origin on every request:

| Request | Answer |
| --- | --- |
| Signed out | Sent to sign in |
| Impersonation or agent facet | 403 |
| Unexpected query fields | 400 |
| HTTP `APP_ORIGIN` | 409 |

- The ZIP is for a ChatGPT administrator. A public installation needs an HTTPS address ChatGPT can reach and automatic OAuth registration. A LAN or VPN installation also needs OpenAI's [Secure MCP Tunnel](https://developers.openai.com/api/docs/guides/secure-mcp-tunnels), because a VPN on a person's computer does not let ChatGPT reach Bilibi; login and token exchange must work through it, not only MCP.
- The download does not create or approve a grant. The archive
  holds public configuration and skills only: no token, credential, record or
  executable.

## Exporter

```sh
bilibi-web plugin-export --harness chatgpt --output <new-directory>
```

- Same builder as the download, from `APP_ORIGIN` (required). It refuses an
  existing output directory, an unknown harness and an
  HTTP origin, each with a message.
- It writes a local package; it does not publish or register it.
- It is an exporter, not a client. A general Bilibi CLI client over `/api/v1`
  does not exist yet.
