# Bulk data: CSV import

`base/bulk_data` lets people bring a module's records in from a CSV file, for any module that opts in, through that module's own write contracts ([ADR 10](../../../docs/architecture/decisions/10-queued-jobs-call-contracts.md), issue 133). Export and bulk updates are later phases. No Bilimbi or Belimbing counterpart.

## Opting a record type in

Declare a dependency on `base/bulk_data` and contribute one `RecordType` per type, with id `<module>.<name>` (the type's key in runs and saved mappings; keep it stable):

```rust
inventory::submit! { Contribution::new(bilibi_bulk_data::RECORD_TYPES, "core/person.contacts", provenance!(), &RecordType {
    label: "Contacts", table: "persons", path: Some("/contacts/{id}"),
    import: Import {
        capability: IMPORT, create: "core.person.create", id: "id",
        duplicates: Some(Duplicates { read: "core.person.duplicates", fields: &["email", "phone"], update: "core.person.update", update_id: "person_id", update_version: "version", update_fields: "identity" }),
        references: &[],
    },
}) }
```

| Field | Meaning |
| --- | --- |
| `label`, `path` | What the records are called; their page with `{id}`, for links from row results |
| `table` | The owner's Grid table: a field restricted there for the person is not offered |
| `import.capability` | The owner's tenant capability to import this type (verb `import`, from its own vocabulary); checked for every row |
| `import.create`, `import.id` | The owner's write contract creating one record, and where the record's id is in its result |
| `import.duplicates` | Optional. The owner's read answering candidates for the match `fields`, and its update contract with the id, version and fields properties. A field the file leaves out keeps the record's value |
| `import.references` | Create fields holding another record's id: a `parent` record type whose earlier import names it by source key, and/or a `lookup` read that must find exactly one record |

- **Mappable fields are data:** the create contract's top-level string, number, integer and boolean properties, with its `enum` as choices and `required` as required.
- **Boot refuses** an unknown or foreign contract, a write a job cannot call (step-up declared, or `Channel::Job` left out), an undeclared or foreign capability, and a match field, reference, lookup property or parent that does not exist.
- A lookup read must match exactly (Core Person's duplicates read matches email exactly); a scoring read such as Core Company's duplicates is not a lookup. Several matches are always a row error.

Adopters: Core Person (`core/person.contacts`, matched on email or phone) and Core Affiliation (`core/affiliation.affiliations`, the contact by source key of a contacts import or by email; the organisation as a column of ids or one fixed value).

## A run

| State | Meaning |
| --- | --- |
| `draft` | Uploaded; map it, check it or import it |
| `previewing`, `previewed` | The dry run is working, or done: every row checked by the owner's preview, nothing written |
| `running`, `completed` | Rows are being written, or all are done; the person gets a notice |
| `cancelled` | Stopped during the import; rows already written stay. A cancelled check returns to `draft` |
| `stopped` (read only) | The job ended without finishing (its person lost access, or every attempt failed); cancel and start again |

- **Upload** (`/imports/new`): the record type and a file. Refused in words: not UTF-8 (a byte-order mark is fine), not CSV, no header, a blank or repeated column, a row wider than the header, larger than `bulk_data.import_max_bytes`, more rows than `bulk_data.import_max_rows`.
- **Mapping**: for each field, a column or a fixed value. A reference field's column is looked up, or names a row of a completed parent import (one of the person's own, in the same company) by its source key. `source_key` names the column a later child import uses. `duplicates`: `skip` (default), `update` or `create`.
- **Check file** (dry run) and **Import** run in the background as the person, through `bilibi_agent_api::job`. Each real row is one transaction: the owner's preview and commit, with the row's result beside it; a refusal is recorded in words and the run goes on. Every row the run writes shares its trace ID.
- **Slices**: an attempt works for two minutes, then queues the rest from the next unrecorded row. A retried attempt never writes a row twice.
- **Access**: each row rechecks the type's import capability and the contract's own; losing `admin.import.create` stops the job.
- **Failed rows** download from `/imports/{id}/failed-rows` (`?pass=preview` for the dry run's): the file's columns plus an Error column, UTF-8 with a byte-order mark, and a cell a spreadsheet would run as a formula is prefixed with `'`.
- **Saved mappings** are saved by name per record type, shared in the company, and offered when the file's header has every column they read.

A run is visible only to the person who started it.

## Contracts

Capabilities `admin.import.view` (pages and reads) and `admin.import.create` (start, check, cancel, save mappings), plus each record type's own.

| Operation | Kind | Does |
| --- | --- | --- |
| `base.bulk_data.types` | read | Record types the person may import, with their fields |
| `base.bulk_data.import.list`, `.get`, `.rows` | read | Their runs, one run (with fitting saved mappings and possible parents), a pass's row results |
| `base.bulk_data.mapping.list` | read | Saved mappings of a record type |
| `base.bulk_data.import.preview`, `.start` | write | Map a run and start the dry run or the real run |
| `base.bulk_data.import.cancel` | write | Stop a working run |
| `base.bulk_data.mapping.save`, `.delete` | write | Saved mappings |

Receiving the file is the page's `receive` event, a listed allowance: Artifacts takes bytes outside a contract, and a plan's input is bounded at 1 MiB. Agents work on runs their person uploaded until agent-supplied files land.

## Storage and audit

`base_bulk_data_runs`, `base_bulk_data_rows` and `base_bulk_data_mappings` (migration `base-bulk-data-001-imports`), every statement carrying the tenant. Changes are audited as `base/bulk_data.run`, `.row` and `.mapping`, never with the file's data; the owner audits each record it writes. The uploaded file is a private artifact (owner `base.bulk_data`, kind `import-source`), kept as Artifacts' retention says.

## Walkthrough

On a disposable database, set `DATABASE_URL`, `BILIBI_BULK_DATA_DEMO=disposable`, `BILIBI_SETTINGS_KEY` and `BILIBI_USER_SIGNING_KEY`, then run `cargo run -p bilibi-bulk-data --example import-walkthrough` from a scratch directory: an unseeded database names no private document folder, so it keeps uploads under `./data/artifacts`. It migrates, installs the walkthrough fixture and, as its manager (`manager@example.test`, whose fixture role holds the import capabilities), imports 1,000 contacts through the contracts and a real Queue executor: a saved mapping reused, a dry run that writes nothing and reports errors and duplicates, the three duplicate policies and a row matching two contacts, affiliations finding their contacts by source key, a revoked grant, a worker stopped mid-run, a file refused for each reason, tenant isolation and the audit trail under the run's trace ID.
