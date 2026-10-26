# Personal data (privacy)

**Data-subject requests, legal holds, retention sweeps and masked-field reveals, above the `base/personal_data` contract.** The decision is [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md); the work is tracked in [issue 132](https://github.com/BelimbingApp/bilibi/issues/132). No Bilimbi counterpart.

Stores, record kinds, grants and the hold guard are contracts in [`base/personal_data`](../personal_data/README.md). This module runs them.

## What exists now

| Part | Status |
| --- | --- |
| Registry contract (`base/personal_data`), Grid personal field marks, boot check | Built |
| Legal holds, guarded against running erasures and sweeps | Built |
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
| Approve | `privacy.request.approve` | A person, never an agent or principal, who can file under the request's company (their own, or reachable when unaffiliated). An erasure also needs someone other than its receiver and Step-up `base.privacy.erasure.approve` (`.floor()`). Checked before any store runs |
| Refuse | `privacy.request.receive` | From `received` or `verified`, with a reason |

- One Queue job per store (`base/privacy-request-store`), as the approver, retried 25 times. A failing store keeps the request `running` and late.
- Each store gets a sealed grant for this request: `Disclosure` to export, `Erasure` to erase.
- Erasure: the store's `erase` runs in its own transaction under the hold guard; every record it returns has its kind's declared audit fields cleared (`bilibi_audit::erase_record`), honouring the filing company's `privacy.audit.legal_minimum_days`; evidence kept for it is listed with its end date. `after_erase` saves each step as it goes, so a retry still counts earlier steps.
- Finish (`base/privacy-request-finish`): one at a time per request; writes the export or certificate and completes. A failure is recorded on the request (`finish_attempts`, `finish_error`) and shown on its page.
- On erasure completion every request about the person forgets its personal text (identifiers, identity note, refusal reason), staged exports are dropped and earlier exports deleted.
- Lists page, count and search in SQL. Agents: `base.privacy.request.*` (approve needs a person).

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
- `Sweeper::sweep(tx, scope, grant, cutoff, holds, after, limit)` examines records above `after` in ID order, skips held ones (counted as kept once), and returns `next` until done.
- Records of no one company follow the tenant's primary company.
- Definition `base/privacy.retention-sweep` (02:00 UTC), as the `scheduler` principal per tenant. Each class run writes one `base_privacy_sweeps` summary: company, period, cutoff, acted, kept, batches. The same run erases audit evidence whose legal minimum has passed (class `base/privacy.audit_evidence`).
- `/privacy/retention` reads one company's settings and the last sweeps, and links to System Settings.

## Masking and reveal

**A field restricted in mask mode shows a partial value; revealing the full value needs `privacy.field.reveal`, a reason, and is recorded.**

- Mode per restricted field on `/authz/field-restrictions`; only Grid-marked personal fields can be masked.
- Reveal: `Privacy::reveal_field`, the contact page's Reveal (through the page contract `base.privacy.field.reveal`), or the agent operation. Capability, reason and Step-up `base.privacy.field.reveal` (off until the tenant turns it on) come first; Grid reads only with the sealed `Reveal` grant; `privacy.field.revealed` is recorded without the value before it is returned.
- A revealed value stays only in that page view, only while the reader may still reveal and the record is unchanged. Embedded contact panels show no Reveal.

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
| `requests-walkthrough` (`seed` first; needs `BILIBI_PRIVACY_DEMO_ROOT`, the three dev keys, and `BILIBI_PRIVACY_DEMO_CHROMIUM` for the A4 certificate) | ZIP export with files; masked reader needs a reason; candidates confirmed one by one, unconfirmed untouched; failing store retried; certificate with kept records and their holds only; every request of the person forgets its personal text; another company's employee cannot approve; 508 requests searched in SQL; due notices past 500 un-notifiable requests |
| `retention-walkthrough` (`seed` first) | Constraint refusals; concurrent period and minimum saves; nothing swept until turned on; 205 records in one summary; a name already `[erased]` still anonymised; evidence inside the legal minimum kept with a deferral; audit ledger and evidence refuse direct writes |
| `masking-walkthrough` | Partial values on every read path, reveal rules and records |
