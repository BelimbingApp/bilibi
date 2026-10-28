# Reports (`base/reports`)

Operator-built reports: a saved question over one Grid table, run as each reader. No Bilimbi counterpart.

## The rule

- A report stores a **definition**, never data. Every run reads through Grid with the reader's own `Scope`, so a reader sees only the rows and fields the record's own page would show them.
- Columns, groupings, measures and sorts the reader cannot see are **left out** and named in `left_out`.
- A **filter** on a column the reader cannot see **refuses the run** (`HiddenFilter`), so a filter cannot be used to infer a hidden value.

## Facade

`Reports` (service `base/reports.reports`). Reads take a pool connection; writes take the caller's `MutationTransaction`.

| Call | Needs | Does |
|---|---|---|
| `page`, `get` | `reports.view` | Reports the reader may run: their own, shared with everyone, or shared with one of their roles. `page` answers one page by name, searched by name and description in the same query, with the number that match. |
| `run` | `reports.view` | One page of rows (each with its record's own page where the owner declares one, and the totals line), or a grouped summary with subtotals and a total. Answers ask-when-run filters by position; `as_rows` with drill filters lists the records behind a group; agents drill as the page does (`equals`, `empty`, or a date bucket's `range` of days). |
| `csv` | `reports.export` | The whole result as CSV, up to the export cap, for the same `RunInput` as the page (answers, drill-downs, records view). Audited as `base.reports.exported` with report id and row count, never the data. Leading `=`, `+`, `-`, `@` are neutralised. |
| `record_types`, `columns` | `reports.view` | What a builder may offer: readable record types, and a type's own fields plus fields one link away. |
| `check` | — | Shape bounds, every column against the author's catalog, then a bounded dry run. A dry run refused only by size or cost still saves. |
| `create`, `copy` | `reports.author` | Saves a report owned by the actor. A copy is unshared. |
| `update`, `delete` | `reports.author`, owner | At `version`; a stale version is `Conflict`, decided before the write. |
| `share` | `reports.share`, owner | Everyone, chosen roles (at most 50), or nobody. Replaces the list. |

## Definition

| Kind | Uses |
|---|---|
| `rows` | `columns` (up to 32 Grid column paths), `filters`, `sort` (up to 3) |
| `summary` | `filters`, `groupings` (up to 2; a date column takes a `bucket`), `measures` (up to 8: `count`, `sum`, `avg`, `min`, `max`), `chart` |

- A rows report lists each column once (a repeat is refused when saved). With `totals` it adds a line summing each number column (not the key) over every row, on the page and in the CSV, one Grid read per eight columns; an empty result shows no totals line.

- Filter tests: `equals`, `one_of`, `contains`, `range`, `within` (a period such as `this_month`), `empty`, `not_empty`. A filter with `ask` takes the reader's value at run time; the stored value is the default, and an ask filter left blank is skipped.
- Text values from forms and agents are typed for the column at run time (numbers, yes/no).
- Dates group and filter in the company display zone (`bilibi_datetime::DisplayPolicy`).

## Settings (global, overridable per company; editor group "Reports", `reports.manage`)

| Key | Default | Range |
|---|---|---|
| `reports.page_size` | 50 | 10–100 (Grid's page bound) |
| `reports.max_groups` | 500 | 10–Grid's `MAX_RESULT_GROUPS` |
| `reports.export_rows` | 5,000 | 100–Grid's `MAX_OFFSET` |

Grid's cost bound and timeouts apply to every run on top of these.

## Pages

| Path | What |
|---|---|
| `/reports` | The reports the reader may run (`FlexTable`), with New report for authors: name, record type and kind create a starter report that opens on its Definition tab. |
| `/reports/{id}` | Read first. **Result** runs it as the reader, with ask-when-run answers, a left-out note, and drill-down: a group, a date bucket (as its period's first and last day), the total or a chart bar opens the records behind it, with a way back. **Definition** is the editor, live for the author, read-only for others. **Sharing** is the author's. Copy and Delete confirm with `ConfirmDialog`. |
| Dashboard | Customize offers every report the reader may run (provider `report`, embed `base/reports.dashboard`). A placed report runs as the reader with its stored answers and shows its chart, or its total as a number, and names any column the reader's access leaves out. It reruns at most once a minute, and sooner when the reader's access to it changes (`access`: its version and which of its columns their catalog resolves). |
| `/reports/{id}/export` | The CSV download (`reports.export`), with the answers, drill-downs and records view in the address (`a<i>`, `a<i>_to`, `d<n>`, `v<n>`, `rows`). |

- Answers, drill-downs, the tab and the page are in the address, so a link repeats the question. An ask box shows its stored value until answered; an answer left blank is no filter (`a<i>=` stays in the address), never the stored value.
- The editor and the Sharing form are drafts of the version they opened at: a save after someone else's is refused as a conflict, and the draft stays in the form until saved or discarded. Every filter, measure and sort a definition holds opens in the editor, and each field is read by name, so a definition at its bounds saves whole.
- A date value, and a range on a timestamp, is a day picker; an equals on a timestamp is its exact instant as text with its offset (`2025-01-31T09:30:00Z`), so a stored default survives editing other fields.
- Every write goes through the operation contracts below (`PAGE_WRITES`: `create`, `save_definition`, `copy`, `save_sharing`, `delete`).
- The menu entry Reports needs `reports.view`.

## Agent operations

Reads: `base.reports.list` (one page by name: `search`, `page` from 1, `page_size` up to 100, default 25; `meta.page.total` says how many match), `base.reports.get`, `base.reports.fields` (record types, or one type's column paths), `base.reports.run`. Reviewed writes: `base.reports.create`, `update`, `copy`, `share`, `delete`. Guide: `base.reports`.

## Storage

- `base_reports`: tenant, owner, name, description, record type, kind, definition (JSON, at most 32 KB), `shared_with_everyone`, `version`.
- `base_report_shares`: one row per role; it goes with its report or role.
- Audited as kind `base_report`.

## Proof

`apps/web/examples/r1-reports.rs` on a disposable database (`BILIBI_GRID_DEMO=disposable`): build, run against plain SQL, share, a field restriction, another tenant, stale version, copy, rows, CSV and its audit, totals, drilling into records, delete.

## Not yet

Matrix, PDF, queued exports and schedules (phase 2).
