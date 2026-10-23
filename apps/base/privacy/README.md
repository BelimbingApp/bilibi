# Personal data (privacy)

**Each module that keeps a person's data answers for it through one contributed handler.** The decision is [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md); the work is tracked in [issue 132](https://github.com/BelimbingApp/bilibi/issues/132). No Bilimbi counterpart.

## What exists now

| Part | Status |
| --- | --- |
| Personal-data registry and handler contract | Built |
| Grid personal field marks and boot check | Built |
| Legal holds | Built (facade; operator page planned) |
| Access and erasure requests, export, certificate | Planned |
| Audit and media erasure paths | Planned |
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
        record_kinds: &["core/person.person"],
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
| `erase` | the request's `MutationTransaction` | `Erased`: actions per kind, records kept with a reason, an optional deferral |

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

- A record is named by its audit record kind (`Change::kind`) and ID.
- A reason is required: 1–255 characters, no control characters. It is a case reference, never personal data.
- Place and release are `MutationBoundary` writes recorded as `base/privacy.hold` (created, then updated).
- `erase` receives the `Holds` read inside the erasure transaction. A held person keeps everything; a held record is listed as kept with `KeptReason::LegalHold`.
- The database guards the same rules for any client (`migrations/001-holds.sql`):
  - a hold has exactly one target
  - a hold is released once, and nothing else about it changes
  - no hold row is deleted or truncated
  - `base_privacy_hold_events` is append-only, with `recorded_at` stamped by PostgreSQL

## Runtime checks

No unit tests. Prove on a disposable database ([runtime parity](../../../docs/runtime-parity.md)).

| Case | Exercise | Expected |
| --- | --- | --- |
| Nothing marked | Boot the app | Boots; pages unchanged |
| Mark without a store | Mark a Core Grid field personal, boot | Boot fails naming the table, field and module |
| Store without the dependency | Contribute a store from a module that does not declare `base/privacy` | Boot fails naming the store |
| Legal holds | `examples/holds-walkthrough.rs` on a disposable database, then again with `report` | Place, duplicate `Conflict`, release once, history; viewer refused; tenant 2 sees none and gets `NotFound`; every raw edit, delete and truncate refused by the database |

Run the holds walkthrough only against a fresh disposable database:

```sh
BILIBI_DISPOSABLE_PRIVACY_DEMO=yes DATABASE_URL=postgres://…/disposable \
  cargo run -p bilibi-privacy --example holds-walkthrough
cargo run -p bilibi-privacy --example holds-walkthrough -- report   # same env
```
