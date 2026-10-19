# Agent connections (E3, E4, E5)

`core/agent_access` owns OAuth device codes, per-person connections, opaque
access/refresh tokens and PKCE authorization codes, ported from Bilimbi's
`apps/core/agent_access` (ADR 0021). E3 is the service layer; E4 adds the
`/oauth/device`, `/oauth/authorize` and `/settings/agent-connections` pages
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
  with the tenancy agent facet. A failed proof is `invalid_token`.
  `observe` records the ID an authenticated request stated (or `(not stated)`),
  keeps the newest 20 distinct IDs and never refuses a changed ID. An ID is
  self-reported and grants no authority. It keeps the request's
  `SelfReport` (ADR 01) the same way in `self_reported_agents`; `seal` and
  `prove` put it on the agent facet, so audit records it with every write.
- `list_connections` and `revoke` are the person's own connections only.
  Bilimbi refuses impersonated sessions and agent facets there. The one gate
  `self_service` refuses a scope carrying the agent facet or an impersonating
  operator (`access_denied`).
- Operator-registered MCP clients live in `agent_access.oauth_clients`: client
  IDs mapped to a name and exact HTTPS `redirect_uris` (`src/oauth_client.rs`
  validates the setting and each request). PKCE S256 and the resource are
  mandatory; the scope is always `bilibi`. `agent_access.authorization_code_seconds`
  bounds the code (120 by default). `agent_access.oauth_requests_per_minute`
  is the host's per-sender limit; `Throttle` is the per-node counter the host applies.
  Editing these needs the platform capability `admin.system.agent-access.manage`.

## Pages

`core/agent_access.pages` serves the three signed-in routes through the discovered page contract; `src/web.rs` mirrors Bilimbi's `device_live.ex`, `authorize_live.ex` and `connections_live.ex` and the templates compose the shared Base UI macros in place (`askama.toml` adds `base/ui/templates` to the template path).

Both consent pages open with the instance banner (`bilibi_system::Instance::banner`, `apps/base/system/README.md`), so the person sees which Bilibi they connect the agent to. The authorization review also shows the configured public origin (`APP_ORIGIN`) as the Bilibi address, separately from the client's return address. Its shared form groups the connection facts, access explanation and primary approval action; Your connections stays available in the page header.

Opening `/oauth/device?user_code=…` only fills the code; the person checks it, reviews the stated agent and the server-computed end time, then approves or refuses. `/oauth/authorize` validates the registered client and exact callback before consent renders, and an approve or refuse answers through the live page's `redirect` with `code` or `error=access_denied`, the client's `state` and `iss` (the configured `APP_ORIGIN`, never the Host header). Both consent forms submit no fields: an approve carrying any field is refused as a changed end time. `/settings/agent-connections` lists the person's grants newest first with status, "Also seen as" and "Self-reported agent", and revokes one only through the shared confirm dialog. It uses `base/ui/templates/settings.html` with `SettingsTab::AgentConnections`, keeping all six My settings tabs visible. Audit sees each decision as the signed-in `user`.

The pages render instants through `templates/datetime.html`, the Bilimbi
`<.datetime>` markup fed by `base/datetime`'s `DisplayContext`; it belongs in
`base/ui` once the shell's display-mode control and `DateTime` hook land (P10,
C5). The flash outlet renders inside each page until the shared shell owns it.
Agent facets and impersonation are refused by `self_service` in
`src/lib.rs`, not by the pages.

## Agent plugins

One answer: plugins are thin, per harness. This module, `base/agent_api`
and the host's `/api/v1` and `/mcp` edges are the base layer every harness
shares; none of them holds harness-specific code.

- `public_clients` lists the registered OAuth public clients (ID and name,
  never callbacks): global, non-secret configuration for the exporter.
- `clients_for(scope)` is the same list behind the `self_service` gate, for a
  person's own setup page. It refuses impersonation and agent facets.
- Neither creates or approves a grant. The `/oauth/authorize` consent still
  names the person and the end date.
- The package builder, the harness adapters (ChatGPT is the first), the
  `/settings/<harness>-plugin` pages and their downloads live in
  `core/agent_plugins` ([README](../agent_plugins/README.md)).
- The Agent connections header links to the ChatGPT setup page.

### How to add a harness

1. Add `apps/core/agent_plugins/src/<harness>.rs` implementing `Harness`:
   name, title, page path, archive name, page copy and `files(&Package)` (its
   manifest, MCP configuration and skill layout). Nothing else.
2. Add it to `HARNESSES` in `src/harness.rs`.
3. Submit its `Route` and `Download` in `src/contributions.rs`, with the
   harness key as the operation.
4. Optional: add its tab to `bilibi_ui::SettingsTab` (`base/ui/src/settings.rs`) and link it from the Agent connections header.
5. Run the app: open the page, download, unzip and check the files.

The builder, archive, page, download and exporter stay unchanged, and nothing
in this module changes. The exporter (`bilibi-web plugin-export`) writes the
same package. A general Bilibi CLI client over `/api/v1` does not exist yet.

## Injected identity

Company/person validation is injected, as the port plan requires. On every
issuance (`exchange`, `refresh`, `exchange_code`) the module reproves the
granting account through `base/tenancy.authentication` (tenant and account
must still authenticate) and each party facet recorded on the connection
through `base/tenancy.parties`. New connections record the actor's
organisation and person facets (or their absence), and every issuance and
bearer request requires the fresh actor's facets to equal them exactly, as
Bilimbi binds a connection to the actor's company. A suspended account, an
archived company, a retired Person, an unlinked affiliation, or an account
linked, unlinked or relinked since consent makes the grant `invalid_grant`
(`invalid_token` at the bearer edge) without spending the presented token;
the person connects again from the account as it now is. The User account
binder supplies those facets to the browser session and to this module's
bearer proof alike. This module runs
no SQL against account, company or person tables; the schema references them
only through scoped foreign keys (`migrations/001-agent-access.sql`).

## Audit

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

It prints the operator setting taking effect; device start, pending,
`slow_down` with the growing interval, review, approval, redemption and spent
receipt; digest-only storage; bearer claims and resource binding; observed
IDs and the 20-entry limit; refresh rotation, replay revocation and its effect
on every token; refusal; receipt and connection expiry through explicit SQL
time travel; the injected account proof refusing a suspended account; owner
revocation and tenant isolation; the full PKCE consent/redemption/replay path
for a registered client; the host throttle; and the audit rows. It uses only
its own disposable rows and resets the two global settings it changed. Raw
codes and tokens are never printed in full. This is an opt-in demonstration,
not a test suite.
