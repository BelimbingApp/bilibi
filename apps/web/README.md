# Discovered pages

Register HTTP pages with `Contribution::new(ROUTES, ...)` from
`bilibi_module_registry::contributions`. The named service must resolve to
`bilibi_ui::page::PageService<bilibi_tenancy::Scope, bilibi_database::Error>`,
wrapping an owner implementation of `Page<Scope, Error>`. The generic UI contract
imports no identity/database crate, so Tenancy can compose pages without a cycle.
Company's adapter in `apps/core/company/src/web.rs` is the current example.
Do not add module page paths or business dispatch switches to the host router.

`src/routes.rs` binds every discovered route at boot. Missing/wrong service
adapters, malformed paths, host-path shadows, renamed shared parameters and
ambiguous intersecting patterns fail startup. Literal children such as
`/companies/legal-entity-types` take precedence over `/companies/{id}`. Parameter
names must agree at shared nodes across HTTP methods. Paths use whole-segment
`{name}` parameters; catchalls and optional/embedded parameters are unsupported.
The registry also rejects identical patterns with different parameter names.

When a page's path changes, keep the old one working with a `Moved`
contribution (`MOVED`) from the owning module, as Person does for `/persons*`
→ `/contacts*`, not a host route or a redirect in the owner's page: Web binds it
at boot under the same shadow and ambiguity rules, requires `to` to be a GET
page's exact pattern, and answers GET with a 308 that keeps the parameter
segments and the query string. It checks no access; the destination does.

Access is checked before the owner renders: Public, SignedIn, Capability(key),
Operator, or OperatorCapability(key). Operator requires the persisted Tenancy
proof, never a tenant-ID convention or an admin grant. OperatorCapability
requires both proof and capability; direct denials still win. Browser GETs
without a valid session redirect to login. Authenticated refusals return 403.
Other methods also require the configured Origin and, for an authenticated
scope, `X-CSRF-Token`. The secret rotates when the session switches identity
(impersonation), so a request from a page rendered before the switch is refused;
the live socket likewise binds the identity and secret it was opened under
(`live::Identity`) and reloads its page, without acting, once they differ. Owners must reauthorize mutations inside their transaction.
The adapter receives the operation, proven optional scope, path/query values,
bounded request body and presentation flag. HTML escaping belongs to the owner.

Framing comes from `Sec-Fetch-Dest: iframe` (or `frame`) on each HTTP navigation,
never a cookie, account setting or query parameter. A route with `framed: false`
returns 403 inside a frame. Allowed pages use Base UI's `Framed` template, copied
from Bilimbi's chromeless layout marks/classes; no shell, menu or bell is loaded.
CSP permits same-origin frames. The frame flag grants no authority. The legacy
socket carries a cosmetic framing hint so mounts and refreshes keep that layout;
it rechecks route access and framing eligibility on mount/events/refreshes.

# Downloads

A route that answers with a file (Bilimbi's download and export controllers:
a source document, a return's CSV, a stored PDF, the auditor ZIP) is a
`Contribution::new(DOWNLOADS, …, &Download { path, service, operation, access })`
from `bilibi_module_registry::contributions`, never a `Route` whose page writes
bytes, and never a host route of its own (that is `media.rs`'s signed stream).
The service resolves to `bilibi_ui::download::DownloadService<Scope, Error>`
wrapping the owner's `Download`, whose `serve` returns a `FileBody` (bytes, the
media type the owner knows, the file's own name, `Attachment` or `Inline`).
`apps/web/examples/download-reference.rs` is the running specimen.

Downloads are GET only, share the page routes' path rules and pattern space
(an intersecting page or host path fails boot), and are never `Public`: access
is proven on every request exactly as for a page, so an anonymous browser is
sent to sign in and a revoked grant refuses the next click with 403, and the
owner's facade is expected to authorize once more. A successful download
counts as session activity. The response headers come from
`FileBody::headers` alone: the content type (a `text/*` type gets UTF-8), an
RFC 6266 `content-disposition` with an ASCII fallback and `filename*`,
`cache-control: private, no-store`, `nosniff`, and a policy that forbids
scripts (sandboxing an attachment) and foreign framing. A refused or missing
file is a bare 403 or 404 (the error layer renders the page for a browser);
no missing-record redirect or shell applies.

`PageBody.live` enables the browser socket adapter for owners implementing
`Page::mount` and `LiveState`. See `docs/live-pages.md` for module-owned state,
targeted panels, URL changes and guarded subscriptions. HTTP-only adapters should
return `live: false`. F6 owns queued events and retained fragment/iframe lifecycle.

# Agent edge (`src/api.rs`)

The host owns the machine endpoints; discovered pages cannot claim them
(`api::reserved`: `/api/v1/…`, `/.well-known/…`, `/oauth/device_authorization`,
`/oauth/token`, `/mcp`, `/webhooks/…`).

- Discovery: `/.well-known/oauth-authorization-server`,
  `/.well-known/oauth-protected-resource[/mcp]` and `/.well-known/bilibi`, all
  built from `APP_ORIGIN`, never the Host header.
- `POST /oauth/device_authorization` and `POST /oauth/token` take urlencoded or
  JSON fields and delegate every decision to `core/agent_access.grants`
  (device, refresh and PKCE code grants). Errors are `400 {"error":code}`;
  storage failure is `503 temporarily_unavailable`. Each endpoint has its own
  per-sender bucket from `agent_access.oauth_requests_per_minute`. The sender
  is the peer address, or the first non-loopback `X-Forwarded-For` hop when the
  peer is a loopback proxy. `main.rs` serves with `ConnectInfo` for this.
- `/api/v1`: `GET search` (`q`, `limit` 1–50, `kind` read|write,
  `include_unavailable`), `GET operations/{key}`, `GET guides/{key}`,
  `POST operations/{key}/call` (`{"input":{…}}`), `POST plans`
  (`{"operations":[…]}`) and `POST plans/commit` (`{"plan_code","operations"}`),
  with Bilimbi's status codes and `{"error":…}` bodies. Unknown paths and
  methods also answer JSON. Bodies are capped at 2 MiB.

Every `/api/v1` request proves the actor live: `AgentAccess::authenticate` maps
the bearer to claims, then `AgentAccess::seal` reproves the grant, the account
and its party facets and seals a fresh scope with the agent facet. Plans use
the `Grant` `PlanAuthority`, which runs `AgentAccess::prove` again with the
grant row locked inside the plan transaction. A revocation that commits
mid-request therefore refuses the commit with 401. A refused bearer gets
`401 invalid_token` with `WWW-Authenticate: Bearer`. API tokens carry no
resource, so an MCP-bound token is refused here, and an API token is refused
at `/mcp`. `/mcp` uses the same `authenticate` and `Grant`; there is no second
credential check anywhere in the host.

The `record` middleware writes one incoming Integration exchange per request,
refusals included. It records method, path, the operation key (or
`agent_api.plan`/`agent_api.commit`), status, duration, connection id and the
self-reported `X-Bilibi-Agent-Id`/`X-Bilibi-Run-Id` labels; bodies, queries,
headers and tokens are never recorded. Sealed requests record a tenant row;
everything else records a system row. The trace in the `x-bilibi-trace-id`
response header is the exchange's correlation ID and the audit trace of any
mutation that request committed.

# MCP (`src/mcp.rs`)

`/mcp` is stateless Streamable HTTP with JSON replies: no SSE, no session
store, no server-initiated messages. The host negotiates in this order and
answers like Bilimbi: a foreign `Origin` is `403 invalid_origin`; an unknown
`MCP-Protocol-Version` is `400 unsupported_protocol_version` (absent is fine);
anything but POST is `405` with `Allow: POST`; an `Accept` lacking both
`application/json` and `text/event-stream` is `406`; the per-sender bucket from
`agent_api.mcp_requests_per_minute` answers `429` with `Retry-After: 60`.
Malformed JSON is a `400` JSON-RPC parse error. Notifications answer `202` with
no body. Bodies are capped like `/api/v1`.

`initialize`, `ping`, `tools/list` and the two notifications are public. Every
other message passes `api::authenticate` with the MCP resource
(`APP_ORIGIN/mcp`) as audience; a refusal is `401 invalid_token` with
`WWW-Authenticate: Bearer resource_metadata="…/.well-known/oauth-protected-resource/mcp"`.
The protocol adapter is `bilibi_base::agent_api::mcp`: six generic tools
(`search`, `describe`, `guide`, `read`, `plan`, `commit`) over the one Agent
API, so permission and domain decisions happen exactly where `/api/v1` makes
them. Business refusals are tool results with `isError: true` and Bilimbi's
bodies (`invalid_input`, `forbidden`/`missing_capability`, `writes_not_enabled`,
`plan_already_used`, …); wrong tool arguments and unknown tools are JSON-RPC
`-32602`. A grant that stops proving live inside a plan transaction is `401`,
as at `/api/v1`; storage failure is `503`. Commit results whose items carry a
`path` get an absolute `url` from `APP_ORIGIN`. Each request is one exchange
row with operation `mcp.<tool>` or `mcp.<method>`.

# Agent plugins

The downloadable plugin package for an agent harness (Plugin for ChatGPT at
`/settings/chatgpt-plugin`) is a `core/agent_plugins` page and `Download`,
not a host route. It points the harness at this `/mcp` edge from
`APP_ORIGIN`. The host only parses the exporter subcommand:

```sh
bilibi-web plugin-export --harness chatgpt --client <registered-id> --output <new-directory>
```

See `apps/core/agent_plugins/README.md`.

# Webhooks (`src/webhooks.rs`)

`POST /webhooks/{identifier}` is the only signed machine namespace. Register a
handler with `Contribution::new(WEBHOOKS, …, &Webhook { identifier, service })`
from `bilibi_base::integration::webhooks`; the service must resolve to a
`WebhookService` wrapping a `WebhookHandler` (`verify`, then `handle`). Boot
fails when it does not. Owners decide signatures, credentials, replay and
idempotency; `apps/base/integration/src/reference_webhook.rs` is the complete
specimen (`/webhooks/integration-reference`). Do not add a per-provider route,
parser or verifier to the host.

The host reads the exact bytes up to `webhooks.max_bytes` whatever
`Content-Length` says, within `webhooks.read_timeout_ms`, for any content type
and without cookies, CSRF or Origin. It admits attempts per sender per handler
(`webhooks.sender_rate_limit`) before the body is read and verified deliveries
per handler (`webhooks.rate_limit`) within a fixed `webhooks.window_ms` window
whose settings are read once when it opens; failed verifications never consume
verified capacity. The sender is the peer, or the forwarded client behind a
loopback proxy, as for OAuth. A panicking callback refuses that delivery.
Responses are fixed: `202 {"status":"accepted"}` or `403 {"error":"delivery_refused"}`.

Every attempt is one audit action `webhook.delivery` as guest/0 with the
registered identifier (or `unknown`) and a host reason (`accepted`,
`verification_refused`, `handler_refused`, `body_too_large`, `body_unreadable`,
`host_unavailable`); refusals the limiter absorbs (`rate_limited`,
`unknown_handler`) become one row per identifier and reason with a `count` when
the window closes. No body, header, signature, callback error or verified
context is ever recorded, and webhooks do not enter the exchange log. A
delivery whose audit row cannot be written is refused.
