# Grid catalog, page projections, lenses and page columns (G1–G4)

Grid owns flexible columns on list pages: the catalog every module declares its
tables into, the paths a person walks through it to pick columns without writing
a join, the one projection statement that answers them for the rows a list
already shows, and what each account last arranged on each list. It has no page
of its own. G1 is the catalog, the scoped view of it, and the bounded
column-path parser. Query/rollups are implemented by G2, lenses by G3, and the
per-page arrangement a list page hosts (`PageColumns`, `View`, `PageViews` and
the `ui.grid.page_columns` setting) by G4.

## Contributing a table

Depend on `base/grid`, submit one `GridContribution` to `TABLES`
(`bilibi_grid::contribute!("core/x", "x.grid", &GRID)`), and declare:

- a `TableDecl`: stable lowercase `id` (unique across modules), the tenant
  `capability` that reads any of its rows, a `Source`, the `key` field, optional
  `label_field` and `time_field` (a date/datetime), `fields`, and `record_types`
  (the audit `auditable_type` values its rows use, so a restriction reaches their
  audit views; one table claims a type).
- a `Source`: static SQL for **the rows a scope may read**, with the owner's own
  tenant, soft-delete and company filters applied. A tenant source takes bigint
  `$1` (tenant) and may take `$2` (user); a `Source::global` takes none. The grid
  wraps it in a subquery, so every hop of a path carries its owner's boundary and
  Grid never adds a tenant predicate. It must be one `SELECT`/`WITH` statement
  that references `$1` exactly when it is tenant-scoped.
- `Field`s: `Field::new`/`choice`; `column` names the source key when it differs
  from the id; `hidden` fields exist only for joins; `protected` keeps a field
  out of operator restriction (the key, label, time, hidden and join fields are
  protected implicitly).
- `LinkDecl`s: `One` (a foreign key to the target's **key**, or an edge yielding at
  most one row per parent) or `Many` (the reverse, or an attachment table), joined
  `on` two declared fields or through an edge `Source` selecting `from_key` and
  `to_key`. A link may start at another module's table, because the module that
  owns the edge knows it; it must own one end.

Contributions are validated at discovery (duplicate ids, unknown link ends, bad
joins, a link id that shadows a field, wrong source shape, claimed record types)
and the Ready check refuses a table naming a capability Authz does not declare.
Nothing executes a source at boot.

## The scoped catalog

`Catalog::for_scope(client, &scope)` reads fresh grants and field restrictions
(`authz::effective_capabilities`, `authz::fields::restricted`) and keeps only the
tables the actor may read and, within them, only the fields no held role
restricts: any restricting role masks, even alongside grant-all. A link to a
table the actor cannot read is dropped, so such a path is refused exactly like an
unknown one. Platform capabilities need the operator identity. A system identity
has no `Scope` and reads nothing. Build the catalog again wherever the page
refreshes its own capabilities; there is no cache of grants.

The first catalog for a source asks PostgreSQL to describe (not run) its
statement and keeps the selected keys for the process, since the SQL is static.
A declared field its source does not select, a placeholder that is not bigint, or
an edge without `from_key`/`to_key` fails with `CatalogError::Misdeclared`
naming the table and field (the owner must fix it; callers' `?` maps it to an
internal error and logs the cause).

`restriction_catalog` and `restrictable` give the Authz screens the protected-field
catalog as **data**: Authz does not depend on Grid.

## Column paths

`name`, `group.name` (one-links), `movements:count`, `movements.delta:sum`,
`group.items.name:list` (a many-link rolls up; one-links may follow it).
`column::parse` accepts at most 160 bytes, 8 lowercase `[a-z][a-z0-9_]*` segments
and one known aggregate (`count sum avg min max latest list`); anything else is
`Malformed` before any catalog is consulted. A path holds at most one many-link
and must then end in an aggregate; `count` takes no field; `sum`/`avg` need a
number, `min`/`max` an ordered value, `latest` a table with a time field.
`resolve_all` collapses duplicates and accepts at most 32 columns. `suggest` ranks
reachable columns (paths up to three links, 50 results at most, 4096 candidates
at most).

## Reference tables

`reference_groups`, `reference_items` and `reference_movements` (tables
`base_grid_ref_*`, capability `grid.reference.view`) are non-business stock data
Grid owns so later slices have a catalog to run against: a one-link, many-links, a
chained one-link after a many-link, an enum, decimals, dates, and a restrictable
field (`reference_items.unit_cost`). Managers, viewers and staff gain the
capability through the contributed role extension; run `authz-reconcile` after
deploying. The tables are empty unless a walkthrough or fixture fills them.

## Running evidence

No unit tests. On a disposable migrated database:

```sh
BILIBI_GRID_DEMO=disposable cargo run -p bilibi-web --example g1-grid
```

It fills reference rows (tenants 1 and 2), shows catalogs differ by capability,
direct deny, platform gating and role revocation; resolves valid and refused paths
(typed errors); feeds the parser hostile input; ranks suggestions; hides
`unit_cost` through a role restriction (including an admin holding that role,
revoked live) and shows a stale restriction on a protected field is inert; runs
the source with `$1` for two tenants; and installs deliberately defective
contributions to show discovery refuses each. It removes what it added.

## Page projections (G2)

Call `bilibi_grid::project(&db, &scope, "reference_items", &ids, &specs)` after
paging through the owner's list API. IDs are JSON number/string values; specs
are paths such as `group.name` or `movements.delta:sum`. Do not build SQL or pass
previously resolved `Column`s: `project` builds a fresh catalog and resolves specs
in its own read-only, repeatable-read transaction. Owner sources on the root,
every link target and every edge receive the same tenant/user bindings. The host
still validates the live session/actor before calling the module API.

The result has resolved typed `columns`, page-ordered `rows` (`key` plus `cells`
keyed by column DOM id), and PostgreSQL's `estimated_cost`. Duplicate IDs/specs
collapse in first-seen order; missing and unauthorized roots disappear. IDs are
cast to the server-described native key type, preserving indexed lookups and
supporting bigint, UUID and text owners without request-derived SQL identifiers.
Numbers and booleans remain JSON numbers and booleans. Dates/datetimes are ISO
strings paired with the column's `FieldType`. Missing fields are JSON null;
counts over empty paths are zero; other empty aggregates are null. `latest`
orders by date descending, null dates last, then key descending for stable ties.
`list` sorts distinct text values and caps the cell at 300 characters.

Each column uses a correlated scalar subquery rather than multiplying root rows
across independent many-links. One-links enforce at-most-one visible target:
a broken declaration fails the whole read, including inside aggregates. Edges
use set membership so duplicate edge pairs cannot inflate counts. Tail one-links
retain a many row whose target is absent, matching LEFT JOIN semantics.

Bounds are 100 page IDs (each scalar at most 256 encoded bytes), 32 input specs,
128 total hops and the G1 parser's depth/length limits. An `EXPLAIN (FORMAT JSON)`
checks total estimated cost against 100,000 before the projection executes.
Every statement runs with a transaction-local 2-second statement timeout and
500ms lock timeout; commit/rollback restores the pooled connection's settings.
The estimate is an admission check, not a promise about runtime; the timeout is
the execution backstop. Owners must still index their sources and join keys.

The query returns typed `QueryError`s and converts to the shared database error
for page handlers. It does not sort/filter walked columns or persist layouts (G4).

Run the live projection walkthrough against a **disposable** migrated
PostgreSQL database:

```sh
BILIBI_GRID_DEMO=disposable cargo run -p bilibi-web --example g2-grid
```

It replaces the non-business reference fixture, prints real projections for all
seven aggregates, one-links, many-links and tails, two actor scopes, cross-tenant
IDs and an empty group; applies/revokes restrictions and grants; exercises scoped
edges and global text keys; then demonstrates cardinality refusal, input bounds,
cost rejection, timeout and a successful projection after failure. It removes its
reference rows on success. No unit tests or test runner are involved.

## Searching related text

`query::text_search_sql(client, scope, root, paths)` builds a membership subquery for an owner's paginated search and count. Execute it immediately under the same scope, binding tenant, user and normalized search text to `$1`, `$2` and `$3`. The owner retains its pagination and statement timeout. Paths resolve against a fresh catalog; unavailable tables and restricted fields contribute no matches. Many-links search every text value before cell truncation, so a value beyond the displayed rollup still matches.

## Lenses (G3)

A lens is how a column's values are read: `value`, `bar` (scaled to the
column's range), `band` (a colour band), and, for a `count`/`sum` over a dated
many-link with no tail (`lens::trends`), `trend` (twelve calendar months as a
sparkline) and `delta` (the change since a date). `lens::available` and
`lens::normalize` decide what a column offers; `lens::flex_column` builds its
FlexTable column (walked columns never sort; numeric ones align right).

A host reads lenses in three steps, as Bilimbi's `PageColumns` does:

1. `project_lenses(&db, &scope, root, &ids, &specs, &Lenses { trend, delta, since })`
   is `project` plus, per row, `series` (the aggregate for each of
   `Projection::months`, a month without rows counting zero) and `before` (the
   aggregate over rows dated strictly before `since`, default thirty days
   back). They are correlated subqueries in the same bounded statement.
   Months and dates are UTC calendar days: every Grid read sets `TimeZone` to
   UTC locally.
2. `stats(&db, &scope, root, &specs)` only for columns worn as a bar or band:
   min/max of each numeric column over **every** row the scope may read, with
   the planner estimate. Above the 100,000 admission bound it is `skipped`
   (cells then draw without a position); `lens::cost` turns the estimate into
   the FlexTable's heavy-query warning above 20,000.
3. `lens::cell(value, column, range, lens, Extras { series, before }, &display)`
   per cell. Pass the request's `DisplayContext`: a date becomes a zone-free
   `Typed::Date` (`dd/mm/yyyy`) and a timestamp a `Typed::Instant` with both
   server clock texts, so a clock change re-renders it through every lens. Do
   not format a date into text yourself. Numbers position on the range (dates
   as days, timestamps as seconds; equal ends are 0.5), sequential bands are
   `min(trunc(n*5), 4)`, text/enum/list values band categorically by Erlang
   `phash2` (`bilibi_ui::flex_table::category`) and booleans binary, exactly
   as Bilimbi's `Lens.cell`. Decimals and floats show two places; the delta
   lens appends the signed change (`+3`, `−2.50`, `±0`).

The reference movements carry `moved_on` (date, the time field) and
`recorded_at` (timestamptz) so every typed path has data.

Run the live lens page against a **disposable** migrated database
(sign in as the fixture manager after the example reconciles roles):

```sh
BILIBI_GRID_DEMO=disposable DATABASE_URL=... APP_ORIGIN=... BIND_ADDR=... \
  cargo run -p bilibi-web --example g3-lenses
```

It replaces the reference fixture with twelve tenant-1 items and fourteen
months of movements relative to today, then serves `/g3-lenses`: switch a
column's lens from its chip, set the change-since date, step the row height and
flip the timestamp clock; each reply re-projects for the signed-in scope.
`G3_DUMP=<file>` first writes every cell through every lens it allows, with
its inputs, as JSON lines for comparison against Bilimbi's `Lens.cell`.

## Page columns (G4)

`PageColumns` is the host a list page composes, as Bilimbi's
`Grid.Web.PageColumns` is: the page keeps its own query, filters, sort and
pagination, declares the columns it draws itself as `BuiltinColumn`s, and
`PageColumns` adds the walked columns for exactly the rows the page listed.
Removed default columns appear first in the picker, including related columns.

Do not host walked columns through `ListColumns` plus your own projection:
`PageColumns` owns the catalog pruning, the memory and the URL shape.

The page holds one `PageColumns` in its live state and:

1. builds it at mount with `PageColumns::mount(views, table, builtins)`, where `views` is the `base/grid.page_views` service (`PAGE_VIEWS`) and `table` is the catalog table its rows come from (the page's name in memory); optionally chain `.with_defaults(&["name", "group.name"])` when related columns should appear by default;
2. calls `from_params(scope, query)` on render, mount and every params
   update, and merges `params()` into the query it returns as
   `LiveBody.query`, so the address carries what is shown;
3. after paging its own rows, calls `load(scope, keys, display)` with the
   rows' keys as JSON scalars and the request's `DisplayContext`;
4. renders `flex_table(id, rows)` over `rows` (a clone of
   `PageColumns::rows`, each row's slots filled for the built-in ids);
5. delegates its `grid` event to `handle(scope, values)`: after
   `Outcome::Patch` it loads again and returns the merged query; `Sort(key)`
   is a built-in's sort for the page's `ListState`; `Update` re-renders.

`View` is what a person arranged, as one value that round-trips through the
URL (`cols`, `lens`, `z`, `since`; defaults left out) and the stored JSON
(`columns`, `lenses`, `zoom`, `since`). An address that carries any of those
keys wins over memory, so a shared link shows what its sender saw and changes
nobody's memory until the reader arranges something. An address that carries
none opens the page the way the account left it (`PageViews::fetch`, read
afresh every time, never from a copy held in the page), and as the page's own
columns when the account arranged nothing.

`PageViews` keeps the arrangement per account and per page in the
user-scoped `ui.grid.page_columns` setting, one `{page, view}` entry per
page, written through the shared mutation boundary (audited like any other
write) on every change and only when it differs from what is stored. A view
that is the page's own is not kept: the reset op returns the page to its own
columns and rows and forgets what was kept. A write the store refused costs
the memory of the arrangement, not the arrangement.

`load` resolves the view's specs against a fresh catalog for the scope: a
field an operator restricted since, a table the account may no longer read
or a spec no catalog knows is pruned from the table, the address and the
suggestions, and leaves the memory on the next arrangement. A lens named
for a column that is not shown is dropped rather than carried. A page whose
table is not in the account's catalog still works: the built-ins render and
the add box offers only the page's removed built-ins, which it offers first
by their label beside the catalog's columns. Rollup expansion in place
(`expand`/`collapse`) is accepted and ignored until Grid has an expansion
read; a rollup cell still shows its aggregate through every lens.

Run the live page against a **disposable** migrated database (sign in
as the fixture manager after the example reconciles roles):

```sh
BILIBI_GRID_DEMO=disposable DATABASE_URL=... APP_ORIGIN=... BIND_ADDR=... \
  cargo run -p bilibi-web --example g4-page-columns
```

It replaces the reference fixture (`reference::FIXTURE`, shared with
`g3-lenses`) and serves `/g4-page-columns`: the tenant's reference items as the
page's own list (search, category, sort, two pages) with Name, Category and
Quantity drawn by the page. Open the notch, add `group.name`,
`movements:count`, `movements.delta:sum` or `unit_cost`, switch lenses,
reorder and change the row height: the address updates, and a reload or a
later visit without an address shows the same arrangement. Paste the address
into another account's browser to see the sender's view leave that account's
memory alone. Reset forgets the page. The restriction switch restricts
`unit_cost` for the signed-in account's roles and lifts it again, so a walked
`unit_cost` column disappears from the table, the address and the add box
while restricted.
