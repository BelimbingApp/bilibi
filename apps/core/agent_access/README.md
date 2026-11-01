# Agent connections (E3, E4, E5)

`core/agent_access` owns OAuth device codes, per-person connections, opaque
access/refresh tokens and PKCE authorization codes, ported from Bilimbi's
`apps/core/agent_access` (ADR 0021). E3 is the service layer; E4 adds the
`/oauth/device`, `/oauth/authorize` and `/settings/ai-connections` pages
(`src/web.rs`, `templates/`). The host OAuth metadata/device/token endpoints
and the bearer edge (E5) live in `apps/web/src/api.rs`. Use `AgentAccess`
(`core/agent_access.grants`, `src/lib.rs`) for every code, token and
connection decision; never query `core_agent_*` tables from another module or
compare a credential yourself.

## Contract

- Every secret is stored as its SHA-256 digest (`src/secrets.rs`). Raw values
  leave the service only in `DeviceStart` and `TokenResponse`. Audit changes
  carry status and expiry facts, never digests, and the module contributes
  `Redaction`s for its digest fields on top of that.
- The built-in public client is `bilibi-native` (no secret, no registration).
  Device receipts last ten minutes; polling starts at five seconds and each
  early poll answers `slow_down` and adds five seconds. Access tokens last at
  most one hour, bounded by the connection. These are protocol constants.
- The global setting `agent_access.connection_days` (90 by default, 1 to 365)
  sets a new connection's length. `connection_expiry` computes the end time a
  consent page displays; `decide` and `authorize` accept that server-computed
  value and refuse a past one. Existing connections keep their end times.
- Refresh tokens last until the connection ends and rotate on every use. A
  spent refresh token or authorization code presented again revokes the whole
  connection, and that revocation commits before the refusal is returned. Old
  access tokens stay valid until they expire. Revocation and expiry apply to
  every token of the connection.
- The parent connection row is locked before any receipt, code or token row,
  everywhere, so concurrent redemption, refresh and replay cannot invert lock
  order. A receipt is spent by a conditional update, so a racing poll loses.
- `authenticate` is the one unscoped read: it maps a live access token to
  `AgentClaims` (connection, tenant, user and recorded party facets); the
  claims are never a scope. `seal` (and `prove`, on the caller's transaction,
  optionally locking the grant row) is the live actor proof for one request.
  It checks the grant is live against the wall clock, re-authenticates the
  account, reproves each recorded party facet, and returns that fresh scope
  with the tenancy agent facet. A failed eligibility proof is `invalid_token` at the host edge.
  `seal` bounds workplace row-lock waits to 150 ms and surfaces contention as a retryable database error; the [host contract](../../web/README.md#agent-edge-srcapirs) owns the response.
  `observe` records the ID an authenticated request stated (or `(not stated)`),
  keeps the newest 20 distinct IDs and never refuses a changed ID. An ID is
  self-reported and grants no authority. It keeps the request's
  `SelfReport` (ADR 01) the same way in `self_reported_agents`; `seal` and
  `prove` put it on the agent facet, so audit records it with every write.
- `list_connections` and `revoke` are the person's own connections only.
  Bilimbi refuses impersonated sessions and agent facets there. The one gate
  `self_service` refuses a scope carrying the agent facet or an impersonating
  operator (`access_denied`).
- MCP apps register themselves through `POST /oauth/register` (RFC 7591) or an HTTPS client ID metadata document. No operator-maintained client list or client secret is needed. See [Client registration](#client-registration).
- PKCE S256 and the MCP resource are mandatory; the scope is always `bilibi`. `agent_access.authorization_code_seconds` bounds the code (120 by default). `agent_access.oauth_requests_per_minute` is the host's per-sender, per-endpoint limit on each node. Changing these settings needs `admin.system.agent-access.manage`.

## Pages

`core/agent_access.pages` serves the three signed-in routes through the discovered page contract; `src/web.rs` mirrors Bilimbi's `device_live.ex`, `authorize_live.ex` and `connections_live.ex` and the templates compose the shared Base UI macros in place (`askama.toml` adds `base/ui/templates` to the template path).

Both consent pages open with the instance banner (`bilibi_system::Instance::banner`, `apps/base/system/README.md`), so the person sees which Bilibi they connect the agent to. The authorization review also shows the configured public origin (`APP_ORIGIN`) as the Bilibi address, separately from the client's return address. Its shared form groups the connection facts, access explanation and primary approval action; Your connections stays available in the page header.

Opening `/oauth/device?user_code=…` only fills the code. The person reviews the stated agent and connection end time before approving or refusing. `/oauth/authorize` validates the client, callback, resource and PKCE before rendering consent. It labels the app name **self-declared** and shows both the return host and full address. A registration establishes no verified app identity or permission.

When an app name matches a live connection, both consent pages show the existing connections, their approval dates and last use. Replace existing connections is selected by default only when none of the matching connections has been used; otherwise Keep both is selected. Approval needs the choice and never revokes without it; Replace revokes the reviewed matching connections in the same audited transaction. Matching uses the stated name, which does not verify identity. The facade's `decide` and `authorize` never revoke; only the consent pages replace. Consent is browser-only; agents cannot approve connections. The forms accept only this choice, and the server keeps the reviewed IDs and expiry. An approve or refuse returns `code` or `error=access_denied`, `state` and `iss` (the configured `APP_ORIGIN`). Expiry, revocation, account checks, audit and step-up policies apply as before.

`/settings/ai-connections` lists the person's grants, live connections first and newest first, with their last use or Never used. It revokes through the shared confirmation dialog. Its copyable prompt gives this instance's MCP address and the exact `claude mcp add` and `codex mcp add` commands, without client IDs. Under it, "If the prompt does not work" repeats those two commands with a short sign-in note. A note beside the setup steps tells the person to start a new conversation after connecting and signing in, whether they used the prompt or the manual commands, and to quit and reopen the app if the tools do not appear. `setup_prompt::identity` names the instance once (a label that only repeats the role is not said twice). Impersonation and agent facets cannot set up connections. `/settings/agent-connections`, `/settings/mcp` and `/settings/chatgpt-plugin` redirect here. Below the prompt it offers Download plugin (the ChatGPT ZIP that `core/agent_plugins` serves at `PLUGIN_DOWNLOAD`), with one line saying the plugin connects from OpenAI's cloud and so needs a public address or OpenAI's Secure MCP Tunnel; the button shows only when `offers_plugin` holds (the person's own staff session and an HTTPS `APP_ORIGIN`, the download's own checks). When `Instance::public_address` is false it adds a short help section before the prompt: tools on this computer connect directly, Node-based tools in WSL need `NODE_EXTRA_CA_CERTS` pointing at the mkcert root, and apps whose connectors run in the vendor's cloud need a public tunnel address.

### The AI tag in the status bar

The `shell.ai` embed (`src/ai_status.rs`, `templates/ai_status.html`, operation `ai_status` of `core/agent_access.pages`) is the AI tag every signed-in person sees in the status bar's centre and the panel it opens. Web renders it into the shell and mounts it as the shell panel `#app-ai` (`apps/web/README.md`). It shows the person their own AI and nothing else:

- The tag reads AI in every state. A faint border means no live connection; the success colours (green) mean at least one. The tooltip names the connected apps and the last use ("Claude, ChatGPT · last used 5 min ago") or says "No AI app connected".
- Not connected: the same prompt as this page (`setup_prompt`), the `{origin}/mcp` address, a copy icon for each (`copy_box`, `copy_button` in `base/ui`; a check shows for a moment, and a refused copy changes nothing), and a link to the manual steps on this page. When `Instance::public_address` is false (a Dev copy, or an address only this machine or network can reach), the panel says to use an AI program on this computer, because AI websites and bots cannot reach it. The same Download plugin button and line as the page sit under the address.
- Connected, under the heading **Connected AI**: each live connection (`list_connections`) with its last use and end date, both the shared datetime component in the time zone chosen in the top bar (`templates/datetime.html`); a Manage link to this page (the only link to it, there is no footer link); and **Recent activity**, the person's recent agent requests in words (`Integration::own_agent_activity`, `apps/base/integration/README.md`), newest first, each naming its app (marked "(disconnected)" once revoked; revoked apps' history stays) and its time as a datetime, with refusals named.
- Each activity line says what the AI operated on, from the kinds, counts and ids recorded with the request (`bilibi_integration::Target`, written by the host edge, `apps/web/README.md`): "Read 12 companies", "Saved a change to Company Acme Sdn Bhd", "Prepared a change to 3 invoices for you to review". A single record is named through `Records::resolve` with the reader's own scope, so a masked field, an out-of-reach company, a removed record or an unmounted kind shows only the kind and count. No name is stored. A request recorded without a kind (before kinds were recorded, or an operation no record kind owns) shows the operation's registered title or the generic line.
- The short list leaves out the MCP handshake rows, looking up an action and reading how one works, and opening an app card; they stay on the full activity page. The Recent activity heading links to it (`/system/api-exchanges?direction=incoming`, behind `admin.system.outbound-exchange.list`) only for a reader who holds that capability; others see no link, because the page would refuse them. Connecting another app sits in a fold.
- The panel follows the person's own changes live through `AgentAccess::subscribe_connections` (approve, revoke) and `Integration::subscribe_agent_activity` (a recorded request).
- An impersonating or agent session sees neither prompt nor connections, only that AI apps are connected from the person's own account.

## Connect a local agent

Give your app this instance's MCP address and sign in through your browser.

| App | Add from your project folder |
| --- | --- |
| Claude Code | `claude mcp add --scope project --transport http bilibi "$APP_ORIGIN/mcp"` |
| Codex | Put `[mcp_servers.bilibi]` with `url = "<APP_ORIGIN>/mcp"` in the trusted project's `.codex/config.toml`, then run `codex mcp login bilibi` |

In Claude Code, approve the project server and open `/mcp` to sign in. Review Bilibi's consent page before approving. To stop access, open AI connections and revoke the connection. The next request is refused.

After connecting and signing in, **start a new conversation in your AI app** before asking it to use Bilibi. Do this whether you used the page’s prompt or the manual setup above. If Bilibi’s tools do not appear, quit and reopen the app. Some clients can refresh tools while running; a restart is a fallback, not a requirement for every client.

See the current [Claude Code instructions](https://code.claude.com/docs/en/mcp) and [Codex instructions](https://learn.chatgpt.com/docs/extend/mcp?surface=cli). Codex's `mcp add` command writes user configuration; use the project file above for an isolated setup.

## Client registration

Each app supplies its own name and redirect URIs; Bilibi checks safety and matching rather than prescribing a callback path.

- `POST /oauth/register` accepts JSON `redirect_uris` and optional `client_name`, `grant_types`, `response_types`, `scope` and `token_endpoint_auth_method`. It returns HTTP 201 with an unpredictable `client_id`, issue time and accepted metadata. It supports only public clients (`none`), authorization code and refresh grants, `code` responses and `bilibi` scope. Missing app names display as “Unnamed app”. Unknown extension metadata is ignored; secrets and unsupported authentication are refused.
- Redirect URIs must be HTTPS or HTTP loopback (`localhost`, `127.0.0.1`, `[::1]`). Any path is allowed at registration. HTTPS matches exactly; HTTP loopback matches the host, path and query exactly while allowing any port ([RFC 8252 §7.3](https://www.rfc-editor.org/rfc/rfc8252#section-7.3)). The actual callback must match exactly at code redemption. Fragments, credentials, wildcards and custom schemes are refused. The target clients use HTTPS or loopback; no custom scheme is needed.
- A non-URL client ID must already be registered before authorization. Registration alone cannot create a grant. The person still consents, and PKCE proves possession during code exchange.
- The authorization-server metadata advertises both `registration_endpoint` and `client_id_metadata_document_supported`. The [current MCP specification](https://modelcontextprotocol.io/specification/2026-07-28/basic/authorization/client-registration) prefers metadata documents; DCR remains for compatibility. [Codex](https://learn.chatgpt.com/docs/extend/mcp?surface=cli), [Claude Code](https://code.claude.com/docs/en/mcp) and [ChatGPT](https://developers.openai.com/plugins/build/auth) support this discovery.
- Metadata client IDs must be HTTPS URLs with a path, no credentials, fragment, query or dot segments. Fetches use `base/integration` with public DNS addresses pinned for the connection, public TLS roots, no proxy or redirects, a five-second timeout and a 16 KiB limit. Invalid or unavailable documents fail closed. The document must have a matching `client_id`, `client_name`, safe `redirect_uris` and public-client authentication. ChatGPT's plural authentication-method field is intersected with `none`. Valid documents cache for up to one hour, respecting `no-store`, `no-cache`, `max-age` and `Age`; the default is five minutes. Cache-miss fetches have a node-wide OAuth request limit as well.
- Registration receipts are stored in `core_agent_oauth_clients`. Every DCR request prunes at most 100 registrations that were last used over 30 days ago and have no connection that is live or ended in the last 30 days, so an app that still holds its client ID for a current or recent grant can renew. The 30 days is fixed, not a setting. Receipt writes and pruning are pre-authentication lifecycle housekeeping, like device receipts. They assert no tenant or person. Consent and grant actions remain audited business mutations.
- Migration `004-client-registration` deletes the retired setting. Existing access tokens, refresh tokens and pending authorization codes retain their consented client and callback binding until expiry or revocation. Their use never admits a new authorization for an unregistered ID.

## Agent plugins

The package builder and harness adapters live in [core/agent_plugins](../agent_plugins/README.md). Packages use discovery and automatic registration without an operator-selected client. Downloading or exporting a package grants nothing; the person's browser consent still controls access.

## Injected identity

Company/person validation is injected, as the port plan requires. On every
issuance (`exchange`, `refresh`, `exchange_code`) the module reproves the
granting account through `base/tenancy.authentication` (tenant and account
must still authenticate) and each party facet recorded on the connection
through `base/tenancy.parties`. New connections record the staff actor's Person
facet and the absence of the customer-only organisation and affiliation facets.
Every issuance and bearer request requires the fresh actor's facets to equal
them exactly. A suspended account, a retired Person, or a Person link changed
since consent makes the grant `invalid_grant` (`invalid_token` at the bearer
edge) without spending the presented token; the person connects again from the
account as it now is. Desk changes do not rewrite an ordinary OAuth connection,
but each operation and reviewed plan still proves its exact workplace. The User
account binder supplies the identity facets to the browser session and to this
module's bearer proof alike. This module runs
no SQL against account, company or person tables; the schema references them
only through scoped foreign keys (`migrations/001-agent-access.sql`).

## Audit

Company provider access tokens optionally use these same grants without OAuth issuance. Core User owns signature verification, the company trust, exact linked account and provider workplace; this module stores only a token digest and the configuration/link binding, ending with the provider expiry. A request with no company input runs in that provider workplace, never in the account's remembered browser desk. First use and revocation are audited; no raw JWT or claim set is retained. Every request and plan reproof checks the saved binding and live desk, so disabling or saving the provider, unlinking the subject or removing the desk invalidates earlier use. A revoked token cannot recreate its connection. See [provider access tokens](../user/sso.md#provider-access-tokens) for the HTTP selector and setup. Migration `core-agent-access-005-provider-access` adds these nullable grant bindings; ordinary OAuth connections keep their existing lifecycle.

Consent, refusal and owner revocation commit as the signed-in `user`. Token
redemption, rotation, replay revocation and observation commit as actor kind
`agent` with the granting account's ID, naming the connection in the change.
Receipt polling (`authorization_pending`, `slow_down`) is pre-authentication
housekeeping like a password-reset token and is not a business mutation.

## Walkthrough

Against a disposable database after the host's `migrate`, appoint
the fixture operator (`INSERT INTO platform_operator(singleton,tenant_id)
VALUES(true,1)`) so the setting writes succeed, then run

```sh
BILIBI_DISPOSABLE_AGENT_ACCESS_DEMO=yes DATABASE_URL=... \
  cargo run -p bilibi-agent-access --locked --example agent-access-walkthrough
```

It prints the operator setting taking effect; device start, pending, `slow_down` with the growing interval, review, approval, redemption and spent receipt; digest-only storage; bearer claims and resource binding; provider access through the exact linked company's live staff desk; bounded held-company refusal; company-before-desk archive contention; agent-plan stability across an unrelated browser desk switch; commit refusal after the explicit target desk is removed; observed IDs and the 20-entry limit; refresh rotation, replay revocation and its effect on every token; refusal; receipt and connection expiry through explicit SQL time travel; the injected account proof refusing a suspended account; owner revocation and tenant isolation; the full PKCE consent/redemption/replay path for a registered client; the host throttle; and the audit rows. It uses only its own disposable rows and resets the connection-length setting it changed. Raw codes and tokens are never printed in full. This is an opt-in demonstration, not a test suite.

`observe` also keeps the newest 20 transport client labels in `observed_clients`, separately shown as "Client identity". Its observation audit carries both that label and any valid self-report from the same request. `exchange_code_from` is `exchange_code` with the host-observed `ClientIdentity` on its action audit; a later tool report cannot fill an earlier immutable action.
