# ADR 05: Personal-data registry, data-subject requests and audit erasure

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** Personal-data retention, legal hold, access and erasure requests, and field masking across every mounted module
**Last Updated:** 2026-10-22

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
| Retention | `base/privacy`, `base/settings`, `base/schedule` | Contributed record classes, tenant overrides, sweeps |
| Masking | `base/authz`, `base/grid` | Partial-value restriction mode with audited reveal |
| Audit erasure | `base/audit` | One guarded function that replaces erased values in earlier evidence |
| File erasure | `base/media` | Owner-authorised erasure of a submitted attachment, leaving a tombstone |

### Handler contract

**Every handler takes a sealed `Scope` and a `Subject`, and Base needs no Core dependency.**

- `Subject` is a `bilibi_party_ids::PersonId` plus listed identifiers (email, phone). It never carries free text from the requester.
- `locate` returns counts per record kind, split into matches by Person and matches only by identifier.
- `export` returns JSON plus file references (media and artifact IDs). Base resolves the files; the handler never copies bytes.
- `erase` anonymises or deletes, and returns counts per record kind plus records kept, each with a reason (`legal_hold`, `retained_until`, `legal_minimum`, `deferred`) and a date when known.
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
- Erasure approval needs `privacy.request.approve`, held by someone other than the person who received the request, and the Step-up operation `base.privacy.erasure.approve` declared `.floor()`.
- Agents may prepare and verify requests through `agent_api`. Approval needs a person.
- `running` enqueues one idempotent, retried Queue job per handler. The request completes only when every handler has answered. A failed handler stays retrying and the request shows as late; it never completes silently.
- A store that cannot erase in time (an installer backup, an external system behind an Extension) answers with a deferral, a reason and a completion date.
- **No silent matching.** Matches only by email or phone are listed for the operator to confirm, as `core/person` duplicates already propose without choosing.
- `privacy.request.due_days` is a tenant setting (default 21, the PDPA access-request period). Requests near it notify through `base/notifications`.

### Export and certificate

- The export is one artifact with an expiry, read through `Artifacts::read`, so every download is audited.
- The certificate is rendered through A4 and stored with `put_retained`. It lists each store, its action, counts, records kept and why, deferrals with dates, and the approver.

### Legal hold

**`base/privacy` holds a person or one record; artifact holds stay authoritative for documents.**

- Each hold has a reason, an actor and append-only history, like `base_artifact_retention_events`.
- Handlers and sweeps ask privacy whether a subject or record is held before erasing or purging.
- A held record survives erasure and sweeps and is named on the certificate. After release, the next sweep acts.

### Retention

**Modules contribute record classes; tenants override periods; nothing runs until an operator enables it.**

- A `RecordClass` declares a default period (or none, which keeps forever), the clock start, `purge` or `anonymise`, an optional legal minimum, and its sweep handler.
- Tenants override periods in tenant-scoped settings on an operator page. An override below the legal minimum is refused.
- One contributed schedule definition runs the sweeps in batches. Like every contributed task, it runs only after an operator enables it.
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
- It replaces those fields in `old_values`, `new_values` and the subject identifier of that subject's rows with `[erased]`. Who, when, which record and which fields remain.
- It writes one append-only ledger row per call: request, record kinds, row counts, fields and actor.
- The evidence guard still refuses every other update, delete and truncate.

### File erasure

**`base/media` gains an owner-authorised erasure of a submitted attachment that removes the bytes and leaves a tombstone row.** The `ImmutableAttachment` rule still holds for every other path.

### Masking

**A partial-value restriction mode beside today's removal, with audited reveal.**

- Off until configured. A masked field shows a partial value by kind: `a•••@example.com`, `•••• 1234`, year only.
- Reveal needs `privacy.field.reveal` and a reason, writes an audit action, and can require Step-up.
- Lists, records, exports, history and agent JSON all go through `fields::mask`. No route gives a reader without reveal the full value.

### Tenancy

**Requests, holds, overrides and certificates are tenant-scoped.** The platform operator cannot read another tenant's requests or exports.

### Anonymise by default

**Erasure keeps rows, keys, amounts and links and replaces personal fields with a placeholder.** Orders and cases keep their integrity and show an anonymised stub. A handler may delete records with no business value.

## Phases

| Phase | Contents |
| --- | --- |
| First | Registry, personal field marks, legal hold, requests with approval and certificate, audit erasure path, media erasure, Platform handlers (`core/person`, `core/user`, `core/affiliation`, `base/media`, `base/artifacts` through each Owner, `base/events` payloads, `base/integration` previews, `base/audit`) |
| Second | Retention classes, tenant overrides, sweeps and operator page |
| Third | Partial masking and audited reveal |

## Consequences

- A Domain declares what personal data it holds and how to export and erase it. It builds no request flow, certificate or retention page of its own.
- A Domain that marks a Grid field personal must contribute a handler, or boot fails. Marks are opt-in, so official Domains keep booting unchanged.
- `base/audit` evidence is no longer strictly append-only. The one exception is narrow, guarded and ledgered, and its README says so.
- With nothing configured, pages, writes and audit behave exactly as before.
- Proof follows issue 132's acceptance list on the walkthrough fixture, with browser evidence per [runtime parity](../../runtime-parity.md). No unit tests.

## Out of scope

- Consent capture and withdrawal: its own item. The consent store will register a handler.
- Rectification: through each module's normal edit paths.
- Data residency: a deployment decision, not application behaviour.
- Backup policy, archive tiers, breach notification, DPIA support and a compliance register.
- A search index: none exists. One added later registers a handler.
