# Personal data (privacy)

**Each module that keeps a person's data answers for it through one contributed handler.** The decision is [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md); the work is tracked in [issue 132](https://github.com/BelimbingApp/bilibi/issues/132). No Bilimbi counterpart.

## What exists now

| Part | Status |
| --- | --- |
| Personal-data registry and handler contract | Built |
| Grid personal field marks and boot check | Built |
| Legal holds | Built (facade, operator page, agent operations) |
| Access and erasure requests, export, certificate | Built |
| Audit and media erasure paths | Built |
| Platform handlers: Person, User, Affiliation, Address, Events, Integration | Built |
| Notice as a request nears its due date | Planned |
| Retention classes and sweeps | Planned |
| Partial masking and audited reveal | Planned |

## Contribute a store

**Declare `base/privacy` as a dependency, then contribute one `PersonalData` per store.**

```rust,ignore
static HANDLER: PeopleData = PeopleData;
inventory::submit! { Contribution::new(bilibi_privacy::PERSONAL_DATA, "core/person.people",
    Provenance { module: "core/person", source: file!(), line: line!() },
    &bilibi_privacy::PersonalData {
        label: "People",
        record_kinds: &["person"],
        handler: &HANDLER,
    }) }
```

- The contribution ID is the store key: `<module>.<snake_case name>`.
- `record_kinds` are the `Change::kind` values the store writes, so audit erasure knows whose evidence to clear.
- Boot refuses a store from a module that does not depend on `base/privacy`.

## Handler contract

**Every call takes a sealed `Scope` and a `Subject`, and is idempotent.**

| Call | Client | Answers |
| --- | --- | --- |
| `locate` | plain connection | `Located`: counts per record kind, by person and by identifier only |
| `export` | plain connection | `Exported`: the records as JSON, plus media and artifact references |
| `erase` | the request's `MutationTransaction` | `Erased`: actions per kind, records kept with a reason, an optional deferral, and the erased fields of each record for audit |
| `after_erase` | plain connection, after commit | Work outside the transaction (removing files, deleting artifacts). Retried until it succeeds, so idempotent; keeps what holds name |

- `Subject` is a `PersonId` plus identifiers (email, phone). Match by person first. A match by identifier alone is counted, never acted on, until the operator confirms it.
- Anonymise by default: keep rows, keys, amounts and links, and replace personal fields with `ERASED` (`[erased]`). Delete only records with no business value.
- Write through the supplied transaction and return every change. Never open a second transaction.
- Report what the store must keep (`KeptReason`) instead of erasing it. A store that cannot finish now answers with a `Deferral` and a date.
- `base/privacy` checks the request's approval and Step-up before calling `erase`; the handler does not recheck its own edit capability.

## Personal field marks

**Mark a Grid field that holds a person's data with `Field::personal(PersonalKind::…)`.**

- Kinds: `Name`, `Email`, `Phone`, `Identifier`, `Date`, `FreeText`.
- Boot fails when a Grid table marks a field personal and its module contributes no store, naming the table, the field and the module.
- Marks are opt-in. A table without marks behaves exactly as before.

## Legal holds

**A hold on a person or one record stops erasure and sweeps for what it names.** Artifact holds stay authoritative for documents.

| Call | Needs | Does |
| --- | --- | --- |
| `place_hold(client, scope, target, reason)` | `privacy.hold.manage` | Places a hold on `HoldTarget::Person` or `HoldTarget::Record { kind, id }`. A second active hold on the same target is `Conflict` |
| `release_hold(client, scope, id, reason)` | `privacy.hold.manage` | Releases an active hold once. Another tenant's hold is `NotFound` |
| `holds(client, scope, limit)` | `privacy.hold.view` | The tenant's holds, active first then newest, at most 500 |
| `hold_history(client, scope, id)` | `privacy.hold.view` | Placed and released events, oldest first |
| `active_holds(client, scope, person)` | `privacy.hold.view` | `Holds` for an erasure or sweep: `person_held()`, `record_held(kind, id)` |

- A record is named by its audit record kind (`Change::kind`, as a store lists it in `record_kinds`, such as `person`) and ID. A kind no mounted store declares is `Invalid`: a hold on it would protect nothing.
- A reason is required: 1–255 characters, no control characters. It is a case reference, never personal data.
- Place and release are `MutationBoundary` writes recorded as `base/privacy.hold` (created, then updated).
- `erase` receives the `Holds` read inside the erasure transaction. A held person keeps everything; a held record is listed as kept with `KeptReason::LegalHold`.
- The database guards the same rules for any client (`migrations/001-holds.sql`):
  - a hold has exactly one target
  - a hold is released once, and nothing else about it changes
  - no hold row is deleted or truncated
  - `base_privacy_hold_events` is append-only, with `recorded_at` stamped by PostgreSQL

## Requests

**An access or erasure request runs through one contributed Workflow flow, `received → verified → approved → running → completed`, or `refused`.**

| Step | Needs | Rule |
| --- | --- | --- |
| Receive | `privacy.request.receive` | Kind (`access` or `erasure`), the Person and any identifiers. Due date is receipt plus `privacy.request.due_days` |
| Verify | `privacy.request.receive` | A note saying how the requester's identity was checked |
| Approve | `privacy.request.approve` | An erasure needs someone other than whoever received it, and Step-up `base.privacy.erasure.approve` (`.floor()`). Approval moves the request to `running` and queues one job per store |
| Refuse | `privacy.request.receive` | From `received` or `verified`, with a reason |

- Each store answers in its own Queue job (`base/privacy-request-store`), run as the approver and retried up to 25 times. A failing store leaves the request `running` and marks it late; it never completes.
- When every store has answered or deferred, `base/privacy-request-finish` writes the document and completes the request.
- An erasure runs each store's `erase` in that store's own `MutationTransaction`, marked as the request's erasure and with holds read inside it, clears the returned fields from audit evidence, then calls `after_erase` on a plain connection.
- Agents may list, read, locate, receive, verify, approve and refuse (`base.privacy.request.*`); approving an erasure still needs a person's Step-up.
- Pages: `/privacy/requests`, `/privacy/requests/{id}`, `/privacy/holds`, under Admin → Personal data.

## Documents

**An access request ends with an export; an erasure ends with a certificate. Both are `base/artifacts` documents owned by `base.privacy`.**

| Document | Kept | Read |
| --- | --- | --- |
| Export: one JSON bundle of every store's records, with media and artifact references | `privacy.export_days`, then purged | `GET /privacy/requests/{id}/document` through `Artifacts::read`, so each download is audited |
| Certificate: each store, action, counts, records kept and why, deferrals with dates, approver | `put_retained` for `privacy.certificate_years` | Same route |

## Erasure in audit and media

**Values go; who, when, which record and which fields stay.**

- `bilibi_audit::erase_values` replaces a record's declared fields with `[erased]` in its mutation evidence and writes an append-only `base_audit_erasures` row (`base/audit` README).
- `Media::erase_attachments` turns a person's attachments into tombstones; `Media::remove_erased_bytes` removes the bytes after commit (`base/media` README).
- Both refuse outside a privacy erasure transaction.

## Settings

| Key | Scope | Default | Meaning |
| --- | --- | --- | --- |
| `privacy.request.due_days` | Global, Tenant | 21 | Days from receipt to answer |
| `privacy.export_days` | Global, Tenant | 30 | Days an export stays downloadable |
| `privacy.certificate_years` | Global, Tenant | 7 | Years a certificate is retained |

## Runtime checks

No unit tests. Prove on a disposable database ([runtime parity](../../../docs/runtime-parity.md)).

| Case | Exercise | Expected |
| --- | --- | --- |
| Nothing marked | Boot the app | Boots; pages unchanged |
| Mark without a store | Mark a Core Grid field personal, boot | Boot fails naming the table, field and module |
| Store without the dependency | Contribute a store from a module that does not declare `base/privacy` | Boot fails naming the store |
| Legal holds | `examples/holds-walkthrough.rs` on a disposable database, then again with `report` | Place, duplicate `Conflict`, release once, history; viewer refused; tenant 2 sees none and gets `NotFound`; every raw edit, delete and truncate refused by the database |

| Requests | `examples/requests-walkthrough.rs` with `seed`, then without, on a disposable database with the queue running | Access: one export covering every store, download audited as `artifacts.read`. Tenant 2 sees no request. Erasure: refused for whoever received it, approved by a second person with an authenticator code; a failing store leaves it `running` and late until a retry answers; afterwards person and account are stubs, attachment bytes gone, document deleted, audit values `[erased]` with ledger rows, certificate lists every store; a console search finds no erased value. A hold on an unknown kind is `Invalid`; a held person is kept and named on the certificate; after release a new erasure acts |

Run the holds walkthrough only against a fresh disposable database:

```sh
BILIBI_DISPOSABLE_PRIVACY_DEMO=yes DATABASE_URL=postgres://…/disposable \
  cargo run -p bilibi-privacy --example holds-walkthrough
cargo run -p bilibi-privacy --example holds-walkthrough -- report   # same env
```

The requests walkthrough also needs a private folder for files and the three development keys (`./scripts/start-app.sh` creates them under `.local/dev/`):

```sh
export BILIBI_DISPOSABLE_PRIVACY_DEMO=yes BILIBI_PRIVACY_DEMO_ROOT=/tmp/privacy-demo DATABASE_URL=postgres://…/disposable
export BILIBI_SETTINGS_KEY=… BILIBI_USER_SIGNING_KEY=… BILIBI_MEDIA_SIGNING_KEY=…
cargo run -p bilibi-privacy --example requests-walkthrough -- seed   # production seeds, own process
cargo run -p bilibi-privacy --example requests-walkthrough
```
