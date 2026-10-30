# Bulk data: CSV import and export

`base/bulk_data` lets people bring a module's records in from a CSV file through that module's own write contracts, and take a list's records out to CSV through its own list contract, for any module that opts in ([ADR 10](../../../docs/architecture/decisions/10-queued-jobs-call-contracts.md), issue 133). Bulk updates and `FlexTable` row selection are later slices. No Bilimbi or Belimbing counterpart.

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
    export: Some(Export { capability: EXPORT, list: "core.person.list" }),
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
| `export` | Optional; `None` keeps the type out of export. The owner's tenant capability to export it (verb `export`) and its list read contract, which takes integer `page` and `page_size` and returns the records with their `import.id` |

- **Mappable fields are data:** the create contract's top-level string, number, integer and boolean properties, with its `enum` as choices and `required` as required.
- **Boot refuses** an unknown or foreign contract, a write a job cannot call (step-up declared, or `Channel::Job` left out), a list read a job cannot call or that does not page, an undeclared or foreign capability, and a match field, reference, lookup property or parent that does not exist.
- A lookup read must match exactly (Core Person's duplicates read matches email exactly); a scoring read such as Core Company's duplicates is not a lookup. Several matches are always a row error.

Adopters: Core Person (`core/person.contacts`, matched on email or phone; exported from the Contacts list with `admin.person.export`) and Core Affiliation (`core/affiliation.affiliations`, the contact by source key of a contacts import or by email; the organisation as a column of ids or one fixed value).

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

## An export

- **Start** from the list (Contacts: **Export CSV**) or `base.bulk_data.export.start`: the list's filters (its list contract's input without `page` and `page_size`, checked against that contract's schema) or the ticked `ids`, and the columns it shows (Grid specs, 1 to 64, each once). It needs `admin.import.view` and the type's export capability, and a company; the export is filed under it.
- **Runs** in the background as the person (`base/bulk_data-export`, up to 5 attempts): it rechecks both capabilities, pages through the list contract, then projects the records through Grid in the list's order (ticked ids go straight to Grid, in the order given). A field restricted for the person is blank; another tenant's or company's record never appears, since the list contract and Grid both read as the person.
- **Refused in words**, with no file: more records than `bulk_data.export_max_rows` (nothing is cut off), a list that takes longer than ten minutes to read, a revoked grant, a column the list no longer has, a file too large for private documents. A file stored by an attempt that then fails is deleted, not left to expire.
- **The file**: UTF-8 with a byte-order mark, the columns' labels as its header, a cell a spreadsheet would run as a formula prefixed with `'`. It downloads from `/exports/{id}/file`, read through Artifacts every time, and expires after `bulk_data.file_days`.
- **`/exports/{id}`** shows its state (Preparing, Ready, Not finished, Stopped) and the download; the person gets a notice when it ends. An export is visible only to the person who started it.

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
| `base.bulk_data.export.get`, `.list` | read | One of the person's exports, with its download path once ready; their newest exports |
| `base.bulk_data.export.start` | write | Start an export of a list (filters as JSON object text) or of ticked ids |

Receiving the file is the page's `receive` event, a listed allowance: Artifacts takes bytes outside a contract, and a plan's input is bounded at 1 MiB. Agents work on runs their person uploaded until agent-supplied files land.

## Storage and audit

`base_bulk_data_runs`, `base_bulk_data_rows` and `base_bulk_data_mappings` (migration `base-bulk-data-001-imports`), and `base_bulk_data_exports` (`base-bulk-data-002-exports`), every statement carrying the tenant. Changes are audited as `base/bulk_data.run`, `.row`, `.mapping` and `.export`, never with a file's data or an export's filter values; the owner audits each record it writes. An export keeps its filters only while it runs: a search can quote a person's name or email.

Settings (operator UI, group Bulk data): `bulk_data.import_max_bytes`, `bulk_data.import_max_rows`, `bulk_data.export_max_rows` and `bulk_data.file_days`. The uploaded file (kind `import-source`) and the exported file (kind `export`, subject `export:{id}`) are private artifacts of owner `base.bulk_data` that expire after `bulk_data.file_days`.

## Walkthrough

On a disposable database, set `DATABASE_URL`, `BILIBI_BULK_DATA_DEMO=disposable`, `BILIBI_SETTINGS_KEY` and `BILIBI_USER_SIGNING_KEY`, then run `cargo run -p bilibi-bulk-data --example import-walkthrough` from a scratch directory: an unseeded database names no private document folder, so it keeps uploads under `./data/artifacts`. It migrates, installs the walkthrough fixture and, as its manager (`manager@example.test`, whose fixture role holds the import capabilities), imports 1,000 contacts through the contracts and a real Queue executor: a saved mapping reused, a dry run that writes nothing and reports errors and duplicates, the three duplicate policies and a row matching two contacts, affiliations finding their contacts by source key, a revoked grant, a worker stopped mid-run, a file refused for each reason, tenant isolation and the audit trail under the run's trace ID.

`cargo run -p bilibi-bulk-data --example export-walkthrough`, with the same variables, exports the fixture's contacts as its manager: the filtered rows and shown columns, link columns, a restricted field left blank and a formula-like name opened as text, refusals before it starts, the row limit failing it in words, a grant revoked before the job runs, ticked records only (another tenant's left out), another tenant refused, the audit trail without filter values, the file's expiry and the person's notices.
