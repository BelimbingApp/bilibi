# Core Geonames

Global geographic reference data: countries, first-level divisions (Admin1),
postcodes and cities. No tenant column, no owner. Callers use `Geonames`
(registry service `core/geonames.lookup`); table layout and SQL stay private.

## Empty on install

The migrations (`migrations/001-geonames.sql`, `002-postcode-overrides.sql`)
create the four reference tables and the postcode-correction sidecar with **no
rows**. Every lookup answers with an empty result (`None`, `[]`, a zero-total
`Page`) until an operator imports; `Geonames::presence()` reports what is there
and `Presence::is_empty()` is the install state. There is no unconditional
seed: the import below is an explicit operational action. Address forms must
treat an empty lookup as "type it yourself", not an error.

## Import

```bash
cargo run -p bilibi-web -- geonames-import
cargo run -p bilibi-web -- geonames-import --datasets countries,admin1
cargo run -p bilibi-web -- geonames-import --datasets countries,admin1 --postcodes MY,SG
cargo run -p bilibi-web -- geonames-import --cache-dir /var/lib/bilibi/geonames --force
```

Countries, Admin1 and cities (`cities15000`) import by default, in that order.
**Postcodes are opt-in per country** (`--postcodes MY,SG`, two-letter ISO
codes; `ImportOptions::postcodes`): a country's file ranges from a few rows
to millions, so nothing imports postcodes unasked and no seed ever will. A
country must be imported before its postcodes; its rows are replaced wholesale
(GeoNames gives postcodes no upstream identity), a line naming another country
is skipped, and the operator's corrections (below) are materialized again
before the transaction commits. `ReferenceImport` (registry service
`core/geonames.import`) is the API: `run` for the operator command, `run_as`
for a screen, which checks `admin.geonames.update` on the sealed scope first.
A run reports one `DatasetReport` per `Target` (a dataset or one country's
postcodes), in that order.

- **Streaming, atomic per dataset.** Files are read line by line and written in
  500-row `unnest` upserts inside one transaction. A database failure in any
  batch rolls the whole dataset back (`ImportFailure::Database`); a file with no
  valid row is refused (`NoValidRows`) so a truncated payload never passes for
  an empty import. Blank, comment and malformed lines, and rows the schema
  would refuse, are counted as skipped. The run stops at the first failing
  dataset; datasets before it stay committed.
- **Local names survive.** A re-import refreshes every upstream fact but never
  replaces a country's `country` or an Admin1 `name`.
- **ETag cache.** Downloads go through `base/integration` (`stream`, recorded as
  system exchanges) into `--cache-dir`, default `GEONAMES_CACHE_DIR` or
  `$TMPDIR/bilibi/geonames`; use a durable directory in production. A cached
  file with an ETag is revalidated (`If-None-Match`, 304 keeps it); one without
  an ETag is trusted for 7 days with no request. A body lands in a temporary
  file renamed over the cache only after a 2xx. When the server is unreachable
  or answers 429/500/502/503/504, the existing cache is imported and the report
  says so with its age; `--force` re-downloads and never falls back.
- **Cache recovery.** Before each download the cached payload and ETag are
  copied aside; if the download, extraction or import fails, the known-good
  cache is restored and any extracted zip entry deleted.
- `--base-url` points at a mirror with GeoNames' `export/dump` layout;
  `--postcodes-url` at one with the `export/zip` layout (`<CC>.zip` holding
  `<CC>.txt`).

Geonames tables are reference data imported wholesale, so the import writes
outside mutation capture, as Bilimbi excludes these schemas from audit.

## Operator postcode corrections

`geonames_postcodes` stays the Belimbing-compatible source and lookup table
and carries no provenance. The `geonames_postcode_overrides` sidecar
(`src/corrections.rs`) stores the operator's desired record, a snapshot of the
upstream row it supersedes (every `source_*` column NULL for an
operator-created row) and a lock version. The desired record is materialized
into the compatible table, so `lookup_postcode` and `search_postcodes` keep
working unchanged, and `page_postcodes` reports each row's `provenance`
(`Geonames` or `Operator`) and opaque `revision`.

- `Geonames::create_postcode(scope, draft)` and
  `update_postcode(scope, id, revision, draft)` take the sealed scope and check
  `admin.geonames.update` at the moment of the call. Use them, not SQL into
  either table: they are the only path that keeps the sidecar, the compatible
  row and the audit trail together.
- `PostcodeDraft` is a partial form (`from_params` takes the present keys): a
  `None` field is left as it is, so an inline place-name correction sends only
  `place_name`; an empty string clears an optional field. Refusals come back
  as `PostcodeWriteError::Invalid(Vec<FieldError>)` in Ecto's words (`can't be
  blank`, `has invalid format`, `is not available`, `should be at most 20
  character(s)`, `must be less than or equal to 90`, …) so a form reads as
  Bilimbi's. The country and Admin1 must exist; `admin1_code` takes GeoNames'
  raw value (`14`) or the full identity (`MY.14`) and is stored raw, with
  `admin_name1`/`admin_code1` filled from the Admin1 table. Deliberate
  difference from Bilimbi: an Admin1 is resolved only when the draft names it
  or moves the record to another country, so a place-name correction of an
  upstream row whose admin code is not an Admin1 identity (GeoNames publishes
  such rows) is not refused and keeps that row's admin facts.
- `update_postcode` needs the `revision` the index returned; a stale one is
  `Stale` and changes nothing. The first correction of an upstream row
  snapshots it; later corrections bump the lock version.
- During a country refresh, overrides are selected by both original and
  desired country and locked. An upstream row that still matches its snapshot
  exactly is replaced in place; one that changed stays visible beside the
  operator's record, because Bilibi does not silently hide new upstream facts;
  an operator-created row is reinserted.
- The sidecar row is the operator's decision and goes through mutation
  capture (`base_audit_mutations`, auditable type `geonames_postcode_override`);
  the materialized compatible row and the import do not, as Bilimbi excludes
  the reference tables from audit.

## Lookups are bounded

- Page queries (`page_countries`, `page_admin1`, `page_postcodes`) take an
  untrusted `IndexQuery`: page sizes clamp to 25/50/100/300, search text to 255
  characters with `LIKE` metacharacters literal, sort keys resolve through closed
  enums to static SQL, and a malformed country filter matches nothing.
- Lists are capped (`MAX_LIST`); form lookups are capped too (postcode prefix
  10, city names 15, exact postcode places 100).
- Lookups are ungated global reads. Capability checks belong to the screens
  that call them (`admin.geonames.list` for the indexes).

## Catalog contributions

- Authz vocabulary: `admin.geonames.view`, `admin.geonames.list` (tenant) and
  `admin.geonames.update` (platform: it edits a table shared by every tenant);
  `tenant_owner` holds all three.
- Grid: table `countries` (global source, keyed by `iso`, read with `list`).
- Menu: the Administration › Geonames branch (`admin.geonames`, visible with
  `admin.geonames.list`) with Countries, Admin1 Divisions and Postcodes.
- Routes: `/geonames/countries`, `/geonames/admin1` and `/geonames/postcodes`
  (registry service `core/geonames.pages`), framed live pages behind
  `admin.geonames.list`.

## Screens

`src/web/` ports Bilimbi `Core.Geonames.Web.{CountriesLive, Admin1Live,
PostcodesLive}`. Each page keeps its search, sort and page in the URL under
Bilimbi's names (`search`, `sortBy`, `sortDir`, `page`, `perPage`; Admin1 adds
`filterCountryIso`, Postcodes `summarySortBy`/`summarySortDir`) through the
shared `ListState`, so Back and reload restore it, and draws its built-in
columns through `ListColumns`/`FlexTable` (add, remove, reorder, zoom and
reset without a Grid catalog). The route gate is `admin.geonames.list`;
`admin.geonames.update` only decides whether the write controls are drawn,
and every write asks for it again inside the API it calls. A refusal is a
flash, never a page-clearing error.

- **Countries**: ISO, name (inline edit → `update_country_name`), capital,
  phone, currency, population and updated date. Update (operators) runs
  `ReferenceImport::run_as` for the countries dataset in the background and
  reports what it did: a fresh or confirmed-current download counts as an
  update, a fallback to the cached file is a warning that says how old that
  data is, and a failed download or import says the data was not changed.
- **Admin1 Divisions**: country filter (only countries with divisions), code,
  name (inline edit → `update_admin1_name`), alt name, updated date. Update
  (operators; Belimbing's Admin1 `update`, absent from Bilimbi) runs the same
  background import for the Admin1 dataset: GeoNames' whole
  `admin1CodesASCII.txt`, every country's divisions, with the same report.
- **Postcodes**: the per-country totals card with its own sort, the index
  with a `Local` badge on operator rows, an inline place-name correction
  that carries the row's revision in the fact's id (`{id}|{revision}`), the
  Edit action per row and the New/Edit Postcode dialog. The dialog validates
  as values change (`check_postcode`), lists the chosen country's divisions
  as `Name (CC.code)`, and saves through `create_postcode`/`update_postcode`:
  refusals land beside their fields, a stale revision or vanished row closes
  the dialog with a flash and reloads the list. Import and Update
  (operators; Belimbing `Postcodes\Index::{import, update}`, absent from
  Bilimbi): Import opens the country picker (search, a checkbox grid with
  ISO codes, already-imported countries marked), and pressed again runs the
  ticked countries' postcodes through `ReferenceImport::import_as`, one
  country at a time in the background (`src/web/update.rs` `Run`), so the
  page shows which country is running and the rows so far; Update runs
  every imported country again. The finished run is one flash that names
  what was imported, what kept its existing data because the download
  failed, and what was not imported and why. A result lands on the page
  that started the run; leaving it loses the message, not the import.

Name updates write the reference tables outside mutation capture, like the
import, as Bilimbi excludes these schemas from audit; postcode corrections
are audited through the sidecar as described above.

## Running proof

```bash
DATABASE_URL=<disposable db> BILIBI_DISPOSABLE_GEONAMES_DEMO=yes \
  cargo run -p bilibi-geonames --example geonames-walkthrough
```

Shows the fresh-install behaviour, then lookups over a few hand-inserted rows,
hostile input and the Grid catalog entry.

```bash
DATABASE_URL=<disposable migrated db> BILIBI_DISPOSABLE_GEONAMES_DEMO=yes \
  cargo run -p bilibi-geonames --example geonames-import-walkthrough
```

Serves `sample/` (small GeoNames-format payloads, see its README) from a local
ETag fixture (`examples/support/fixture.rs`) and runs the importer through
first import, 304 revalidation, local-name preservation, 503/unreachable
fallback, `force`, a no-valid-rows payload, a database failure in a late batch,
the ETag-less cache lifetime and the capability gate.

```bash
DATABASE_URL=<disposable migrated db> BILIBI_DISPOSABLE_GEONAMES_DEMO=yes \
  cargo run -p bilibi-geonames --example geonames-postcodes-walkthrough
```

Imports countries, Admin1 and the opted-in `MY` postcodes from the same
fixture, then walks the corrections: the capability gate, a created row, a
draft refused with every field error, an inline place-name correction, a
stale revision, a refresh that materializes both corrections again, an
upstream change that appears beside the operator's record, a no-valid-rows
payload with the cache restored, a country not imported, a malformed
`--postcodes` and the sidecar's audit trail.

To try the command itself offline, zip `sample/cities15000.txt` as
`cities15000.zip` and `sample/MY.txt` as `MY.zip` beside the other two files,
serve that directory over HTTP and pass it as both `--base-url` and
`--postcodes-url`.
