# ADR 05: Report builder: reports read owners' Grid sources under bounds

**Document Type:** Architecture Decision Record
**Status:** Proposed. The first slice is built: Grid's grouped read and filtered row page (`summarize`, `page_keys`). The rest is in "Slices".
**Agents:** claude/claude-opus-5-5 (author)
**Scope:** Where operator-built reports read their data, how they stay inside each reader's permissions and the database's bounds, and how scheduled deliveries act for their recipients
**Last Updated:** 2026-10-22

> The `Agents` value is self-reporting. Only known parts are included.
>
> Source: [issue 128](https://github.com/BelimbingApp/bilibi/issues/128). No Bilimbi counterpart. Builds on Grid (G1–G4), Dashboard, Queue, Schedule and Artifacts, and on [ADR 01](./01-agent-first-operations.md) for agent access.

## Context

- Business Domains need reports as soon as they hold data. A CRM Domain, for example, needs open opportunities by stage and owner, and cases by priority and age. Without a report builder each question becomes a hand-built page or dashboard panel.
- Grid already lists every module's tables, fields, links, owning capability, time field and tenant-scoped source, and prunes tables and fields by grant and field restriction (`Catalog::for_scope`). It reads a page of rows (`project`) and a whole-table min and max (`stats`) under bounds. It had no grouped read and no filtering or sorting on walked columns.
- A separate reporting database would duplicate every owner's visibility rules and could drift from them.

## Decision

### 1. A report is a saved question, read from the owners' Grid sources

- A report stores a record type (a Grid table), columns, filters, groupings, totals, sort and chart. It never stores result rows.
- Every run, preview, dashboard refresh, export and scheduled delivery reads through Grid under the **reader's own `Scope`**, against a fresh scoped catalog. A table the reader cannot read, or a restricted field, is dropped from the result and the builder, and the page says which columns were left out.
- A filter, grouping, measure or sort on a column the reader cannot see is refused like an unknown column, so it cannot be used to infer a hidden value.
- Reportable record types are the Grid tables modules already contribute. Nothing new is registered. An owner keeps a field out with Grid's existing `.excluded("reason")`.

### 2. Grid gains two bounded reads with `project`'s rules

| Read | What it returns |
| --- | --- |
| `summarize(db, scope, root, spec)` | Up to 2 groupings (a date or date-time by day, week, month, quarter or year in the reader's zone), up to 8 measures (`count`, `sum`, `avg`, `min`, `max`), a line per group, a subtotal per first-level group and a total |
| `page_keys(db, scope, root, spec)` | One page (1–100) of root keys, filtered and sorted (up to 3 sorts, linked columns included, the key breaking ties), and the matching total, for `project` to fill |

- Both use a fresh `Catalog::for_scope`, a read-only repeatable-read transaction, the owner's source with its tenant and user bindings on the root and every link, the `EXPLAIN` admission bound and the same statement and lock timeouts as `project`.
- Specs are parsed column paths and typed values. Only catalog identifiers enter SQL; every value is a bind parameter.
- Filters: equals, one of, contains (text), number range, day range, relative period (today, this week, last 7 or 30 days, this month, quarter or year) and empty or not. A date-time compares against the instants its local days start and end at, so the owner's index on the instant still bounds the read.
- A summary over the group cap is refused with a clear message. It is never silently truncated.

### 3. No separate reporting store until measurements ask for one

Reports read the primary database inside the bounds. A store is added only in these steps, each opened only when `base/perf` samples show reports hurting everyday work:

1. Owners index their sources and join keys, as Grid already asks.
2. An owner keeps its own summary table (with `tenant_id`), updated in its own transaction or by its own Events subscriber, and declares it as another Grid table.
3. A read-only replica connection for report reads and queued exports. Pages say "data as of" when it lags. Writes and permission checks stay on the primary.
4. Materialised views only where the reading capability is tenant-wide, behind a Grid source that applies the base table's visibility.
5. A separate warehouse or an external BI feed is a later decision with its own record.

### 4. Scheduled deliveries act as each recipient

Scheduled work runs as the `scheduler` system principal, which has no `Scope` and reads nothing. A delivery therefore runs as a Queue job whose actor comes from a stored, audited subscription and is re-proved on every attempt, as Queue already does for user jobs. A recipient who has lost access gets nothing, and the run records why.

## Slices

| Slice | State |
| --- | --- |
| Grid grouped read and filtered row page (`summarize`, `page_keys`) | **Built** (`apps/base/grid/src/summary.rs`, walkthrough `g5-grid-summary`) |
| Shared server-rendered SVG chart component in `base/ui` | Not built |
| `base/reports` module: definitions, sharing, builder and read pages, CSV export, agent operations | Not built |
| Dashboard provider hook for report panels | Not built |
| Phase 2: matrix, queued exports, PDF, schedules, module-contributed reports | Not built |

## Consequences

- Reports can never show more than the record's own page shows the same reader, because they read through the same sources and catalog.
- Heavy reports are refused by the cost bound rather than slowing everyone. The answer to a refused report is a queued export (phase 2) or an owner-maintained summary table, not a larger bound.
- Grouping or filtering by an expression over many rows cannot use a plain index. The cost bound and timeout cap that; owners index what is reported often.
- Pick-lists (issue 129) and custom fields (issue 131) need no change here. Pick-list values are stable codes that group and filter like any value. Custom fields reach reports when Grid's scoped catalog includes them, because reports always read the live catalog.
