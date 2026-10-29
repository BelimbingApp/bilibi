# ADR 05: Personal-data registry, data-subject requests and audit erasure

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** Personal-data retention, legal hold, access and erasure requests, and field masking across every mounted module
**Last Updated:** 2026-10-24

> Source: [issue 132](https://github.com/BelimbingApp/bilibi/issues/132). Bilimbi has no privacy module to mirror; its per-store retention workers (`base/integration`, `base/perf`, `base/schedule`) are prior art for sweeps only. Belimbing's [company erasure refusal](https://github.com/BelimbingApp/belimbing/blob/main/app/Core/Company/Exceptions/CompanyErasureException.php) is prior art for keeping history rows when erasing.

## Context

**A tenant must answer access and erasure requests (PDPA 2010, GDPR) across every mounted module, and today nothing can find everything about one person.**

- `base/artifacts` has per-document retention and legal hold, but nothing asks it for one person's documents.
- `base/authz` field restrictions remove a field for restricted roles. There is no partial mask and no audited reveal.
- `base/audit` keeps full old and new values in append-only evidence. A person's email stays there after the record changes.
- `base/media` submitted attachments are immutable (`ImmutableAttachment`), so a person's file cannot be removed.
- `core/person` retire and `core/user` account deletion keep personal fields; nothing anonymises.
- Retention exists only for platform logs (integration payloads, authz decision log, perf, schedule history).

Without a shared contract, each Domain (a CRM first) would build its own export, erasure and retention, and no request could prove it reached every store.

## Decision

**A new `base/privacy` module owns one registry; each module answers for its own data through a contributed handler.**

| Layer | Where | Responsibility |
| --- | --- | --- |
| Personal-data registry | `base/privacy` | `PersonalData` contribution per store, with a handler that can `locate`, `export` and `erase` |
| Personal field marks | `base/grid` | A Grid `Field` can be marked personal with a kind |
| Requests | `base/privacy` on `base/workflow` and `base/queue` | Access, portability and erasure requests through one contributed flow |
| Export and certificate | `base/artifacts` | Expiring export bundle; retained completion certificate |
| Legal hold | `base/privacy` | Holds on a person or one record, append-only history |
| Retention | `base/privacy`, `base/settings`, `base/schedule` | Contributed record classes, company periods and legal minimums, sweeps |
| Masking | `base/authz`, `base/grid` | Partial-value restriction mode with audited reveal |
| Audit erasure | `base/audit` | Guarded replacement of erased values, with a hash chain and verification |
| File erasure | `base/media` | Owner-authorised erasure of a submitted attachment, leaving a tombstone |

### Anonymise by default

**Erasure keeps rows, IDs, keys, links and amounts; only the person's declared personal fields become `[erased]`.** Orders and cases keep their integrity and show an anonymised stub. A handler may delete only records with no business value, subject to legal minimums and holds. Uploaded file bytes are deleted, with a tombstone row kept.

### Handler contract

**Every handler takes a sealed `Scope` and a `Subject`, and Base needs no Core dependency.**

- `Subject` is a `bilibi_party_ids::PersonId` plus listed identifiers (email, phone). It never carries free text from the requester.
- `locate` returns counts per record kind, split into matches by Person and matches only by identifier.
- `export` returns JSON plus file references (media and artifact IDs). Base resolves the files; the handler never copies bytes.
- `erase` anonymises in place, or deletes records with no business value, and returns counts per record kind plus records kept, each with a reason (`legal_hold`, `retained_until`, `legal_minimum`, `deferred`) and a date when known.
- Every call is idempotent: a retried job that finds work already done returns the same counts.
- `erase` writes through `MutationBoundary` like any other owner write. Its `Change` names the erased fields with the value `[erased]`.

### Personal field marks

**A Grid field declares it holds personal data; boot refuses a table that declares one without a handler.**

- Kinds: `name`, `email`, `phone`, `identifier`, `date`, `free_text`. The kind drives the mask shape and the audit erasure field list.
- Validation is at discovery: a table with a personal field in a module that contributes no `PersonalData` handler fails boot, naming the table.
- Marks are opt-in. A module with no marks behaves exactly as before.

### Requests

**One flow, approved by a second person, run as one queue job per handler.**

- Flow: `received → verified → approved → running → completed`, with `refused` from `received`, `verified` or `approved`.
- `verified` records how the requester's identity was checked (free text, bounded, never shown in exports).
- Erasure uses Workflow's shared approval case ([ADR 15](15-shared-approval-cases-in-workflow.md)). Approval needs `privacy.request.approve` and the Step-up operation `base.privacy.erasure.approve` declared `.floor()`. Exclude the data subject's linked accounts, the initiating account and staff who received or verified the request, including their impersonators.
- Under [ADR 01](01-agent-first-operations.md), an independent person's AI agent may approve with that person's live permission and required 2FA code. The same independence checks apply to the person and their agent. A background job or system principal never approves.
- `running` enqueues one idempotent, retried Queue job per handler. The request completes only when every handler has answered. A failed handler stays retrying and the request shows as late; it never completes silently.
- A store that cannot erase in time (an installer backup, an external system behind an Extension) answers with a deferral, a reason and a completion date.
- **No silent matching.** Matches only by email or phone are listed for the operator to confirm, as `core/person` duplicates already propose without choosing.
- `privacy.request.due_days` is a tenant setting (default 21, the PDPA access-request period). Requests near it notify through `base/notifications`.

### Export and certificate

- The export is one artifact with an expiry, read through `Artifacts::read`, so every download is audited.
- No erasure trail (request, certificate, ledger or sweep summary) may store the erased values.
- The certificate is rendered through A4 and stored with `put_retained`. It lists each store, its action, counts, records kept and why, deferrals with dates, and the approver. For records still within their legal period, it states `kept until <date>` and the reason.

### Legal hold

**`base/privacy` holds a person or one record; artifact holds stay authoritative for documents.**

- Each hold has a reason, an actor and append-only history, like `base_artifact_retention_events`.
- Handlers and sweeps ask privacy whether a subject or record is held before erasing or purging.
- A held record survives erasure and sweeps and is named on the certificate. After release, the next sweep acts.

### Retention

**Modules contribute record classes; each company holds its retention periods and legal minimums in settings.**

- A `RecordClass` declares the clock start, `purge` or `anonymise`, a legal minimum, and its sweep handler. When a record's period ends, the sweep anonymises its declared personal fields. `purge` (row deletion) is allowed only for classes that declare they hold no business value, such as temporary logs or abandoned drafts.
- Every class defaults to no period: records are kept until a company chooses otherwise. A module may not ship a default period that erases data on its own. Bilibi sets no upper limit by default.
- Each company sets an optional period and its legal minimum per class through the retention settings page in the existing `base/settings_editor`, under tenant-scoped company settings. A period below the company's legal minimum for that class is refused.
- Bilibi ships no country rules. Ready-made rule sets, such as Malaysia PDPA or EU GDPR, come later as plug-ins that fill those company settings.
- Audit and financial records (payroll, claims, ledgers) keep their personal fields for their legal period. An erasure request acts at once on everything else, subject to holds and declared deferrals. An ordinary retention period does not delay a request.
- The certificate lists the legal deadline and reason for each record kept. The sweep anonymises its declared personal fields after that date, subject to holds.
- Every sweep anonymisation or purge writes through `MutationBoundary`, like request erasure. Audit records the record, field names and actor or system scope, with the sweep run as the cause.
- Each sweep run writes one append-only summary row: tenant, record class, counts, and records skipped for legal hold or legal minimum.
- One contributed schedule definition runs the sweeps in batches. Like every contributed task, it runs only after an operator enables it. General retention sweeps act only on classes that have a period set and are enabled.
- Schedule definitions are installation-wide today. The sweep worker visits each tenant that has enabled classes, under a system scope for that tenant. If per-tenant scheduled work ([issue 138](https://github.com/BelimbingApp/bilibi/issues/138)) lands first, the sweep uses it instead.

### Audit erasure

**Replace erased values in earlier evidence through one guarded database function, and record the erasure in an append-only ledger.** Crypto-shredding was considered and rejected for now.

| | Guarded replace (chosen) | Crypto-shredding |
| --- | --- | --- |
| Change to capture | None | Every captured value encrypted under a per-person key |
| Rows with several people | Replace only the declared fields of the subject | Needs a key per value, or one person's erasure loses another's data |
| Existing evidence | Reachable | Plaintext rows written before the change stay readable |
| Console and audit screens | Unchanged | Need decryption; SQL search over values stops working |
| Append-only promise | One narrow, ledgered exception | Kept literally, but key destruction is itself unrecorded state |

- The function is callable only inside a privacy erasure transaction. It checks a transaction-local marker that only `base/privacy` sets, and the declared field list for the record kinds.
- Audit evidence within its legal period is kept until that date, with the reason on the certificate. The request erases everything else at once, subject to holds and declared deferrals; the sweep anonymises retained evidence after its legal deadline.
- It replaces those fields in `old_values`, `new_values` and declared personal subject identifiers with `[erased]`. Row IDs, actor IDs, time, action, record references and field names remain.
- It writes one append-only ledger row per call: request, record kinds, row counts, fields and actor.
- A hash chain links each audit row to the previous row over its non-personal fields: ID, time, actor ID, action, record and field names. Personal values are excluded, so approved redaction does not break the chain. Deleting a row or changing an ID, actor or time breaks it.
- A “verify audit log” check verifies the chain and reports any break.
- The evidence guard still refuses every other update, delete and truncate.

### File erasure

**`base/media` gains an owner-authorised erasure of a submitted attachment that removes the bytes and leaves a tombstone row.** The `ImmutableAttachment` rule still holds for every other path.

### Masking

**A partial-value restriction mode beside today's removal, with audited reveal.**

- Off until configured. A masked field shows a partial value by kind: `a•••@example.com`, `•••• 1234`, year only.
- Reveal needs `privacy.field.reveal` and a reason, writes an audit action, and can require Step-up.
- Lists, records, exports, history and agent JSON all go through `fields::mask`. No route gives a reader without reveal the full value.

### Tenancy

**Requests, holds, company retention settings and certificates are tenant-scoped.** The platform operator cannot read another tenant's requests or exports.

## Phases

| Phase | Contents |
| --- | --- |
| First | Registry, personal field marks, legal hold, requests with approval and certificate, audit erasure path with hash chain and “verify audit log” check, record classes with company legal-minimum settings and deferred-erasure sweep with audit and summary, media erasure, Platform handlers (`core/person`, `core/user`, `core/affiliation`, `base/media`, `base/artifacts` through each Owner, `base/events` payloads, `base/integration` previews, `base/audit`) |
| Second | Optional company retention periods (default none, no upper limit by default), sweeps for classes with a period set and enabled, retention page in the existing settings editor |
| Third | Partial masking and audited reveal |

## Consequences

- A Domain declares what personal data it holds and how to export and erase it. It builds no request flow, certificate or retention page of its own.
- A Domain that marks a Grid field personal must contribute a handler, or boot fails. Marks are opt-in, so official Domains keep booting unchanged.
- Erasure preserves business rows, IDs, keys, links and amounts. Only declared personal fields are anonymised; file bytes are removed with tombstones kept.
- Audit and financial records keep personal fields until their legal deadline. Certificates explain what remains and why; the sweep anonymises it when due.
- `base/audit` evidence is no longer strictly append-only. The one exception is narrow, guarded and ledgered, and its README says so. The hash chain survives approved redaction; the verification check reports breaks in the non-personal evidence.
- Companies maintain their own retention periods and legal minimums. Bilibi supplies no country rules; plug-in rule sets may fill those settings later.
- With nothing configured, pages, writes and audit behave exactly as before.
- Proof follows issue 132's acceptance list on the walkthrough fixture, with browser evidence per [runtime parity](../../runtime-parity.md). No unit tests.

## Out of scope

- Consent capture and withdrawal: its own item. The consent store will register a handler.
- Rectification: through each module's normal edit paths.
- Data residency: a deployment decision, not application behaviour.
- Backup policy, archive tiers, breach notification, DPIA support and a compliance register.
- A search index: none exists. One added later registers a handler.
