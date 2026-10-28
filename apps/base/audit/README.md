# Audit evidence contract

`base/audit` records durable mutation and action facts in `base_audit_mutations`,
`base_audit_actions`, the mutable bookmark table `base_audit_action_metadata`
and the diagnostics table `base_audit_capture_failures` (migration
`006-audit-evidence`). Rows outlive actors and subjects: no foreign keys to
tenants, accounts or records. PostgreSQL refuses UPDATE, DELETE and TRUNCATE on
evidence, stamps `recorded_at` from `clock_timestamp()` and fills a missing
`occurred_at`. The frozen legacy ledgers `audit_mutations` and
`bilibi_action_commits` were carried over once as `source = 'legacy'` rows and
receive nothing further.

## Capturing business writes

Do not insert audit rows from an owner. Write through
`bilibi_database::MutationBoundary` (`base/database.mutations`) and return every
affected record as a `Change`; `AuditCapture` is bound to that boundary during
service installation and runs inside the owner's transaction. A refused audit
row, an unrecognised principal or a failing commit-time action row refuses the
business commit: there is no best-effort mode. Boot fails when nothing is bound.
Before refusing, the capture writes a diagnostics row on another connection with
identity and error type only, never values or messages.

A `Change` becomes `created`, `updated` or `deleted` from its old/new objects; a
change with two empty objects writes nothing. Bulk writes arrive as one slice
through `Capture::mutations` and are inserted in chunks of 1,000 rows per
statement. Values are stored in full, with NUL stored as `␀`. Redaction is by
field name on every captured record (`GLOBAL_REDACTIONS`: `password`,
`password_hash`, `remember_token`, `secret`, `api_key`, `token`, `payload`),
plus per-record-kind fields an owner contributes with
`Contribution::new(REDACTIONS, ..., &Redaction { kind, fields })`. The change is
recorded; the value appears as `[redacted]`. Widen the policy rather than masking
at a call site, and name non-secret fields differently.

## Attribution

`Attribution::from_context` maps the F2 `Context` principal kinds `user`,
`agent`, `guest`, `console`, `scheduler`, `queue`, `system` and `unknown`.
`actor_id` names the account acted as; `impersonator_id` names the operator
behind an impersonated session and must be a user; a `system` principal with a
non-zero id becomes `system/0` plus `system_principal`. Any other kind, or a
non-numeric user/agent id, is refused. `Attribution::from_scope` is the sealed
session identity, operator included under impersonation (K14); Q2 supplies job
rehydration contexts.
`agent_self_report` (migration `base-audit-007-agent-self-report`, on mutations
and actions) is the agent's self-reported `harness/model-version-effort`,
taken from `Context::agent_self_report` or the scope's agent facet. It is
recorded beside the actor, never replaces it, and is shown as "Model (as reported)" on the Actions and Data Mutations lists and in record history.
The committed action payload records `Context::agent_connection` when an agent acts; its shared trace links every nested mutation to that durable grant. The actor remains the person.
The committed action row carries `event = Context::action`, so name actions as
`owner.operation` (`company.update`), the way Bilimbi names explicit actions.

## Explicit actions and the console

`record_action(client, &attribution, request, event, payload, retained)` records
attempted, refused and failed operations on the caller's own connection, outside
any rolled-back transaction. `RequestInfo` carries IP, URL and a user agent cut
to 80 characters; free text in payloads goes through `bounded_text` (2,000
characters with a marker).

`console::execute_readonly(db, &scope, request, info)` is the read-only SQL
console engine: operator proof, empty/statement/keyword text guards, then a
PostgreSQL `READ ONLY` transaction with a 10 s statement timeout, count, column
names and at most 1,000 rows per page. Every command is one
`database_query.executed|refused|failed` action row with the SQL as typed and the
outcome; result rows never reach the record, and a command whose record cannot
be written returns no answer. Database error details are returned to the operator
but the recorded failure/refusal message is generic: PostgreSQL messages may
quote parameter values or data raised by a function.

`execute_readonly_with_options` also accepts named parameters and output sorting.
Its lexical binder skips strings, quoted identifiers, dollar quotes, casts and
nested comments; values use PostgreSQL's text bind format for the inferred type,
never string substitution. A missing value binds NULL. The transaction uses a
repeatable-read snapshot and is explicitly rolled back even on SQL error.
Arbitrary-precision JSON numbers preserve PostgreSQL numeric values without
floating-point rounding. Unique positional aliases preserve duplicate output
names in JSON conversion; paging arithmetic is checked. Connection/setup errors also go through the recorder on
a fresh pool checkout. `record_outcome` allows the owning service to record
capability/actor refusals through this same rows-free payload builder.

C9's `core/user.database_queries` service owns saved queries and the live
capability/account checks; C10 owns the screen. Both reuse this executor rather
than running editor SQL themselves.

`src/read.rs` holds the bounded tenant reads the Audit screens use:
`list_actions` (search, actor type, event family, result, diagnostics, sort,
bounded page), `toggle_retained` (the mutable bookmark; the recorded bit never
changes), `recent_mutations`, `list_mutations` (search, event, sort, bounded
page) and `subject_history` (one record's latest mutations under any of its
kinds). `MutationEntry::withhold` takes a reader's withheld fields out of an
entry and names them instead. They never ask who may read: this crate sits
below Authz, so the screens in `base/audit_views` own the capability vocabulary,
the checks and which fields are withheld.

## Personal-data erasure

**One narrow exception to append-only evidence, opened only by a sealed `base/personal_data` grant** ([ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md), migration `base-audit-009-erasure`).

- `erase_record(tx, &Erasure, kind, id, person, &Minimums)` clears the personal fields that the kind's owner declared (`RECORD_KINDS`) in every mutation row of that record, and the subject identifier only when declared. An undeclared kind is `Invalid`; the caller never names fields.
- **The database holds the same authority:** the fields must be declared in `base_personal_data_kinds` (written by the kind's owning module's migration; `Internal` when code and row disagree). The cause must be confirmed, for the person named, by the checker its owner registered in `base_audit_erasure_causes` (`base/privacy`: the request's own person while it runs or still owes cleanup, or a sweep still running). The target must be a record the calling transaction itself changed, or a due deferral the sweep is finishing. Anything else is refused, even when called directly as the owner role, so a request never authorises erasure of another person's or an untouched record.
- Declarations and checkers change only in a transaction that also records a migration in the ledger (checked at commit), never by the running application.
- `Minimums` are legal minimums per company: a row of a listed company stays while newer than that company's days; a row of no company follows the strictest. Kept rows get one open `base_audit_erasure_deferrals` row naming the person; the erasure's own rows (its trace) are always cleared.
- `due_evidence(client, tenant, after, limit)` lists due deferrals; the caller leaves out held records and persons under the hold guard and passes the rest to `erase_deferred(tx, &Erasure, ids, &Minimums)` (sweep grants only), which erases them with today's minimums.
- Each call writes one append-only `base_audit_erasures` row in the same statement: request or sweep, kind, ID, fields, rows erased and kept, actor.
- No session setting opens the path: the guard admits a change only while `base_audit_erase_values` runs as the evidence's owner (call stack and `current_user`), and `EXECUTE` is revoked from `PUBLIC`. Every other update, delete and truncate is refused.
- Audit's own store, `base/audit.history`, exports the evidence history of the subject's person record.

## Hash chain

**Every mutation and action row is linked to the one before it over its ID, tenant, time, actor, event, record and field names; values are excluded, so approved erasure keeps the chain whole.**

- **Scope of the guarantee:** a removed row, or a changed ID, tenant, time, actor, event, record or field name, is detected. A changed value is not: values are outside the hash so that erasure can clear them, and only the evidence guard (no update outside the erasure functions) protects them.
- A deferred constraint trigger links each row as its transaction commits, in commit order (advisory lock 841332), so no committed row is ever unlinked; rows from before migration 009 are linked by it.
- `verify(client)` recomputes both chains and names the first `missing`, `changed` or `tail` break, or `unsealed` when a committed row has no link (its trigger was bypassed).
- `base/audit_views` offers **Verify audit log** at `/audit/integrity` to platform operators; every check is recorded as `audit.chain.verified`.

## Runtime evidence

Demonstrated without unit tests on a disposable database upgraded from the
pre-P5 schema; see Firstmate task data `data/bilibi-p5/` (reset script, proof
fixture, logs): legacy carry-over, service save with shared trace, redaction and
NUL handling, 2,500-row bulk capture, system/impersonator/queue attribution,
refused unknown kinds, refused mutation and action rows leaving company and
notification state unchanged with diagnostics written, append-only evidence, every
console outcome recorded, and an HTTP/browser smoke of the running app.

`client_identity` (migration `base-audit-008-client-identity`) records the transport client separately on actions and mutations, via the sealed scope or trusted `Context`. It is shown as "Client identity", never authorization or a replacement for "Model (as reported)".
