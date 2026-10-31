# Reports (`base/reports`)

Operator-built reports: a saved question over one Grid table, run as each reader. No Bilimbi counterpart.

## Parent oversight

Set `definition.oversight: true` (Definition → **Read consenting child tenants**) to run a saved report through Grid's sealed, audited oversight facade. `fields` takes `oversight: true`; Rust `record_types` and `columns` take an explicit oversight boolean. Builders intersect the fields of consenting children granting each table; withheld fields, non-granted personal fields and child-user-bound sources are not offered. Execution independently rechecks every child. A hidden filter, grouping or sort refuses the question, rather than silently changing it.

Unclassified sources are also absent and refused, even with old or crafted grants: [Grid requires owner classification of the complete declaration](../../base/grid/README.md#parent-oversight), before masks or withholding. Core Contacts, accounts, affiliations, addresses and companies are unavailable until their owners complete classification and privacy/store contracts. This applies to saved reports, agents, panels and direct downloads as well as the builder; ordinary same-tenant reports are unchanged.

Summary results put the child first and combine totals over included children; averages use sums and counts. Matrices use the summary layout to keep child provenance. Row results include a Child tenant column, globally bounded pagination and no record links; aggregates-only grants contribute neither rows nor their totals. `Run.oversight` carries per-child grant versions and outcomes. Pages, panels and direct CSV/PDF downloads flag partial results; files include an Oversight status column. A changing cohort or row count during pagination refuses the download, never silently mixing it.

Oversight reads are live, read-only and bounded. Prepared exports and scheduled delivery are refused, including the background execution path: child data must not be retained in the parent after revocation. Ordinary tenant reports keep their existing export and scheduling behavior. Dashboard drawing revisions force a fresh oversight run rather than retaining child answers through a panel cache.

## The rule

- A report stores a **definition**, never data. Every run reads through Grid with the reader's own `Scope`, so a reader sees only the rows and fields the record's own page would show them.
- Columns, groupings, measures and sorts the reader cannot see are **left out** and named in `left_out`.
- A **filter** on a column the reader cannot see **refuses the run** (`HiddenFilter`), so a filter cannot be used to infer a hidden value.

## Facade

`Reports` (service `base/reports.reports`). Reads take a pool connection; writes take the caller's `MutationTransaction`.

| Call | Needs | Does |
|---|---|---|
| `page`, `get` | `reports.view` | Reports the reader may run: their own, shared with everyone, or shared with one of their roles. `page` answers one page by name, searched by name and description in the same query, with the number that match. |
| `run` | `reports.view` | One page of rows (each row linking to its record's page where the owner registers a Base Records kind for the table and the reader may open it, and the totals line), or a grouped summary or matrix with subtotals and a total. Answers ask-when-run filters by position; `as_rows` with drill filters lists the records behind a group; agents drill as the page does (`equals`, `empty`, or a date bucket's `range` of days). |
| `export` | `reports.export` | The whole result as CSV or a text PDF, up to the download bound, for the same `RunInput` as the page (answers, drill-downs, records view). Audited as `base.reports.exported` with report, format and row count, never the data. CSV neutralises a leading `=`, `+`, `-`, `@`; a PDF holds 20 columns and Latin-1 text and refuses anything else in words. A matrix file has the page's layout, column totals included. |
| `exportable` | `reports.export` | Whether that file can be made now, without making or auditing it: the page asks before sending the browser to a PDF, so a refusal is shown in words with CSV beside it. |
| `prepare_export`, `exports`, `exported` | `reports.export` | A larger result prepared on the Queue as the reader, stored as a private artifact of their company that expires after `reports.export_days`; the bell says when it is ready. Only the requester reads it, at `/report-exports/{id}`, and only while what they may see of the report (`access`: its version and which columns resolve) is what it was when the file was made; otherwise it is refused and prepared again. A request whose Queue job ended before it ran (the requester was refused) lists as failed. |
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

- A rows report lists each column once (a repeat is refused when saved), preserving its chosen order across unrelated editor saves. With `totals` it adds a labelled line summing each number column (not the key) over every row, on the page and in the file, one Grid read per eight columns; an empty result shows no totals line. The first unsummed cell labels the total; if every cell is summed, the page prefixes the first sum with "Total:" and files add a leading Row column so sums remain numeric.

- Filter tests: `equals`, `one_of`, `contains`, `range`, `within` (a period such as `this_month`), `empty`, `not_empty`. A filter with `ask` takes the reader's value at run time; the stored value is the default, and an ask filter left blank is skipped.
- Text values from forms and agents are typed for the column at run time (numbers, yes/no); integer equality, one-of and range bounds stay exact, including beyond JavaScript's safe integer range.
- Dates group and filter in the company display zone (`bilibi_datetime::DisplayPolicy`).

## Settings (global, overridable per company; editor group "Reports", `reports.manage`)

| Key | Default | Range |
|---|---|---|
| `reports.page_size` | 50 | 10–100 (Grid's page bound) |
| `reports.max_groups` | 500 | 10–Grid's `MAX_RESULT_GROUPS` |
| `reports.export_rows` | 1,000 | 100–Grid's `MAX_OFFSET` (a larger list is prepared instead) |
| `reports.queued_export_rows` | Grid's `MAX_OFFSET` | 100–Grid's `MAX_OFFSET` (a prepared export pages through Grid too) |
| `reports.export_days` | 7 | 1–90 (never past `artifacts.retention_days`) |
| `reports.max_subscriptions` | 20 | 1–200 per person |
| `reports.statement_ms`, `reports.max_cost` | 2,000 ms, 100,000 (Grid's own bound) | 500–30,000 ms; 1,000–Grid's `MAX_COST_BOUND` |
| `reports.queued_statement_ms`, `reports.queued_max_cost` | 60,000 ms, 10,000,000 | 1,000 ms–Grid's `MAX_STATEMENT_MS`; 1,000–`MAX_COST_BOUND` |

A page, a dashboard panel, a download and an agent run read under the interactive pair; a prepared export and a scheduled delivery under the background pair (`RunInput::background`). Every read of a run takes the pair: a list's keys, the cells that fill them (`grid::project_within`) and its totals. A report over its cost or time bound is refused as `TooHeavy`, which says to prepare it as an export instead, and the Result tab offers Prepare CSV/PDF right there instead of the downloads, which read under the same interactive bounds. Too many groups is refused with "add a filter", since the background has the same group limit.

The settings editor groups the two pairs as **Reports · Interactive reads** and **Reports · Background reads**, using its Area filter. A prepared export above its row cap names that cap and asks for a filter, not another prepared export.

## Pages

| Path | What |
|---|---|
| `/reports` | The reports the reader may run (`FlexTable`), with New report for authors: name, record type and kind create a starter report that opens on its Definition tab. |
| `/reports/{id}` | Read first. **Result** runs it as the reader, with ask-when-run answers, a left-out note, and drill-down: a group, a date bucket (as its period's first and last day), the total or a chart bar opens the records behind it, with a way back. Below it, Prepare CSV/PDF and the reader's own exports. **Definition** is the editor, live for the author, read-only for others. **Schedule** holds the reader's own deliveries. **Sharing** is the author's. Copy and Delete confirm with `ConfirmDialog`. |
| `/built-in-reports/{key}` | A built-in report: Result and a read-only Definition, and Copy. |
| `/report-exports/{id}` | A prepared or delivered export, for its requester only, until it expires. |
| Dashboard | Customize offers the first 100 unplaced reports the reader may run, by name (provider `report`, embed `base/reports.dashboard`); outside Customize only placed reports are read. Each provider call shares one fresh reader catalog across their access revisions (report version and which columns resolve), and a changed revision remounts the panel without its cached HTML. A placed report runs as the reader with its stored answers and shows its chart, or its total as a number, and names any column their access leaves out. It reruns at most once a minute unless its revision changes. Hidden pins survive intervening arrangement edits and return when access does. |
| `/reports/{id}/export` | The CSV download, or a PDF with `format=pdf` (`reports.export`), with the answers, drill-downs and records view in the address (`a<i>`, `a<i>_to`, `d<n>`, `v<n>`, `rows`). |

- Answers, drill-downs, the tab and the page are in the address, so a link repeats the question. An ask box shows its stored value until answered; an answer left blank is no filter (`a<i>=` stays in the address), never the stored value.
- The editor and the Sharing form are drafts of the version they opened at: a save after someone else's is refused as a conflict, and the draft stays in the form until saved or discarded. Every filter, measure and sort a definition holds opens in the editor, and each field is read by name, so a definition at its bounds saves whole. A `one_of` filter is typed as comma-separated text, with `"quotes"` around a value that holds a comma (`"North, East", West` is two values), so a saved list reopens and saves unchanged. Sharing's Discard changes is a submit of its form, so the browser resets the controls with the server's draft; Enter in the editor saves through the form's `data-submit-event`.
- A date value, and a range on a timestamp, is a day picker; an equals on a timestamp is its exact instant as text with its offset (`2025-01-31T09:30:00Z`), so a stored default survives editing other fields.
- Every write goes through the operation contracts below (`PAGE_WRITES`: `create`, `save_definition`, `copy`, `save_sharing`, `delete`, `prepare_export`, `subscribe`, `pause_schedule`, `unsubscribe`).
- The menu entry Reports needs `reports.view`.

## Agent operations

Reads: `base.reports.list` (one page by name: `search`, `page` from 1, `page_size` up to 100, default 25; `meta.page.total` says how many match; `built_in` lists the built-in ones), `get` and `run` (by `id`, or `builtin` key), `fields` (record types, or one type's column paths), `exports`, `subscriptions`. Reviewed writes: `create`, `update`, `copy` (also of a built-in), `share`, `delete`, `prepare_export`, `subscribe`, `pause_subscription`, `unsubscribe`. Guide: `base.reports`.

## Schedules

- A subscription keeps the workplace where it was created and delivers one report there daily, weekly (a weekday) or monthly (day 1–28) at a wall-clock time in the subscriber's display zone, as CSV or PDF, to the subscriber and, with `reports.share`, to the holders of chosen roles.
- Schedule's recurrence `base/reports.deliveries` (every 15 minutes, the `scheduler` principal) sweeps due subscriptions: one delivery per recipient (unique per due time), audited in each tenant as `scheduler`. Like every contributed recurrence it runs nothing until an operator enables it on `/system/schedule` and grants the `scheduler` principal `admin.system.schedule.execute`; see [Schedule setup](../schedule/README.md).
- Each delivery is queued as its recipient in the subscription's workplace (`base/reports-deliver`, re-proved by Queue on every attempt) after their account, desk, `reports.view` and `reports.export` prove (a delivered file is an export, downloaded like one). It runs the report under their own access, keeps the file as their prepared export, rings their bell and mails a link that needs sign-in (`APP_ORIGIN`). The mail carries no data.
- Nothing is sent, and the delivery records why, when the recipient's account no longer proves, they lost their desk in the subscription's workplace, they lost `reports.view` or `reports.export`, the report is no longer shared with them, a filter hides a field from them, or no record matched and the subscription skips empty results (decided before the file is stored or the bell rings; an ungrouped summary is one line however many records it counts). Pausing cancels deliveries not yet started. A delivery whose job Queue ended without running it is closed as skipped by the next sweep. Every delivery's creation, export, cancellation and removal is audited.

## Built-in reports

A module contributes one in code with `bilibi_reports::builtin!` (key, name, description, record type, kind, definition in the agent dialect); boot refuses an invalid or duplicate one. It is offered, read-only and copyable, to a reader whose catalog includes its record type, and runs as that reader. Nothing is stored or seeded. Core Company contributes `companies-by-status`.

## Storage

- `base_reports`: tenant, owner, name, description, record type, kind, definition (JSON, at most 32 KB), `shared_with_everyone`, `version`.
- `base_report_shares`: one row per role; it goes with its report or role.
- `base_report_exports`: a prepared export's request and outcome (its artifact, file name, row count or why it failed, its Queue job, and what the requester could see when it was made), read only by its requester.
- `base_report_subscriptions`, `base_report_subscription_roles`, `base_report_deliveries`: workplace-bound schedules, their extra recipients, and each delivery's state, reason and Queue job.
- Audited as kind `base_report`.

## Proof

`apps/web/examples/r1-reports.rs` on a disposable database (`BILIBI_GRID_DEMO=disposable`): build, run against plain SQL, share, a field restriction, another tenant, stale version, copy, rows, CSV and PDF and their audit, totals, drill into records, matrix, built-ins, a prepared export's queue job, and a subscription's next run, pause and removal.

## Not yet

Computed columns, XLSX, mail attachments, a read replica and owner summary tables are later issues (#128, "Out of scope" and "Reporting store").
