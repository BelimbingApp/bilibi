# Personal data contract (`base/personal_data`)

**The contract every store of a person's data implements, below Grid, Authz and Audit, so any module can answer for its data without a dependency cycle.** `base/privacy` runs it. Decision: [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md).
## Contribute a store

**Depend on `base/personal_data`, declare your record kinds, contribute one `PersonalData` per store.**

```rust,ignore
inventory::submit! { Contribution::new(RECORD_KINDS, "core/person.person", provenance,
    &RecordKind { kind: "person", fields: &["name", "email", "phone"], subject_identifier: true, person: true }) }
inventory::submit! { Contribution::new(PERSONAL_DATA, "core/person.people", provenance,
    &PersonalData { label: "People (contacts)", kinds: &["person"], person_subjects: &["core.person"], handler: &PEOPLE }) }
```

| Rule | Why |
| --- | --- |
| IDs are `<module>.<snake_case name>` | Store and kind keys stay stable |
| A store's `kinds` must be declared `RecordKind`s | Holds and audit erasure name only real kinds |
| `RecordKind.fields` are the top-level keys of the kind's `Change` values that are personal | Audit erasure clears exactly these, never a business field |
| A kind is declared once, by its owner | One answer per kind |
| A kind with personal fields also gets a `base_personal_data_kinds` row from its owner's migration (same fields and `subject_identifier`); `*` names every top-level field, for a kind whose values have no fixed keys (a SCIM document) | The database refuses audit erasure of anything else; a mismatch is `Internal`. The table changes only alongside a recorded migration: a guard against mistakes, not against code running as the application's database role, which can also write the ledger |
| `person_subjects` names the file subject kinds (Media attachment subject, Artifacts owner) whose subject ID is the person ID | Media and Artifacts accept a grant only for those subjects, and only the grant's own person |
| Report in `Erased.records` only records changed in that transaction | The database clears evidence only of records the erasing transaction changed |

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
- Every call is idempotent, and decides by current values (not an earlier erasure's marker): a record changed after an erasure is erased again.
- `ErasedRecord.person` names the record's person when the grant names none (a sweep), so kept evidence still honours a hold on that person.

## Grants

**`Erasure`, `Disclosure` and `Reveal` are sealed: only the `Issuer`, claimed once at boot by `base/privacy`, makes them.** Media, Artifacts, Audit and Grid accept a grant as the authority for their erasure, disclosure and reveal paths; `grant.check(scope)` refuses another tenant; a handler given a `Subject` calls `grant.check_subject(scope, subject)`, which also refuses a subject other than the person the request was approved for (a sweep grant answers for no subject). Media and Artifacts call `grant.check_file_subject(scope, kind, id)`: the subject kind must be declared in some store's `person_subjects` and the ID must be the grant's person. An Artifacts owner whose subjects are not person IDs may answer `Owner::erasure_subject` for the grant's person instead (`base/privacy` does, for a request about that person).

## Holds

**`Holds::acquire(scope, &grant)` opens the hold guard of the grant's tenant on its own connection, for the grant's person (none for a sweep); release it after the guarded work commits.** `person_hold(&grant)`, `records(&grant, kind, ids)`, `record`, `persons(&grant, ids)` ask only about what is in hand, and each refuses (`Forbidden`) a grant other than the one the session was acquired for, so a session of another tenant, person or run never answers for the wrong holds. `RecordHolds::acquire(tenant)` is for a module's own retention outside any grant (Integration's payload pruning): it answers only record holds, and as a type of its own it cannot stand in for `Holds`. With no hold source bound, nothing is held.
