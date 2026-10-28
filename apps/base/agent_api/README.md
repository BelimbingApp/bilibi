# Operation contracts

Every action that reads or changes business data is one operation contract here, and it does not know who calls it (ADR 01, `docs/architecture/decisions/01-agent-first-operations.md`). `/api/v1` and `/mcp` call contracts for agents; pages call them through [Pages as callers](#pages-as-callers). The module keeps its historical name.

Use the `base/agent_api.registry` service for search, describe, guide and read
dispatch. Definitions are discovered through the typed `Definitions` consumer
in [src/lib.rs](src/lib.rs); module owners contribute their operations and guides
with the registry's `Contribution` and source `Provenance`. No central host list
or module-specific endpoint is needed. [src/reference.rs](src/reference.rs) is a
complete non-business contribution example.

Keys start with the contributing module ID with `/` changed to `.`. Operations
need an additional segment; guides may use the module prefix itself. Keys must
be unique across operations and guides. Discovery rejects malformed metadata,
foreign handler ownership, unsupported schema keywords, undeclared capabilities,
read operations on write capabilities, and impersonation capabilities (`*.impersonate`).
Titles have a 120-character limit, summaries 500, guide bodies 8 KiB. Registry
entries retain source provenance and materialized schemas.

Two more contract properties, both checked at boot:

| Field | Meaning |
|---|---|
| `channels` | `Channels::Every`, or `Channels::Only { channels, reason }` for an action that cannot work through some door (it needs a browser session, say). Never a way to keep a sensitive action from agents: that is the capability and step-up. A door the contract does not list sees it as unavailable (`channel_not_allowed`) and is refused on describe, call and plan |
| `step_up` | The Base Step-up operations (`bilibi_step_up::declare!`) its commit may ask for. Each must be declared; a read cannot list one. The owner's write still calls `StepUp::require`; listing the keys here tells every caller a code may be needed. `describe` shows both |

The host passes a fresh sealed `tenancy::Scope` authenticated through the live
account/session or credential owner. E1 deliberately uses the existing proven
account actor; K9 supplies Person/Organisation facets. The production OAuth/HTTP
adapters are the host's `/api/v1` edge (`apps/web/src/api.rs`) and `/mcp`
(`apps/web/src/mcp.rs`), the latter speaking the stateless protocol adapter in
[src/mcp.rs](src/mcp.rs): six generic tools over this same registry and plan
service, with the host supplying a per-request `Session`. System scopes cannot
enter this API. Never deserialize a scope, infer an actor from operation input,
or cache reach across requests.

**Reads hold no dispatcher connection.** `Registry::call(db, scope, key, input)` checks the capability on a pooled connection and returns it before the read handler runs. The handler's `OperationClient` takes one connection on its first query and keeps it until the call ends; a handler that only calls its facade (Base Search, Base Records) takes none. So a facade that takes connections of its own, such as Grid's reads, never waits on a pool filled by its own callers. MCP's `Session` carries the pool (`db`), and `search`, `describe` and `guide` each take a connection for that call only.

Search uses one fresh capability snapshot without per-candidate decision logs.
Direct deny wins over grants and `grant_all`; platform capabilities additionally
need explicit operator identity. Tenancy reproofs read the actor and agent grant live; owner standing additionally re-reads the installation bootstrap receipt and refuses impersonation. An agent holds only its own person's standing, and an owner-only write still requires its declared step-up policy. An agent reaches every capability its person
holds, administration included, and never more. The one exception is
impersonation (`*.impersonate`): an agent switching to another identity would
break "the agent acts as its person" in the audit trail, so it is never agent
reach and no agent operation may declare it. Sensitive writes ask the person
for a code through Base Step-up instead. Search defaults
to ten results, clamps limits to 1–50, supports a read/write filter and optional
unavailable entries. Rankings follow Bilimbi's token stemming, field weights and
key tie breaker. Describe and guide reads also check fresh reach.

Read dispatch validates the declared input, caps `page_size` at 100, asks normal
logged Authz authorization, then calls the owner handler. Owners continue to
call their scoped facade and enforce field restrictions. All direct writes are
refused. Unsupported writes remain discoverable. Supported `WriteHandler`
operations use the plan service below, sharing its audited transaction.

The [schema dialect](src/schema.rs) accepts object/properties/required,
string/enum/maxLength/format, integer/number/enum/minimum/maximum, boolean and
array/items, plus title/description annotations. Every node needs a type and
root input must be an object. Input objects refuse unknown fields. Validation
returns dotted field paths, including array indexes. Null is not implicit.
String formats are date, date-time with offset, and email. Nesting is bounded
at discovery. Output schemas describe data without runtime enforcement, as in
Bilimbi.

Use [json::Data](src/json.rs) for results and its `Restricted` variant for a
withheld field. The marker stores no original value and serializes only as
`{"restricted":true}`. `Data::restricted` replaces restricted field values
before they enter an output. Owners supply typed dates/times as ISO strings and
exact decimals as strings; Rust's JSON value type excludes functions and opaque
runtime handles. Pages carry page/page_size/total metadata. Every envelope has
the untrusted-content notice.

For a runtime walkthrough, migrate an isolated database, then run:

```sh
BILIBI_AGENT_DEMO=disposable cargo run -p bilibi-agent-api --example agent-walkthrough --locked
```

Set `DATABASE_URL` to that disposable database. This runs real `bilibi_web::App`
services and authenticates walkthrough fixture credentials. It prints search, schemas, guide,
restricted results, malformed input, unavailable entries, direct-write refusal,
live grant/revocation, cross-tenant denial, deny precedence and decision logs.
Temporary grants and sessions are removed on successful completion. This is a
manual runtime specimen, with no unit tests or assertions.

## MCP agent and client labels

A model can report itself directly through any generic tool, with no local client configuration:

```json
{"name":"search","arguments":{"query":"companies","agent_self_report":"claude-code/sonnet-4.5-default"}}
```

The optional `agent_self_report` argument is available on `search`, `describe`, `guide`, `read`, `plan` and `commit`. MCP connect instructions and tool descriptions ask agents to include it on every call, ask their user for unknown harness/model/version/effort parts, never guess, and omit parts nobody can give. The example is a shape example, not a value to copy.

- The host consumes the argument before operation dispatch; it is never business input or part of plan identity, input or preview comparisons.
- The argument is the highest-precedence source of the report. Precedence, the shape check, what is recorded and the separate **Client identity** transport evidence are owned by the [host contract](../../web/README.md).

## Reviewed safe writes

Use `base/agent_api.plans` ([src/plans.rs](src/plans.rs)) for `preview` and
`commit`. Supply a trusted `PlanAuthority` adapter owned by the credential
service. It must authenticate on the supplied transaction, reprove account and
party eligibility, and lock the credential/eligibility records so revocation
cannot race execution. `PlanIdentity.credential_binding` identifies the OAuth
connection, or the browser session for a session adapter; include impersonation
identity and session when supported. Never populate it from operation inputs or
cache an authenticated Scope across requests. The host's `Grant` adapter
(`apps/web/src/api.rs`) is the production OAuth authority; the walkthrough
supplies a real session adapter. The credential is
rechecked before owner execution and again before consuming the code.

`PlannedOperation` refuses unknown wire keys. Plans accept 1–20 ordered key/input
objects by default, at most 1 MiB encoded. Global settings
`agent_api.plan_max_operations` (1–100) and `agent_api.plan_expiry_seconds`
(60–3600, default 600) control bounds. Preview validates every schema and normal
Authz capability and calls owner previews. Invalid reviews return `valid:false`
with indexed errors and no code. Valid reviews return ordered operations,
restricted review results, expiry and a random code. Clients obtain the person's
consent before commit; the code proves the reviewed contract, not consent.
Never log the code or credential binding.

Storage retains only SHA-256 hashes of the code, actor/party/credential binding,
canonical ordered inputs, and review/definition facts. Canonicalization sorts
object keys recursively, preserving array order and JSON types. No inputs,
secrets or review values are stored in the plan table or action payload. Two
UUIDv4 values supply 244 unpredictable bits for the 64-character code. Native
PostgreSQL `sha256(bytea)` provides hashing without extensions or new crypto
dependencies. Preview prunes that tenant's plans expired more than a day ago.

Commit locks the tenant-scoped code row, refuses identity/input mismatch, replay
and wall-clock expiry, then rechecks schemas, capabilities and owner previews.
Changed review facts or definition/schema metadata require a new plan. It calls
each owner on the same `MutationTransaction`, rechecking capability before each
write. Every owner error, capture error, expiry or final identity failure rolls
back all writes, audit rows and code consumption. A competing submission waits
for the row lock then sees the consumed code. Failed commits leave the code
usable until expiry, subject to a fresh successful revalidation.

### Which Bilibi a plan lands on

Every preview and every successful commit carries `instance` (`bilibi_system::Instance`, `apps/base/system/README.md`), so the agent can tell its person "this will create the order on Staging":

```json
"instance": {"environment": "staging", "role": "Staging", "origin": "https://staging.example.com", "label": "Staging", "production": false, "notice": "You are on Staging (Staging), at https://staging.example.com. This is a test copy. Changes here do not affect production."}
```

- `environment` is `development`, `staging` or `production` (the saved `instance.role` setting); `role` is its display word (Dev, Staging or Production); `origin` is the canonical `APP_ORIGIN`; `label` is the operator's `instance.label` setting, which defaults to the role word.
- MCP names it too: `initialize` (`serverInfo.title` and the instructions) and the `plan` and `commit` tool descriptions in `tools/list`. `GET /.well-known/bilibi` carries the same object.
- The plugin skill (`core/agent_plugins`) tells the agent to open every review and confirmation with it, as a full-width banner in an HTML review. Bilibi draws no plan review page itself; the harness does.

An owner's commit may answer `Error::StepUpRequired` (Base Step-up). The whole commit rolls back, and the plan code stays usable until it expires. The edges answer `step_up_required` with the operation and reason. The client asks its human for a current 2FA code and commits the same plan again with `step_up_code`. That code is used once, for that operation, and opens the agent connection's window (`apps/base/step_up/README.md`).

Owners implement deterministic `WriteHandler::preview` without business writes,
including defaults, versions, reference facts and restricted fields in the
review. `commit` must lock and revalidate its owner facts (including concurrent
first inserts), enforce field policy, and use `tx.mutate`/`tx.record` for all
writes. The plan's preview comparison cannot replace owner concurrency checks.
Never open a separate owner transaction or publish before the outer commit.
The non-business reference write demonstrates version fencing and audited
mutation. There is no product UI; the HTTP endpoints are the host's `/api/v1`
edge (`apps/web/README.md`).

Run the real safe-write walkthrough after migrating an isolated DB:

```sh
BILIBI_AGENT_DEMO=disposable cargo run -p bilibi-agent-api --example plan-walkthrough --locked
```

It prints code-free reviews and outcomes for input/order/identity mismatch,
canonical object ordering, direct write refusal, single-use replay, concurrent
submissions, a second-operation failure, capability revocation, expiry, stale
owner facts/review digest, audit failure and recovery, and session revocation.
Bounded SQL output shows reference state, audit counts, consumption and hash
lengths. Clock/review drift and audit refusal fixtures alter only the explicitly
disposable database. This is a manual app-service specimen with no test runner,
mocked identities or assertions. Sessions and temporary grants are removed on
successful completion; reference/audit evidence remains for inspection.

## MCP Apps: a module's card in the chat

One answer: a module that wants a small page inside a chat host registers one `App` under `APPS`; `/mcp` does the rest, and the card itself works through the generic tools.

An MCP App (specification 2026-01-26, which ChatGPT implements) is an HTML document the host shows in a sandboxed frame when a tool points at it. In Bilibi:

| Part | Where | What it is |
| --- | --- | --- |
| `App` contribution | the owning module, `Contribution::new(APPS, …)` | `uri` (`ui://bilibi/<name>-v1.html`), `tool` (`open_software`), `title`, `description`, `operation` (a read operation of the same module whose input schema is the tool's and whose answer is the card's first data), `html` (the whole document, self-contained), `connect_domains` and `resource_domains` (both empty for a card that reaches nothing but the host), `skill` (how an agent uses it, for the plugin package) |
| Boot check | `Registry::validate_apps`, `from_catalog` | the tool name is lower-case with underscores and not a generic tool; the URI is unique and `ui://`; the operation exists, is a read and belongs to the registering module |
| `initialize` | `src/mcp.rs` | declares the `resources` capability beside `tools` |
| `tools/list` | `src/mcp.rs` | one tool per app after the six generic ones, with `_meta.ui.resourceUri` and the older `openai/outputTemplate` alias, `readOnlyHint`, and the instance in its description |
| `tools/call` of an app tool | `src/mcp.rs` | `Registry::call` of the app's operation: schema, channel and capability checks as for `read` |
| `resources/list`, `resources/read` | `src/mcp.rs`, public like `tools/list` | the card as `text/html;profile=mcp-app` with `_meta.ui.csp` (and the `openai/widgetCSP` alias) from the declared domains; an unknown URI is JSON-RPC `-32002` |

The card calls `read`, `plan` and `commit` through the host's `tools/call` bridge, so it adds no permission path: a person who cannot reach an operation is refused inside the card exactly as an agent is. Every tool carries `openai/widgetAccessible: true` for that reason. The card tells the model what happened with `ui/update-model-context` and can post into the chat with `ui/message`; the host decides whether to pass either on. A card never frames a Bilibi page (sign-in cookies do not travel into the host's frame) and never loads a script or style from elsewhere.

The first card is `base/software` (`apps/base/software/card/software-v1.html`, tool `open_software`, operation `base.software.open`). The plugin package (`core/agent_plugins`) lists each app tool in the workflow skill and writes the app's `skill` as a skill of its own.

## Pages as callers

A page's write event calls the contract the agent doors call, through `base/agent_api.pages` ([src/page.rs](src/page.rs), `PageContracts`). Same validation, capability check, owner preview, audited transaction, version fence and step-up; the page keeps its rendering and live patching.

| Call | Use for | Path |
|---|---|---|
| `submit(call, input)` | A form the person just reviewed (inline edit, create, a dialog that collects input) | `Plans::submit`: the owner's preview, then its commit, in one transaction; no code |
| `preview(call, input)` then `commit(call, code, input)` | A confirmation dialog (Core Person's retire) | The plan service with a code bound to the browser session; keep the code in the mounted state |
| `read(scope, key, input)` | A direct read | `Registry::call` |

- `Call` names the live request's scope and credential (`LiveRequest::credential`), the page service, the event and the contract. The browser session is the plan's credential: proved and held (key-share lock) on the plan's own transaction, so a sign-out cannot race the write.
- Owner refusals come back typed: `InvalidInput(fields)`, `Forbidden`, `NotFound`, `Backend(Conflict)`, … Return `Backend(Unauthorized)` and `Backend(StepUpRequired { .. })` from the page unchanged: the host signs the browser in again or sends it to confirm and back.
- Core Person, Core Affiliation, Core Company's relationship screens, Core Tenant Administration and Base Events' replay page are the running examples.

## Page writes declaration and check

Each page or panel service declares its write events once, with Module Registry's `PageWrites` contribution (`PAGE_WRITES`, keyed by the service id):

```rust
inventory::submit! { Contribution::new(PAGE_WRITES, SERVICE, provenance!(), &PageWrites {
    service: SERVICE,
    contracts: &[("create", "core.person.create"), ("save_field", "core.person.update")],
    pending: Pending::None,
}) }
```

- **Boot.** Module Registry refuses a malformed declaration (a service of another module, an event named twice, an allowance without a reason). `PageContracts` refuses a contract that is not a registered write, and a service no route or embed serves.
- **Every live event.** The host dispatches a page or panel event under the permit its declaration gives (`bilibi_database::page_write`). `MutationBoundary::begin` then refuses a business transaction the permit does not cover: only the event's contracts (marked by `PageContracts`), or anything for an event listed as pending. An undeclared service or event may write nothing. The refusal is `Error::Internal`, logged with the service, event and action.
- **The call.** `PageContracts` refuses a contract the page did not declare for that event.
- A person's own view state (list columns, dashboard arrangement, theme, pins) is not business data: its owner opens `MutationBoundary::begin_view_state`, which the check leaves alone.
- Writes made outside a live page event (agents, jobs, seeds, HTTP form posts) are not affected.

### Allowance: pages not yet migrated

Each gap is a `Pending` declaration in the owning module, with its reason. Remove it when the event calls its contract. `grep -rn "Pending::Every\|Pending::Events" apps` lists them.

| Service | Why it is pending |
|---|---|
| `base/audit_views.pages` | Action-log export writes through the Audit Views facade |
| `base/authz.pages` | Roles, field restrictions and two-factor policy; `base/authz_agent`'s grant contracts serve agents only |
| `base/integration.pages` | Exchange actions |
| `base/schedule.pages` | The schedule board |
| `base/session.pages` | Ending sessions |
| `base/settings_editor.pages` | Saving and restoring settings |
| `base/system.pages` | Localization changes |
| `base/tiling.pages` | Workspace and shared layouts |
| `base/workflow.pages` | The workflow reference page |
| `core/agent_access.pages` | Device approval and connection revocation |
| `core/company.pages` | Company edits, lifecycle, activities, departments, reference types; the create form records activities `core.company.create` does not take |
| `core/geonames.pages` | Postcode corrections |
| `core/user.pages`, `core/user.query-pages` | Accounts, passwords, two-factor, notifications, saved queries |
| `core/user_administration.pages` | Account administration |

## Agent connection context

MCP initialization and generated plugin workflow skills use `BUSINESS_CONTEXT` from [src/lib.rs](src/lib.rs): Bilibi is the person's business system, and agents search its operations and guides before answering or claiming a field or action is missing. `INSTANCE_INSTRUCTIONS` tells agents to announce the label, role and canonical address in their first answer and every plan review and confirmation. MCP `serverInfo.title` names all three; plan and commit results carry them in `instance` through `bilibi_system::Instance::agent_json`.

## Tenant provisioning

[Core Tenant Administration](../../core/tenant_administration/README.md) registers `core.tenant_administration.tenant.list`, `.get` and `.create`. The Tenants page uses `PageContracts` for its directory reads and create event, with no pending write allowance. Search and describe discover these contracts through the same registry as the ChatGPT plugin; the plugin contains no tenant-specific logic.

The create input names the tenant, a `primary_company` and optional `additional_companies` names. One reviewed commit creates the tenant, companies and primary assignment on the plan transaction. Tenant-create permission requires the platform operator's standing and capability; its declared step-up policy applies to both doors. Company provisioning takes Tenancy's sealed receipt for a fresh insert, never an input tenant id or a retargeted scope. Creating a tenant does not switch the caller into it or grant access to its business data.
