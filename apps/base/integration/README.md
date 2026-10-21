# Integration exchange log and outgoing transport

`base/integration` records incoming and outgoing HTTP exchanges in one log,
`base_integration_exchanges`, and is the only outgoing HTTP client. Domain
modules own request meaning, response parsing, retries and user-facing
outcomes. API adapters own authentication and call the incoming recorder after
dispatch. Business writes keep using the ordinary mutation audit; exchange
logging is separate.

The service is `base/integration.exchanges` (`Integration`).

## Outgoing calls: use `send`/`stream`, not `reqwest` directly

```rust
let reply = integration.send(
    Origin::User(&scope),
    &Call { system: "partner".into(), operation: "items.fetch".into(),
            owner_type: Some("domain/orders".into()), owner_id: Some(order_id), ..Call::default() },
    Outgoing::get(url).header("Authorization", token),
).await?;

// Installation-wide reference data has no tenant or person, and streams
// straight into the caller's file without holding the body.
integration.stream(Origin::System,
    &Call { system: "geonames".into(), operation: "reference.download".into(), ..Call::default() },
    Outgoing::get(url).max_body_bytes(500 << 20), &mut file).await?;
```

`Origin::User` is a session scope, `Origin::Principal` a queued job's
`SystemScope` (tenant-owned, no person) and `Origin::System` tenantless
traffic. One row describes the complete logical call: retries are off and
belong to the owner. A 4xx/5xx is an ordinary `Reply`; only transport failures
are `Failure::Transport` (`timeout`, `connect`, `redirect`, `body_limit`,
`sink`, `transport`), recorded before they return. Buffered replies stop at
`max_body_bytes` (10 MiB by default); `stream` writes chunk by chunk with the
same bound and leaves cleanup of a partial target to the caller. The whole-call
timeout defaults to 30 s. `Failure::Invalid` (bad metadata, URL, non-HTTP
scheme or header) sends and records nothing. `Failure::Unrecorded` means the
remote system may have acted but the row could not be written: never treat it
as permission to repeat a consequential command. HTTPS trusts the public roots
only. `Outgoing::trust_system_roots()` also trusts the server's own store
(`/etc/ssl/certs`, where `update-ca-certificates` puts a private authority), for
a call to an address the operator chose; the release transport uses it so an
instance on a private name is reachable. Redirects are followed unless
`Outgoing::no_redirects()` is set, which returns a 3xx answer as the reply so a
credential header is never sent on to another address. When the next host is
known and must not see the credential, use `Outgoing::redirects` with a
`RedirectPolicy` instead of a private client: only the listed HTTPS hosts are
contacted, private and local addresses are refused, credential headers are
removed when the host changes, and the exchange row keeps the original URL.
The `Location` target, including a signed download query, is not stored.

For URLs supplied by an untrusted caller, use `Outgoing::public_https()`. It accepts HTTPS only, resolves the host once, refuses non-public addresses, pins the accepted addresses for the TLS connection and disables proxies and redirects. Public certificate roots still verify the original host. Set a short timeout and body limit for metadata documents; never fetch an untrusted URL with the ordinary redirecting client.

## Incoming calls

```rust
integration.record_incoming(&scope, &Incoming {
    method: Some("PATCH".into()), endpoint: Some("/api/companies/17".into()),
    operation: Some("company.update".into()), response_status: Some(403), duration_ms: Some(12),
    connection_id: Some(connection), agent_id: Some(agent), run_id: Some(run), ..Incoming::default()
}).await?;
// A credential that resolved no tenant (refused, unknown, expired):
integration.record_incoming_system(&Incoming { method: Some("POST".into()),
    endpoint: Some("/api/v1/operations".into()), response_status: Some(401), ..Incoming::default() }).await?;
```

Call once after dispatch, including refused and failed requests. The tenant and
user come only from the sealed scope; a system row asserts neither, and only the
platform operator can see it. Connection, agent and run ids are observations,
never authority. `agent_self_report` is the agent's shape-checked
`harness/model-version-effort` (`bilibi_tenancy::SelfReport`), and
`agent_self_report_malformed` records that the caller sent one that failed the
check and was dropped; never set both. The exchange list and detail show it as
"Model (as reported)" (or "Malformed, dropped"). An omitted method or duration stays unknown; no status and no
error class records `unknown`; an operation key stands in for the endpoint when
there is no HTTP path. Out-of-range status, negative duration or over-long
fields refuse with `Error::Invalid`.

## Privacy

Every stored endpoint loses URL credentials, the query string and the
fragment; do not put secrets in path segments or observational ids. Raw error
messages are never stored (`error_message` is generic). Bodies and headers are
kept only with `retain_payload: true`, as `{"kind":"json","value":…}` previews:
keys naming authorization, cookie, password, secret, token, api key, credential
or signature are `[redacted]`, every string value is `[omitted]`, numbers,
booleans and structure stay. Non-JSON or streamed bodies, and previews over
32 KiB, record only `{"kind":"omitted","reason":…}`. `payload` redacts again
on read (`redaction::inspect_stored`).

## Reads, deletion and retention

`list`, `get`, `payload` and `delete` take the session scope and authorize on
every call: `admin.system.outbound-exchange.list`, `.payload.view` (with list)
and `.delete`. These are tenant capabilities; Auditor and System Viewer get
list only. A tenant sees its own rows; only the platform operator scope widens
to all tenants and system rows, and it never bypasses the capability. `list`
takes a `Filter` (direction, outcome, one search across host/path/operation,
status, user and agent ids, exact status, actor, UTC `since` and exclusive
`until`) and page sizes 25/50/100/300. Deletion is an audited mutation
(`base/integration.exchange`) carrying metadata only.

Recording and pruning are machine measurements on their own pooled connection,
outside the caller's transaction and outside mutation capture. The global
operator setting `integration.payload_retention_days` (default 30) drives the
contributed daily schedule `base/integration-payload-retention` (03:17 UTC,
forbid overlap), which clears previews and keeps metadata. Like every
contributed recurrence it runs nothing until an operator enables it on
`/system/schedule`.

## Runtime evidence

`examples/integration-walkthrough.rs` runs against a disposable database
(`BILIBI_DISPOSABLE_INTEGRATION_DEMO=yes`, `DATABASE_URL`) with a local fixture
server: retained/redacted preview, HTTP error, body limit, streamed download,
connect failure, timeout, tenant and system incoming rows, every visibility and
capability gate, audited deletion and payload pruning.

## Inbound webhooks

`webhooks.rs` is the contract between a module and the host's
`/webhooks/{identifier}` boundary (`apps/web/src/webhooks.rs`). Contribute
`Webhook { identifier, service }` to `WEBHOOKS`; the service is a
`WebhookService(Arc<dyn WebhookHandler>)`. `verify` authenticates the raw
delivery (exact bytes, all headers, attributed sender) and returns an opaque
proof; `handle` acts on it idempotently. Both answer `Refused` without a reason:
the host exposes one fixed refusal and audits only its own codes. Handlers must
not panic and never see the response.

`reference_webhook.rs` is the specimen at `/webhooks/integration-reference`:
HMAC-SHA256 over `"{timestamp}.{nonce}." + body` in `X-Bilibi-Signature`
(lowercase hex), `X-Bilibi-Timestamp` within ±300 s and a single-use
`X-Bilibi-Nonce` claimed atomically in `base_integration_reference_webhook`, so
a replayed delivery is refused while a delivery the host rate-limited after
verification can be retried unchanged. Its secret is
`BILIBI_REFERENCE_WEBHOOK_SECRET` at boot, because a webhook has no actor and
Settings releases encrypted values only to an authorized scope; unset, it
refuses everything. Production owners resolve credentials from their own
definitions. The host reads the `webhooks.*` limits contributed here.

## API Exchanges pages

`/system/api-exchanges` and `/system/api-exchanges/{id}` (`src/web.rs`,
`templates/`, service `base/integration.pages`, menu item under System) are
Bilimbi's `IndexLive`/`ShowLive`. The list runs the shared flexible table
over the built-in columns, direction tabs, outcome select, one search, page
sizes 25/50/100/300 and the shared period filter; calendar days follow the
actor's display mode and, in local mode, the zone the `BrowserTimeZone` hook
reports, with an unresolvable range listing nothing and saying why. The
operator scope sees every tenant and the system rows under the same caution
as Bilimbi. The column arrangement uses Grid `PageColumns` and is remembered per account. Scalar metadata shares the detail schema through `with_record_slots`; Endpoint, Outcome, Direction, Owner and the remaining facts are optional columns. Retained payloads are structured content and stay on the detail page. The detail page inspects the retained payload only on an
explicit click behind `.payload.view`, deletes behind a confirm dialog and
`.delete`, and returns to the list afterwards; a refusal is a flash. To see
rows, run the walkthrough above against the app's database.

`client_identity` records the edge-observed client label separately from the agent self-report. The host bounds it to 255 printable ASCII characters; Exchanges shows it as "Client identity". It is transport evidence, never authority.
