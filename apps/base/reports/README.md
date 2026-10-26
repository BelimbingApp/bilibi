# Reports (`base/reports`)

Operator-built reports: a saved question over one Grid table, run as each reader. Decision: `docs/architecture/decisions/06-report-builder-reads-owner-sources.md`. No Bilimbi counterpart.

## The rule

- A report stores a **definition**, never data. Every run reads through Grid with the reader's own `Scope`, so a reader sees only the rows and fields the record's own page would show them.
- Columns, groupings, measures and sorts the reader cannot see are **left out** and named in `left_out`.
- A **filter** on a column the reader cannot see **refuses the run** (`HiddenFilter`), so a filter cannot be used to infer a hidden value.

## Facade

`Reports` (service `base/reports.reports`). Reads take a pool connection; writes take the caller's `MutationTransaction`.

| Call | Needs | Does |
|---|---|---|
| `list`, `get` | `reports.view` | Reports the reader may run: their own, shared with everyone, or shared with one of their roles. |
| `run` | `reports.view` | One page of rows (each with its record's own page where the owner declares one, and the totals line), or a grouped summary or matrix with subtotals and a total. Answers ask-when-run filters by position; `as_rows` with drill filters lists the records behind a group. |
| `export` | `reports.export` | The whole result as CSV or a text PDF, up to the download bound. Audited as `base.reports.exported` with report, format and row count, never the data. CSV neutralises a leading `=`, `+`, `-`, `@`; a PDF holds 20 columns and Latin-1 text and refuses anything else in words. |
| `prepare_export`, `exports`, `exported` | `reports.export` | A larger result prepared on the Queue as the reader, stored as a private artifact of their company that expires after `reports.export_days`; the bell says when it is ready. Only the requester reads it, at `/report-exports/{id}`. |
| `subscribe`, `subscriptions`, `pause`, `unsubscribe` | `reports.schedule` (roles: `reports.share` too) | Scheduled deliveries; see Schedules. |
| `builtins`, `builtin`, `run_builtin`, `copy_builtin` | `reports.view` / `reports.author` | Built-in reports modules contribute; see Built-in reports. |
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
| `matrix` | `filters`, exactly 2 `groupings` (down, across; at most 24 values across are drawn as a matrix), at most 1 measure (else the record count), `chart` |

- A rows report with `totals` adds a line summing each number column (not the key) over every row.

- Filter tests: `equals`, `one_of`, `contains`, `range`, `within` (a period such as `this_month`), `empty`, `not_empty`. A filter with `ask` takes the reader's value at run time; the stored value is the default, and an ask filter left blank is skipped.
- Text values from forms and agents are typed for the column at run time (numbers, yes/no).
- Dates group and filter in the company display zone (`bilibi_datetime::DisplayPolicy`).

## Settings (global, overridable per company; editor group "Reports", `reports.manage`)

| Key | Default | Range |
|---|---|---|
| `reports.page_size` | 50 | 10–200 |
| `reports.max_groups` | 500 | 10–Grid's `MAX_RESULT_GROUPS` |
| `reports.export_rows` | 1,000 | 100–Grid's `MAX_OFFSET` (a larger list is prepared instead) |
| `reports.queued_export_rows` | Grid's `MAX_OFFSET` | 100–Grid's `MAX_OFFSET` (a prepared export pages through Grid too) |
| `reports.export_days` | 7 | 1–90 (never past `artifacts.retention_days`) |
| `reports.max_subscriptions` | 20 | 1–200 per person |

Grid's cost bound and timeouts apply to every run on top of these.

## Pages

| Path | What |
|---|---|
| `/reports` | The reports the reader may run (`FlexTable`), with New report for authors: name, record type and kind create a starter report that opens on its Definition tab. |
| `/reports/{id}` | Read first. **Result** runs it as the reader, with ask-when-run answers, a left-out note, and drill-down: a group, a date bucket (as its period's first and last day), the total or a chart bar opens the records behind it, with a way back. Below it, Prepare CSV/PDF and the reader's own exports. **Definition** is the editor, live for the author, read-only for others. **Schedule** holds the reader's own deliveries. **Sharing** is the author's. Copy and Delete confirm with `ConfirmDialog`. |
| `/built-in-reports/{key}` | A built-in report: Result and a read-only Definition, and Copy. |
| `/report-exports/{id}` | A prepared or delivered export, for its requester only, until it expires. |
| Dashboard | Customize offers every report the reader may run (provider `report`, embed `base/reports.dashboard`). A placed report runs as the reader with its stored answers and shows its chart, or its total as a number; it refreshes at most once a minute. |
| `/reports/{id}/export` | The CSV download, or a PDF with `format=pdf` (`reports.export`), with the answers in the address (`a<i>`, `a<i>_to`). |

- Answers, the tab and the page are in the address, so a link repeats the question.
- Every write goes through the operation contracts below (`PAGE_WRITES`: `create`, `save_definition`, `copy`, `save_sharing`, `delete`, `prepare_export`, `subscribe`, `pause_schedule`, `unsubscribe`).
- The menu entry Reports needs `reports.view`.

## Agent operations

Reads: `base.reports.list` (with `built_in`), `get` and `run` (by `id`, or `builtin` key), `fields` (record types, or one type's column paths), `exports`, `subscriptions`. Reviewed writes: `create`, `update`, `copy` (also of a built-in), `share`, `delete`, `prepare_export`, `subscribe`, `pause_subscription`, `unsubscribe`. Guide: `base.reports`.

## Schedules (ADR 06, decision 4)

- A subscription delivers one report daily, weekly (a weekday) or monthly (day 1–28) at a wall-clock time in the subscriber's display zone, as CSV or PDF, to the subscriber and, with `reports.share`, to the holders of chosen roles.
- Schedule's recurrence `base/reports.deliveries` (every 15 minutes, the `scheduler` principal) sweeps due subscriptions: one delivery per recipient (unique per due time), audited in each tenant as `scheduler`. Like every contributed recurrence it runs nothing until an operator enables it on `/system/schedule`.
- Each delivery is queued as its recipient (`base/reports-deliver`, re-proved by Queue on every attempt) after their account and `reports.view` prove. It runs the report under their own access, keeps the file as their prepared export, rings their bell and mails a link that needs sign-in (`APP_ORIGIN`). The mail carries no data.
- Nothing is sent, and the delivery records why, when the recipient's account no longer proves, they lost `reports.view`, the report is no longer shared with them, a filter hides a field from them, or the result is empty and the subscription skips empty results. Pausing cancels deliveries not yet started.

## Built-in reports

A module contributes one in code with `bilibi_reports::builtin!` (key, name, description, record type, kind, definition in the agent dialect); boot refuses an invalid or duplicate one. It is offered, read-only and copyable, to a reader whose catalog includes its record type, and runs as that reader. Nothing is stored or seeded. Core Company contributes `companies-by-status`.

## Storage

- `base_reports`: tenant, owner, name, description, record type, kind, definition (JSON, at most 32 KB), `shared_with_everyone`, `version`.
- `base_report_shares`: one row per role; it goes with its report or role.
- `base_report_exports`: a prepared export's request and outcome (its artifact, file name, row count or why it failed), read only by its requester.
- `base_report_subscriptions`, `base_report_subscription_roles`, `base_report_deliveries`: schedules, their extra recipients, and each delivery's state and reason.
- Audited as kind `base_report`.

## Proof

`apps/web/examples/r1-reports.rs` on a disposable database (`BILIBI_GRID_DEMO=disposable`): build, run against plain SQL, share, a field restriction, another tenant, stale version, copy, rows, CSV and PDF and their audit, totals, drill into records, matrix, built-ins, a prepared export's queue job, and a subscription's next run, pause and removal.

## Not yet

Computed columns, XLSX, mail attachments, a read replica and owner summary tables are later issues (#128, "Out of scope" and "Reporting store").
