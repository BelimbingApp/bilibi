# Personal data contract (`base/personal_data`)

**The contract every store of a person's data implements, below Grid, Authz and Audit, so any module can answer for its data without a dependency cycle.** `base/privacy` runs it. Decision: [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md). No Bilimbi counterpart.

## Contribute a store

**Depend on `base/personal_data`, declare your record kinds, contribute one `PersonalData` per store.**

```rust,ignore
inventory::submit! { Contribution::new(RECORD_KINDS, "core/person.person", provenance,
    &RecordKind { kind: "person", fields: &["name", "email", "phone"], subject_identifier: true, person: true }) }
inventory::submit! { Contribution::new(PERSONAL_DATA, "core/person.people", provenance,
    &PersonalData { label: "People (contacts)", kinds: &["person"], handler: &PEOPLE }) }
```

| Rule | Why |
| --- | --- |
| IDs are `<module>.<snake_case name>` | Store and kind keys stay stable |
| A store's `kinds` must be declared `RecordKind`s | Holds and audit erasure name only real kinds |
| `RecordKind.fields` are the top-level keys of the kind's `Change` values that are personal | Audit erasure clears exactly these, never a business field |
| A kind is declared once, by its owner | One answer per kind |

## Handler

| Call | Gets | Answers |
| --- | --- | --- |
| `locate` | plain connection | Counts per kind (by person, by identifier only) and identifier-only `Candidate`s |
| `export` | plain connection, `Disclosure` | Every record (never a first page) and `FileRef`s |
| `file` | plain connection, `Disclosure` | One listed file's bytes, read through its owner |
| `erase` | the erasure `MutationTransaction`, `Erasure`, `Holds` | Actions, erased records, kept records with reason and hold |
| `after_erase` | plain connection, `Erasure`, `Holds`, `Progress` | Work after commit (files, documents), each step recorded through `Progress` |

- Act on records linked to the person and on `Subject.confirmed`; never on another identifier match.
- Changes record `[erased]` (`ERASED`) where a value was, so erasure reads apart from absence.
- Every call is idempotent.

## Grants

**`Erasure`, `Disclosure` and `Reveal` are sealed: only the `Issuer`, claimed once at boot by `base/privacy`, makes them.** Media, Artifacts, Audit and Grid accept a grant as the authority for their erasure, disclosure and reveal paths; `grant.check(scope)` refuses another tenant.

## Holds

**`Holds::acquire(tenant, person)` opens the tenant's hold guard on its own connection; release it after the guarded work commits.** `person_hold`, `records(kind, ids)`, `record`, `persons(ids)` ask only about what is in hand. With no hold source bound, nothing is held.
