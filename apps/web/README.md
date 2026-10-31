# Sign-in

The host's `/` and `/login` forms continue to `/login/challenge` when [Core User's sign-in policy](../core/user/README.md#sign-in-admission) requires 2FA.

The pending credential uses a separate HttpOnly, SameSite=Lax cookie scoped to `/login/challenge` so a provider redirect can reach the challenge. It never becomes `bilibi_session`. GET reads enrolled methods only. POST checks Origin and the pending CSRF before starting a challenge, verifying a code or cancelling. Admission replaces it with a fresh session cookie and follows the account's normal landing page; expiry returns to sign-in. The anonymous shared card has no shell or live transport.

The optional external credential adapter uses `GET /login/sso/{provider}/start`, `GET /login/sso/callback`, and signed `POST /login/sso/backchannel/{provider}`. These are identity lifecycle routes, not per-provider endpoints. Core User owns the provider-neutral registry and every decision; the host owns session checks, a separate browser-binding cookie, the protocol callback response and normal admission cookies. Back-channel delivery uses signed protocol proof instead of browser Origin/CSRF. See [company sign-in](../core/user/sso.md). Start links use top-level navigation so CSP stays unchanged. State and nonce bind the callback to its initiating browser; starts, callbacks and back-channel deliveries each share the existing per-sender OAuth throttle. Expired flow records and their encrypted material are removed in bounded batches on the next start for that connection, and ended external-session records, expired proofs, challenges and logout replays on the next write of their kind. Callback responses are not cached and send no referrer.

# SCIM provisioning

The host exposes the provider-neutral identity lifecycle adapter at `/scim/v2/{provider}/{resource}[/{id}]`, delegating bearer proof, company isolation and all provisioning decisions to Core User. Responses use SCIM JSON, bounded request bodies and the existing per-sender OAuth throttle. Credentials and bodies are never logged. See [Core User SCIM](../core/user/scim.md). These routes are independent of browser cookies and agent tokens; discovered pages cannot shadow `/scim`.

# Discovered pages

Register HTTP pages with `Contribution::new(ROUTES, ...)` from
`bilibi_module_registry::contributions`. The named service must resolve to
`bilibi_ui::page::PageService<bilibi_tenancy::Scope, bilibi_database::Error>`,
wrapping an owner implementation of `Page<Scope, Error>`. The generic UI contract
imports no identity/database crate. Pages that call operation contracts live above
Agent API, as Core Tenant Administration does for `/tenancy/tenants`.
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
AnyOf(keys), Operator, OperatorCapability(key), Owner, or StepUp { operation, inner }. AnyOf opens a combined page for an account that holds any one of its keys (Bilimbi `{:any_of, [..]}`, a menu leaf's `Requirement::AnyOf` is the matching link): boot refuses an empty, blank or repeated key and an undeclared one, a direct deny removes only its own key, and the owner still enforces each section and write with `Capability`. Operator requires the persisted Tenancy
proof, never a tenant-ID convention or an admin grant. OperatorCapability
requires both proof and capability; direct denials still win. Browser GETs
without a valid session redirect to login. Authenticated refusals return 403.

**Portal pages** ([ADR 16](../../docs/architecture/decisions/16-customer-accounts-and-portal.md), `src/portal.rs`). `Portal(key)` opens a page, download or `EmbedContext::Portal` embed to a customer holding portal capability `key`. Such routes live only under `/portal/`, are never framed, and are bound apart from staff routes, so page search, the shell and the live socket never list one. The host reads only the `bilibi_portal` cookie there and only `bilibi_session` elsewhere; a session resolves only for the account kind its cookie serves (`Sessions::check` staff, `check_portal` customer). The owner's service is a `PageService<PortalScope, Error>` or `DownloadService<PortalScope, Error>`, and the host wraps its page in the portal layout (`bilibi_ui::PortalLayout`): the portal site's title (Core Customer Access; the tenant's name while it has no site), the portal menu (`Menu::visible_portal_tree` against the customer's effective capabilities) and the account's links. `POST /portal/sign-out` ends the customer's own session (`UserService::portal_logout`) and clears the portal cookie. A GET with no customer session, or with a staff one, goes to `/portal/sign-in`, which returns to the portal the browser last signed in to. A portal step-up still owed is refused with 403 until the portal has its own prompt. Boot refuses the wrong shapes listed in `apps/base/module_registry/README.md`; `examples/portal-reference.rs --refusals` shows each.

**Portal credential pages** (`src/portal_auth.rs`). Each sits under the address of its tenant's portal site, which names the tenant and brands the card (`PortalCard` around a `PortalForm`); an unknown address is a 404 card naming no tenant.

| Path | Does |
| --- | --- |
| `/portal/sign-in/{site}` | `UserService::portal_login` among that tenant's customers; throttled per sender like `/login` |
| `/portal/forgot/{site}` | `request_portal_password_reset`; one answer whether or not the address is a customer |
| `/portal/reset/{site}/{token}` | `portal_reset_password`, then back to sign-in |
| `/portal/accept/{site}/{token}` | Core Customer Access redeems the invitation, then back to sign-in |

- Sign-in alone sets `bilibi_portal`: `HttpOnly`, `SameSite=Lax`, `Path=/portal`, `Max-Age` one day, `Secure` on an `https` origin, holding the fresh token each sign-in issues.
- One browser holds one portal session: signing in to any portal ends the session the cookie held before, this tenant's or another's.
- `bilibi_portal_site` (same flags, one year) remembers the last portal's address for `/portal/sign-in`.
- A tenant that requires a second step for `core.user.portal_sign_in` refuses customers with a message, and the pending sign-in is cancelled; the portal has no challenge page yet.
- Recovery is throttled per sender before any password work: forgotten-password and reset requests, staff and portal together, are limited by `user.recovery.per_sender_per_minute` (default 5); over it, the form answers 429 with a message that names no account.
- **Credential links** (`/reset-password/{token}`, `/portal/reset/{site}/{token}`, `/portal/accept/{site}/{token}`, `CREDENTIAL_PATHS` in `src/lib.rs`): the token is a bearer secret. The failure log and the debug error report show `[redacted]` for it, for their query and for a `Referer` that is one. Their pages send `Referrer-Policy: strict-origin`, so a following request carries only this origin (not `no-referrer`, which makes the browser post the page's own form with `Origin: null`). The cards also send `Cache-Control: no-store`.

The top-bar search (`src/shell.rs`, `search_pages`) lists a GET page whose path has no parameter when the person may open it, judged as the menu judges (display only; the route checks again when opened), and only when it has a name: `Page::title` for its operation, else its menu row. A page with neither is not found. A route that only redirects is listed under its title when the person may also open where it leads, so an old address kept working by a redirect returns no title. The dialog's records are the `shell.search.records` shell embed (Base Search), which the shell renders into the dialog and the live socket mounts as the panel `#app-search-records`, as it does the bell (`shell.notifications`) and the status bar's AI tag (`shell.ai`, Core Agent Access, mounted as `#app-ai`).

Step-up (`apps/base/step_up/README.md`) is the same on every surface:

- `StepUp { operation, inner }` checks `inner`, then `StepUp::require` for the declared `operation`. A GET that owes a check is redirected to the 2FA prompt (the method choice when several are enrolled) with `return=<address>`; any other method, an embed or a panel is refused with 403. Boot refuses a step-up of `Public`, of another step-up, or of an undeclared operation. Menus and tile pickers check only `inner`.
- A live socket on such a page rereads the window on each event and 15 s refresh without extending it. Once the window has run out, the socket reloads its address so the HTTP guard asks again.
- When an owner's facade answers `Error::StepUpRequired` during a GET render or a live event, the browser goes to the prompt and comes back to the page; nothing acted.
- `/api/v1` commit and the MCP `commit` tool take an optional `step_up_code` and answer `step_up_required` (`bilibi_step_up::refusal`): 403 on the API, an `isError` result on MCP.
- Base System supplies the live enforcement policy from the saved `instance.role`: Dev skips ordinary checks; role changes and other `.always()` operations always require a fresh code.

A live page or panel event runs under the page-write permit its service declared (`PageWrites`, `base/agent_api.pages`): the host wraps the owner's `update` in `bilibi_database::page_write::during`, so a business transaction the declaration does not cover is refused before it writes. Refresh and params carry no permit. Contract: `apps/base/agent_api/README.md` (Page writes declaration and check).

`POST /session/company` is the top bar's company switcher (`bilibi_ui::company_bar`, `src/shell.rs` `choose_company`): an ordinary form with the session's CSRF secret, the chosen company (empty for the account's own) and the address to return to, which must be a path on this site. Core User proves the company is one the account reaches and records the switch; the browser comes back to that address under the new company, and a company the person does not reach answers with a flash there. The shell shows the current company (`Scope::current_company`) in the top bar and names both it and the affiliation in the account menu; the choices come from `PartyProofs::reachable_organisations`. Contract: [Core Company README](../core/company/README.md#the-current-company).

Other methods also require the configured Origin and, for an authenticated
scope, `X-CSRF-Token`. The secret rotates when the session switches identity
(impersonation), so a request from a page rendered before the switch is refused;
the live socket likewise binds the identity, the secret and the current company it was opened under (`live::Identity`) and reloads its page, without acting, once they differ. The company is the one the page was rendered under: stamped as `data-company-id` on the shell root and sent by `live.js` as the `company` query of `/live`, so a reconnect after the session switched company reloads the page rather than re-rendering under a stale top bar. Owners must reauthorize mutations inside their transaction.
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

An optional `X-Bilibi-Identity-Provider` header selects a company's explicitly enabled provider-token trust instead of Bilibi OAuth. Duplicate selectors refuse the request. Core User verifies the provider signature, resource audience, scopes and exact existing company/account link; Core Agent Access records a digest-only revocable grant and uses the same sealing, plan authority, step-up and audit path. API and MCP audiences are distinct. See [company sign-in](../core/user/sso.md#provider-access-tokens) for opt-in, token shape and revocation limits.

The `record` middleware writes one incoming Integration exchange per request, refusals included. It records method, path, the operation key (or `agent_api.plan`/`agent_api.commit`), status, duration, connection id, the self-reported `X-Bilibi-Agent-Id`/`X-Bilibi-Run-Id` labels and the agent's self-report and a separately labelled client identity; bodies, queries, other headers and tokens are never recorded.

The agent's self-report (ADR 01, "Agent identity at runtime") is `X-Bilibi-Agent-Self-Report: harness/model-version-effort`, for example `amp/gpt-6-astra-high`; at `/mcp` it may instead come in `params._meta` under `bilibi/agent-self-report`, which replaces the header's value. A model can send the optional `agent_self_report` argument on `search`, `describe`, `guide`, `read`, `plan` or `commit`; it takes precedence over both. The host consumes it before dispatch, so it never enters business inputs or a plan's comparison. MCP connect instructions and tool descriptions tell agents to report on every call, ask their user for unknown parts, never guess, and omit parts nobody can give. `bilibi_tenancy::SelfReport::parse` checks the shape only: one `/`, either side may be empty but not both, ASCII letters, digits, `.`, `_`, `-`, at most 120 characters. A missing or malformed value never refuses a request: a malformed one is dropped and the exchange records that it was malformed. A valid one rides on the sealed scope's agent facet (and on the `Grant`'s reproved scope), so it lands on the exchange row, on every audit mutation and action of a write it commits (`agent_self_report`), and in the connection's bounded list of distinct reports (`AgentAccess::observe`). It is labelled "Model (as reported)" wherever it is shown and is never used for authorization. Sealed requests record a tenant row; everything else records a system row. The trace in the `x-bilibi-trace-id` response header is the exchange's correlation ID and the audit trace of any mutation that request committed.

**Client identity** is transport evidence, separate from the agent's self-report. The edge records a bounded, printable User-Agent as `User-Agent: …` on the exchange, the sealed scope and write audit, and in the connection's newest 20 observed clients. MCP requests without usable User-Agent record `MCP: client identity not supplied`. Initialize records bounded `clientInfo` name/version as `MCP clientInfo: name/version` on that exchange; public initialize metadata is never matched to later requests on this stateless transport. OAuth code redemption also records User-Agent when supplied. Audit Actions, Data Mutations, record history, Exchanges and Your connections show client identity separately. It grants no authority and never supplies unknown model details. Earlier immutable audit rows stay as originally recorded.

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

`initialize`, `ping`, `tools/list`, `resources/list`, `resources/read` and
the two notifications are public: the tool list and a card's HTML hold no data
and no credential, and an MCP Apps host may fetch them before the person signs
in. Every other message passes `api::authenticate` with the MCP resource
(`APP_ORIGIN/mcp`) as audience; a refusal is `401 invalid_token` with
`WWW-Authenticate: Bearer resource_metadata="…/.well-known/oauth-protected-resource/mcp"`.
The protocol adapter is `bilibi_base::agent_api::mcp`: six generic tools (`search`, `describe`, `guide`, `read`, `plan`, `commit`) over the one Agent API, so permission and domain decisions happen exactly where `/api/v1` makes them, plus one tool per registered MCP App. Each app tool points the host at its card's `ui://` resource and dispatches through its registered source (`apps/base/agent_api/README.md`, "MCP Apps"). Business refusals are tool results with `isError: true` and Bilimbi's bodies (`invalid_input`, `forbidden`/`missing_capability`, `writes_not_enabled`, `plan_already_used`, …); wrong tool arguments and unknown tools are JSON-RPC `-32602`. A grant that stops proving live inside a plan transaction is `401`, as at `/api/v1`; storage failure is `503`. Commit results whose items carry a `path` get an absolute `url` from `APP_ORIGIN`. Each request is one exchange row with operation `mcp.<tool>` or `mcp.<method>`.

# Versions (`bilibi-web status`)

`bilibi-web status` prints what the program is made of: the Platform version, its commit and release tag, and each mounted Domain or Extension with its version and commit, as `scripts/discover-modules.py --pin` recorded them at build time. It needs no database.

`build.rs` compiles the pin and checks Git drift. The private `src/composition.rs` validates the mounted set and exposes the stable registry `Composition` through `bilibi_web::composition`. Only this executable host watches Git, so a commit rebuilds the host and final link instead of every registry consumer.

The status bar shows the same facts through `bilibi_ui::version_badges`. The scheme, the catalogue and the states a build can be in are in `docs/versions.md`.

# Agent plugins

AI connections (`/settings/ai-connections`) is a `core/agent_access` page, not a host route. `/settings/mcp`, `/settings/agent-connections` and `/settings/chatgpt-plugin` redirect there. The page shows this `/mcp` edge from `APP_ORIGIN`. The ChatGPT plugin download stays a `core/agent_plugins` contribution. The package is named after the instance (`bilibi-staging`, "Bilibi Staging", `bilibi-staging-chatgpt-plugin.zip`), so a person can hold a Dev, a Staging and a Production connection in one harness. The host only parses the exporter subcommand:

```sh
bilibi-web plugin-export --harness chatgpt --output <new-directory>
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

# Response compression and caching

The host compresses textual responses (`text/*`, JSON, JavaScript, XML and `+json`/`+xml` types such as SVG) with gzip or Brotli according to `Accept-Encoding`.

- Compression runs at a fixed balanced quality (level 4) so a cold stylesheet request does not spend seconds of CPU. Any response with a `Content-Disposition` (private attachments, signed media, archives) and any non-text body (PDF, archives, fonts, images, octet-stream) is never recompressed and keeps its bytes and `Content-Length`, whatever its MIME type.
- Compression adds `Vary: Accept-Encoding`; already encoded responses and streams such as server-sent events retain their existing handling.
- Pages and API responses default to `cache-control: no-store`. Private downloads retain `private, no-store` and their sandboxing policy.
- Only an embedded asset served with its matching content version receives `public, max-age=31536000, immutable`. Unversioned assets keep `no-store`; an unknown asset or incorrect version returns 404 with `no-store`.
- HTML specimens remain pages with `no-store`, including the embedded specimen.
- CSP, `nosniff` and the referrer policy still apply to every response. A handler's stricter policy is preserved.

Shared templates use `bilibi_ui::Assets::url` for CSS, scripts and icons; CSS also links versioned fonts. See [the UI asset contract](../base/ui/assets/README.md#urls-and-browser-caching) for the catalogue and hashing rules.

OAuth discovery advertises `/oauth/register` and client ID metadata documents. Public client registration, redirect matching and receipt retention belong to [Agent Access](../core/agent_access/README.md#client-registration). Startup requires HTTPS for Staging and Production; Dev permits HTTP only on loopback. Token issuance rechecks the same policy if the stored instance role changes.
