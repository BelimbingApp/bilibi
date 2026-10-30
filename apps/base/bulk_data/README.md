# Bulk data: CSV import, export and bulk updates

`base/bulk_data` lets people bring a module's records in from a CSV file through that module's own write contracts, take a list's records out to CSV through its own list contract, and set one field on many records through its own update contract, for any module that opts in ([ADR 10](../../../docs/architecture/decisions/10-queued-jobs-call-contracts.md), issue 133). Lists tick rows with `FlexTable`'s `Selection` (`apps/base/ui`). No Bilimbi or Belimbing counterpart.

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
    bulk_update: Some(BulkUpdate {
        capability: BULK_UPDATE, list: "core.person.list", get: "core.person.get", get_id: "person_id",
        update: "core.person.update", update_id: "person_id", update_version: "version", update_fields: "identity",
        fields: &["kind"],
    }),
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
| `bulk_update` | Optional; `None` keeps the type out of bulk updates. The owner's tenant capability (verb `bulk-update`), its paged list read (records with `import.id` and `version`), its read of one record by `get_id`, its update contract with the id, version and fields properties, and the `fields` people may set: scalar properties of the update's fields (choices, dates, flags, numbers, or an owner's id) |

- **Mappable fields are data:** the create contract's top-level string, number, integer and boolean properties, with its `enum` as choices and `required` as required.
- **Boot refuses** an unknown or foreign contract, a write a job cannot call (step-up declared, or `Channel::Job` left out), a list read a job cannot call or that does not page, an undeclared or foreign capability, a match field, reference, lookup property or parent that does not exist, and a bulk field that is not a scalar of the update's fields.
- A lookup read must match exactly (Core Person's duplicates read matches email exactly); a scoring read such as Core Company's duplicates is not a lookup. Several matches are always a row error.

Adopters: Core Person (`core/person.contacts`, matched on email or phone; exported from the Contacts list with `admin.person.export`; kind set in bulk with `admin.person.bulk-update`) and Core Affiliation (`core/affiliation.affiliations`, the contact by source key of a contacts import or by email; the organisation as a column of ids or one fixed value).

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

- **Start** from the list (Contacts: **Export CSV** in the header for the filtered list, or in the selection bar for the ticked rows) or `base.bulk_data.export.start`: the list's filters (its list contract's input without `page` and `page_size`, checked against that contract's schema) or the ticked `ids`, and the columns it shows (Grid specs, 1 to 64, each once). It needs `admin.import.view` and the type's export capability, and is filed under the person's own company. Its job runs as the person without the session's company choice, and Artifacts keeps a person's files under their own company, so a session switched to another company is refused in words (imports and bulk updates likewise); running in a switched company needs Queue and Artifacts to carry that choice. Queue proves the person's company and affiliation again for every attempt, so a job whose person was moved to another company since never runs there: the run shows Stopped.
- **Runs** in the background as the person (`base/bulk_data-export`, up to 5 attempts): it first purges the company's expired run files, rechecks the capabilities, pages through the list contract, then projects the records through Grid in the list's order (ticked ids go straight to Grid, in the order given). A field restricted for the person is blank; another tenant's or company's record never appears, since the list contract and Grid both read as the person.
- **No gaps**: offset pages are not a snapshot, so the list is read in full until two reads in a row hold the same records (at most three reads). A record moved, added or dropped while it is paged is never skipped; a list whose members keep changing fails in words. Edits that keep a record in the list do not count against it.
- **Bounded memory**: the file is written a row at a time and stops once it is larger than `artifacts.max_bytes`, so an export never holds more than that much of its file (plus one Grid batch of 100 rows).
- **Refused in words**, with no file: more records than `bulk_data.export_max_rows` (nothing is cut off), a list that takes longer than ten minutes to read, a revoked grant, a column the list no longer has, no shown column the person may read, a file too large for private documents. A file stored by an attempt that then fails is deleted, not left to expire.
- **The file**: UTF-8 with a byte-order mark, the columns' labels as its header, a cell a spreadsheet would run as a formula prefixed with `'`. It downloads from `/exports/{id}/file`, read through Artifacts every time, and expires after `bulk_data.file_days`.
- **Access when downloading**: when the file is made, the export saves what the person could see (the export, list and Imports grants, which columns Grid lets them read, and the type's restricted fields). The download and the export's page compare it every time, as Reports' prepared exports do: a revoked grant or a newly restricted field refuses a file made before it, and they export again.
- **Expiry**: the export keeps its file's expiry; past it, its page and the Imports list show it Expired and offer no download.
- **Stopped jobs**: on its last attempt the job ends the export as failed. A job Queue ends without running it (its person lost the Imports pages first) cannot do that, so the company's next export first ends such exports as failed, drops their filters and tells their persons.
- **Round trip**: an import drops the apostrophe an export puts before a formula-like cell (`'+60 12…`), so a file exported, edited and imported back keeps its values; any other apostrophe stays.
- **Retention**: an hourly scheduled task (`base/bulk_data-file-sweep`, enabled on the Schedule board like every task) purges expired run files in every tenant through `Artifacts::sweep_expired`, so they go even where nobody uploads or exports again; each upload and each export also purges the company's expired files first. A file that cannot be purged is logged by its code, and Artifacts holds it for an operator after its attempt limit.
- **`/exports/{id}`** shows its state (Preparing, Ready, Expired, No longer available, Not finished, Stopped) and the download while it may be taken; the person gets a notice when it ends. An export is visible only to the person who started it.

## A bulk update

| State | Meaning |
| --- | --- |
| `resolving` | The job is finding the records, once, with the version each has now |
| `running`, `completed` | Records are being updated, or all are done; the person gets a notice |
| `cancelled` | Stopped by its person; records already updated stay and are listed |
| `failed` | Ended before updating anything (too many records, none matched, access lost), in words |
| `stopped` (read only) | The job ended without finishing; records already updated stay |

- **Start** from the list's selection bar (Contacts: tick rows, or tick the page and **Select all N matching**, then **Set kind…**) or `base.bulk_data.update.start`: the field, the value as text (converted as an import converts a cell), and either the ticked `ids` or the list's `filter` (`{}` for every record; naming neither, or both, is refused), with `except_ids` for the rows unticked after "Select all N matching", which the bar counts as "599 of 600". Unticked rows are left out before `bulk_data.update_max_records` counts, and are bounded only by what a selection holds (`Selection::MAX_KEYS`). It needs `admin.import.view` and the type's bulk-update capability, and is filed under the company the person works in now; fields restricted for the person are not offered. More than `bulk_data.update_max_records` records are refused.
- **Resolve once**: the job reads the list (until two full reads hold the same records, as an export does) or each ticked record, as the person, and keeps each record's id and its version in the last read; the filter's values are then dropped. A record edited after that is reported stale on its row, so a busy list still resolves. Records of another tenant or company are never found.
- **Each record** is one transaction: the owner's read of it, then its update contract with the resolved version and every other field as read (a hidden field is left out, so the owner keeps it), with the result beside it. A record already set is left alone (`skip`); one changed since it was resolved is refused by the owner's version fence and reported, never overwritten. Every record shares the update's trace ID.
- **Access**: each record rechecks the type's bulk-update capability; a revoked grant refuses the rest in words.
- **Slices**: an attempt works for two minutes, then queues the rest from the next unrecorded record; a retried or resumed attempt never writes a record twice.
- **`/updates/{id}`** shows what it sets, its state and how many records are done, **Stop update** (with a confirmation) while it works, and each record's result in a `FlexTable`. `/imports` lists the person's newest exports and bulk updates under its imports. A bulk update is visible only to the person who started it.

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
| `base.bulk_data.export.start` | write | Start an export of a list (filters as JSON object text, optionally `except_ids` to leave out) or of ticked ids |
| `base.bulk_data.update.fields` | read | The fields of a record type the person may set in bulk |
| `base.bulk_data.update.get`, `.rows`, `.list` | read | One bulk update, its records' results, the person's newest |
| `base.bulk_data.update.start`, `.cancel` | write | Start a bulk update of ticked ids or a list's filters (optionally `except_ids` to leave out); stop one |

Receiving the file is the page's `receive` event, a listed allowance: Artifacts takes bytes outside a contract, and a plan's input is bounded at 1 MiB. Agents work on runs their person uploaded until agent-supplied files land.

## Storage and audit

`base_bulk_data_runs`, `base_bulk_data_rows` and `base_bulk_data_mappings` (migration `base-bulk-data-001-imports`), `base_bulk_data_exports` (`base-bulk-data-002-exports`), and `base_bulk_data_updates` and `base_bulk_data_update_rows` (`base-bulk-data-003-updates`), every statement carrying the tenant. Changes are audited as `base/bulk_data.run`, `.row`, `.mapping`, `.export`, `.update` and `.update_row`, never with a file's data or a filter's values; the owner audits each record it writes. An export keeps its filters only while it runs, and a bulk update only until it has found its records: a search can quote a person's name or email.

Settings (operator UI, group Bulk data): `bulk_data.import_max_bytes`, `bulk_data.import_max_rows`, `bulk_data.export_max_rows`, `bulk_data.update_max_records` and `bulk_data.file_days`. The uploaded file (kind `import-source`) and the exported file (kind `export`, subject `export:{id}`) are private artifacts of owner `base.bulk_data` that expire after `bulk_data.file_days`.

## Walkthrough

On a disposable database, set `DATABASE_URL`, `BILIBI_BULK_DATA_DEMO=disposable`, `BILIBI_SETTINGS_KEY` and `BILIBI_USER_SIGNING_KEY`, then run `cargo run -p bilibi-bulk-data --example import-walkthrough` from a scratch directory: an unseeded database names no private document folder, so it keeps uploads under `./data/artifacts`. It migrates, installs the walkthrough fixture and, as its manager (`manager@example.test`, whose fixture role holds the import capabilities), imports 1,000 contacts through the contracts and a real Queue executor: a saved mapping reused, a dry run that writes nothing and reports errors and duplicates, the three duplicate policies and a row matching two contacts, affiliations finding their contacts by source key, a revoked grant, a worker stopped mid-run, a file refused for each reason, tenant isolation and the audit trail under the run's trace ID.

`cargo run -p bilibi-bulk-data --example export-walkthrough`, with the same variables, exports the fixture's contacts as its manager: the filtered rows and shown columns, link columns, a restricted field left blank and a formula-like name opened as text, refusals before it starts, the row limit failing it in words, a grant revoked before the job runs, ticked records only (another tenant's left out), another tenant refused, a file refused after a field is restricted or the grant revoked (and allowed again once restored), an expired file purged from disk before the next export and then shown expired, an export of only restricted columns refused, an export whose job was ended by a revoked grant finished by the next export (filter dropped, its person told), the size limit failing an export in words, a list renamed while it is read, the audit trail without filter values, the file's expiry and the person's notices.

`cargo run -p bilibi-bulk-data --example update-walkthrough`, with the same variables, changes 500 contacts' kind as the manager: refusals before it starts (no capability, a field not offered, a value outside the choices, more records than the limit), ticked records with another tenant's among them, every match with one contact edited meanwhile (reported, not overwritten) and the audit under the update's trace, already-set records, a grant revoked mid-run, a stop mid-run, a worker stopped mid-run (none written twice), another tenant refused, and no filter values in the audit trail.
