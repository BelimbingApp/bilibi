# Agent operation boundary

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

The host passes a fresh sealed `tenancy::Scope` authenticated through the live
account/session or credential owner. E1 deliberately uses the existing proven
account actor; K9 supplies Person/Organisation facets. The production OAuth/HTTP
adapters are the host's `/api/v1` edge (`apps/web/src/api.rs`) and `/mcp`
(`apps/web/src/mcp.rs`), the latter speaking the stateless protocol adapter in
[src/mcp.rs](src/mcp.rs): six generic tools over this same registry and plan
service, with the host supplying a per-request `Session`. System scopes cannot
enter this API. Never deserialize a scope, infer an actor from operation input,
or cache reach across requests.

Search uses one fresh capability snapshot without per-candidate decision logs.
Direct deny wins over grants and `grant_all`; platform capabilities additionally
need explicit operator identity. An agent reaches every capability its person
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

An owner's commit may answer `Error::StepUpRequired` (Base Step-up). The whole commit rolls back, and the plan code stays usable until it expires. The edges answer `step_up_required` with the operation and reason. The client asks its human for a current authenticator code and commits the same plan again with `step_up_code`. That code is used once, for that operation, and opens the agent connection's window (`apps/base/step_up/README.md`).

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
