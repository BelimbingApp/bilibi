# Personal data (privacy)

**Data-subject requests, legal holds, retention sweeps and masked-field reveals, above the `base/personal_data` contract.** The decision is [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md); the work is tracked in [issue 132](https://github.com/BelimbingApp/bilibi/issues/132). No Bilimbi counterpart.

Stores, record kinds, grants and the hold guard are contracts in [`base/personal_data`](../personal_data/README.md). This module runs them.

## What exists now

| Part | Status |
| --- | --- |
| Registry contract (`base/personal_data`), Grid personal field marks, boot check | Built |
| Legal holds, guarded against running erasures, sweeps and owed cleanup | Built |
| Access and erasure requests, confirmed identifier-only candidates, export ZIP, A4 certificate | Built |
| Audit erasure with legal minimum and deferral, hash chain and "Verify audit log" | Built (`base/audit`, `base/audit_views`) |
| Media and Artifacts erasure and disclosure under a sealed grant | Built |
| Platform stores: Person, User, Affiliation, Address, Events, Integration, Audit | Built |
| Due notices | Built |
| Record classes, company periods and legal minimums, sweeps with one summary per run | Built |
| Partial masking and audited reveal, export reveal | Built |

## Boot check

**A Grid table that marks a field personal needs a store from the same module, or boot fails naming the table, field and module.** Marks are opt-in.

## Legal holds

**A hold on a person or one record stops erasure and sweeps for what it names.** Artifact holds stay authoritative for documents.

| Call | Needs | Does |
| --- | --- | --- |
| `place_hold(client, scope, target, reason)` | `privacy.hold.manage` | `HoldTarget::Person` or `HoldTarget::Record { kind, id }`. A second active hold on the same target is `Conflict`, also when two arrive at once |
| `release_hold(client, scope, id, reason)` | `privacy.hold.manage` | Once. Another tenant's hold is `NotFound` |
| `holds(client, scope, search, offset, limit)` | `privacy.hold.view` | One page, active first, and the total; `search` matches a hold, person or record ID or a record kind |
| `hold_history(client, scope, id)` | `privacy.hold.view` | Placed and released events |

- A record is named by a kind some module declared (`RECORD_KINDS`); another kind is `Invalid`.
- A reason is 1–255 characters, no control characters: a case reference.
- **Guard:** erasures and sweeps hold the tenant's hold guard (a shared advisory lock on its own connection) from before their transaction until after it commits; placement takes it exclusively. A hold placed meanwhile waits; the next erasure sees it.
- Erasure and sweep reads ask only about the records in hand (`Holds::records`, `persons`), never every hold in the tenant.
- The database refuses a second target, re-release, edits, deletes and truncates (`migrations/001-holds.sql`).

## Requests

**One Workflow flow: `received → verified → approved → running → completed`, or `refused`.**

| Step | Needs | Rule |
| --- | --- | --- |
| Receive | `privacy.request.receive` | Kind, Person, identifiers, filing company. Due after `privacy.request.due_days` |
| Locate | `privacy.request.view` | Counts per store and kind; records found only by an email or phone (phones by their digits) are listed as candidates |
| Verify | `privacy.request.receive` | How the identity was checked, and which candidates are the person's. A confirmation that is not a current candidate is `Invalid` |
| Approve (access, portability) | `privacy.request.approve` | A person or their AI agent (ADR 01), never a system principal or a queued job, who can file under the request's company (their own, or reachable when unaffiliated). Checked before any store runs; approved, store rows and running commit together |
| Send an erasure for approval | `privacy.request.receive` | A verified erasure becomes a round of Workflow's shared approval case `privacy.erasure` (ADR 15). The case's rules must be published for the company first (Approval rules, or `bilibi_privacy::erasure_rules()` from an installation step) |
| Decide an erasure | `privacy.request.approve`, Step-up `base.privacy.erasure.approve` (`.floor()`) | On Waiting for my approval or `base.privacy.erasure.decide`: a person or their agent, never a system principal or job, and never impersonating. Excluded: every account linked to the data subject, whoever received or verified the request, and an operator who impersonated them. The final approval starts the run in the same transaction; a rejection (with its reason) or a recall returns the request to its receiver with a new version, to send again or refuse |
| Refuse | `privacy.request.receive` | From `received` or `verified`, with a reason, once no approval round is open. The reason stays on the request row, never in Workflow history |

- **The owner enforces the flow, whoever drives the Workflow engine:** an erasure becomes `approved` only while its approval case applies the final decision (a crate-private task-local, never a transition input); `running` needs one store row per mounted store; `completed` needs every store answered and finished and the export or certificate filed. Workflow comments on a request are refused (history is permanent; personal text must stay erasable).

- One Queue job per store (`base/privacy-request-store`), as the approver, retried 25 times. A failing store keeps the request `running` and late.
- Each store gets a sealed grant for this request: `Disclosure` to export, `Erasure` to erase.
- Erasure: the store's `erase` runs in its own transaction under the hold guard; every record it returns has its kind's declared audit fields cleared (`bilibi_audit::erase_record`). Each evidence row keeps its own company's `privacy.audit.legal_minimum_days`, whatever company filed the request; rows of no company keep the strictest. Evidence kept is listed with its end date. `after_erase` saves each step as it goes, so a retry still counts earlier steps.
- Stores decide by current values: a person or account changed after an earlier erasure is erased again.
- **Erasure and access take turns per person** (a per-person advisory lock): an access store does not read while an erasure of the person runs (it retries), and an export is published only if every store's staged read is from the person's current erasure generation; otherwise the artifact is discarded and every store reads again.
- **Owed cleanup:** an erasure that completes while records were kept for a legal hold or a retention date, or a store deferred, writes a `base_privacy_resumptions` row. The daily retention run erases the person again under that request's grant (whatever the class settings), in the stores that kept or deferred only, keeping what is still held or retained, until nothing is kept for those reasons; then the row is done. It is not retried before `not_before`, the earliest date a kept record or deferral ends; a hold has no end, so while one keeps a record the cleanup is retried at every run. A completed request authorises erasure only while its cleanup is owed.
- Erasing an account deletes its sign-in, reset, external identity and credential rows, each recorded as its own change (keys only, never a token or external subject).
- Finish (`base/privacy-request-finish`): one at a time per request; writes the export or certificate and completes. A failure is recorded on the request (`finish_attempts`, `finish_error`) and shown on its page.
- On erasure completion every request about the person forgets its personal text (identifiers, identity note, refusal reason), staged exports are dropped and earlier exports deleted, whichever company they were filed under (request documents answer `erasure_subject` for their request's person).
- Lists page, count and search in SQL. Agents: `base.privacy.request.*` and `base.privacy.erasure.submit`; the erasure decision is the case's contract `base.privacy.erasure.decide`.

## Documents

| Document | Kept | Read |
| --- | --- | --- |
| Export: one ZIP, `export.json` with every store's records plus each listed file, read through its store | `privacy.export_days`, then removed by the export purge schedule | `GET /privacy/requests/{id}/document`, through `Artifacts::read` |
| Certificate: A4 PDF (`render_inline`): each store, action and counts, every record kept with reason and end date, the holds that kept them, receiver, verifier and approver; never the identity note | Retained for `privacy.certificate_years` | Same route |

- An export larger than `artifacts.max_bytes` fails as `export_too_large`, never shorter.
- A reader who sees any field restricted needs `privacy.field.reveal` and a reason (`?reason=`) to download an export; it is recorded as `privacy.export.revealed`.
- Export purge: definition `base/privacy.export-purge` (03:30 UTC), runs once enabled.

## Due notices

**A daily task tells a request's handlers its due date is near, once.**

- Definition `base/privacy.due-notices` (07:00 UTC), runs once enabled and the `scheduler` principal is granted.
- Each tenant, as the `scheduler` principal: open requests due within `privacy.request.notice_days`, in due order, page after page, so ones nobody can be told about never hold back the rest.
- Rechecked under the request's row lock: a request closed meanwhile is not notified.
- Recipients: receiver, verifier, approver still active (`NotificationBoundary::eligible`), never a scope built for them.
- The due time is shown in the request's company zone, named, and in UTC.

## Retention

**Modules contribute record classes; each company turns a class on and sets its period and legal minimum in System Settings; one daily sweep acts.** Nothing is swept by default; no upper limit; no country rules.

```rust,ignore
bilibi_privacy::record_class! {
    module: "core/user", key: "core/user.notifications", label: "Notifications",
    clock_start: "when the notification was sent", action: Purge,
    business_value: false, floor_days: None, sweeper: &NOTIFICATIONS,
}
```

- The macro contributes the class and its company settings: `privacy.retention.<key>.enabled` (off), `.period_days` (empty), `.legal_minimum_days` (empty), and a constraint refusing a period below the legal minimum or the class floor.
- `Purge` is refused at boot for a class with business value.
- `Sweeper::sweep(tx, scope, grant, companies, cutoff, holds, after, limit)` examines the records `companies` covers above `after` in ID order, skips held ones (counted as kept once), and returns `next` until done. A sweeper filters with `Companies::predicate(column, n)` and `Companies::parameters()`.
- Each company's records follow that company's settings, swept in a run of their own. The primary company's run also covers records of no company, or of a company that no longer exists.
- Definition `base/privacy.retention-sweep` (02:00 UTC), as the `scheduler` principal per tenant. Each company's class run writes one `base_privacy_sweeps` summary: company, period, cutoff, acted, kept, batches. The same run resumes owed erasure cleanup, then erases audit evidence whose legal minimum has passed (class `base/privacy.audit_evidence`), skipping deferrals whose record or person is held (counted as kept). Each part runs even if another failed; the run then retries.
- `/privacy/retention` reads one company's settings and that company's last sweeps, and links to System Settings.

## Masking and reveal

**A field restricted in mask mode shows a partial value; revealing the full value needs `privacy.field.reveal`, a reason, and is recorded.**

- Mode per restricted field on `/authz/field-restrictions`; only Grid-marked personal fields can be masked.
- Reveal: `Privacy::reveal_field`, the contact page's Reveal (through the page contract `base.privacy.field.reveal`), or the agent operation. Capability, reason and Step-up `base.privacy.field.reveal` (off until the tenant turns it on) come first; Grid reads only with the sealed `Reveal` grant; the value is read next, then `privacy.field.revealed` is recorded without the value, in a transaction that rechecks the capability and commits before the value is returned. An export download is checked before its bytes are read and recorded (`privacy.export.revealed`) the same way after. Each step takes one connection at a time.
- A revealed value stays only in that page view, only while the reader may still reveal and the record is unchanged. Embedded contact panels show no Reveal.

## Failure handling

**Security, authorisation, tenant, hold and integrity failures fail closed: nothing is written and nothing is reported as done.** Every job step is idempotent, so a retry repeats no effect.

| Case | Outcome | Recovery |
| --- | --- | --- |
| Missing capability, wrong tenant, unknown key, failed Step-up, or an excluded person deciding an erasure | Refused before any write | Manual: the right person acts |
| An erasure sent before its approval rules are published | Refused; nothing is sent | Manual: publish the rules on Approval rules, then send again |
| A store fails (database error, failing owner) | Its transaction rolls back; the request stays `running` and late, `last_error` shows a code | Retry: the job retries 25 times; then manual (fix the store, the job is retried) |
| Erasure of the person running while an access store reads | `Conflict`, nothing staged | Retry, automatically, after the erasure |
| Export staged before an erasure, or a staged store missing | Built artifact discarded, staged rows dropped, every store reads again | Retry, automatically |
| Finish fails (artifact, PDF, size, a file no longer found) | Request stays `running` and late; `finish_attempts`, `finish_error` on its page. Only a request that no longer exists cancels its jobs | Retry; `export_too_large` stops: manual |
| Workflow engine moves a request without the owner's work | `running` or `completed` refused | None needed |
| Legal hold on a record or person | Record kept and listed with its hold; owed cleanup persisted | Retry: the daily run after release |
| Document kept for its retention date | Kept and listed with its date; owed cleanup persisted, due at that date | Retry: the first daily run after the date erases it; until then the cleanup is not run |
| Evidence inside a company's legal minimum | Kept with a deferral naming the person | Retry: the daily run once due, unless held |
| Audit erasure with an undeclared kind or field, another person, a record the transaction did not change, or a cause that is not running, owing cleanup or an open sweep in the tenant | Refused by the database | Stop: a code or data defect, fix it |
| A declaration or cause checker changed outside a migration | Refused at commit (a guard against mistakes; code running as the application's database role can also write the ledger) | Stop: ship a migration |
| A reveal's value cannot be read, or its record cannot commit | Nothing returned: the reveal is recorded only after the value or export is read, and returned only once that record commits, one connection at a time | Manual: retry the reveal |
| Code and `base_personal_data_kinds` disagree on a kind | `Internal`, the erasure step rolls back | Stop: ship the migration that matches the code |
| A refused setting in a save | The whole save rolls back (the transaction is poisoned) | Manual: correct the values |
| A committed evidence row without a chain link, or a removed or changed row | Verify audit log reports `unsealed`, `missing`, `changed` or `tail` | Manual: investigate; evidence is never repaired in place |
| One tenant's or one owed cleanup's failure | Recorded; the others still run; the run reports failure | Retry |

Diagnostics hold codes, counts, IDs and kinds only: never a personal value, secret or token.

## Settings

| Key | Scope | Default | Meaning |
| --- | --- | --- | --- |
| `privacy.request.due_days` | Global, Tenant | 21 | Days to answer |
| `privacy.request.notice_days` | Global, Tenant | 3 | Days before due that handlers are told |
| `privacy.export_days` | Global, Tenant | 30 | Days an export stays downloadable |
| `privacy.certificate_years` | Global, Tenant | 7 | Years a certificate is retained |
| `privacy.audit.legal_minimum_days` | Company | empty | Days audit evidence keeps values after an erasure |
| `privacy.retention.<class>.*` | Company | off, empty, empty | Per class: sweep on, period, legal minimum |

## Runtime checks

No unit tests. Each walkthrough runs on a fresh disposable database ([runtime parity](../../../docs/runtime-parity.md)) with `BILIBI_DISPOSABLE_PRIVACY_DEMO=yes` and `DATABASE_URL`.

| Walkthrough | Shows |
| --- | --- |
| `holds-walkthrough` (then `report`) | Place, release, history, isolation, database guards; two simultaneous placements give one hold and one `Conflict`; a hold placed while an erasure guard is open waits for it and the next erasure sees it; paged list with total |
| `requests-walkthrough` (`seed` first; needs `BILIBI_PRIVACY_DEMO_ROOT`, the three dev keys, and `BILIBI_PRIVACY_DEMO_CHROMIUM` for the A4 certificate) | ZIP export with files; masked reader needs a reason; candidates confirmed one by one, unconfirmed untouched; failing store retried; certificate with kept records and their holds only; every request of the person forgets its personal text; an erasure sent for approval cannot be approved outside its case or decided by whoever received and verified it, and runs on an independent person's approval, or on their AI agent's with their Step-up code (never the excluded person's agent or a queued job); a rejection or recall returns it to its receiver; another company's employee cannot approve; 508 requests searched in SQL; due notices past 500 un-notifiable requests |
| `retention-walkthrough` (`seed` first) | Constraint refusals; concurrent period and minimum saves; nothing swept until turned on; 205 records in one summary; a name already `[erased]` still anonymised; evidence inside the legal minimum kept with a deferral; audit ledger and evidence refuse direct writes |
| `masking-walkthrough` | Partial values on every read path, reveal rules and records |
| `failures-walkthrough` (`seed` first; same variables as `requests-walkthrough`) | The database refuses an erasure with a made-up request, sweep, kind or field, and changes to its cause checkers and kinds; rows linked at commit, a bypassed row reported; a refused setting rolls back its save; no request text in Workflow history; the engine alone cannot run or complete; access waits for a running erasure and a staged export is rebuilt after one; an account's sign-in and reset rows audited one by one; a repeat erasure acts on current values; per-company legal minimums; the due sweep honours a person hold; held data erased after release; Search and Grid summaries neither match nor show a masked email; a request cannot erase another person's or an untouched record, and declarations cannot be widened outside a migration; a document retained for 90 seconds is not retried before its date and is deleted by the run after it; history of an affiliation and person already emptied by an edit is erased; an export filed under another company is deleted by an erasure approved by an employee of the request's company |
