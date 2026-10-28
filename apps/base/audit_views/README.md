# Audit screens

`/audit/actions`, `/audit/mutations`, the `record.history` embed and the
dashboard's Recent Activity panel, ported from Bilimbi `Audit.Web`
(`ActionsLive`, `MutationsLive`, `MutationDetails`, `MutationDiff`,
`RecordHistory`, `DashboardActivityPanel`). The evidence, its capture and its
reads live in `base/audit`; this module only presents them.

Why a module of its own: Authz records its grants through Audit
(`base/authz` depends on `base/audit`) and a screen needs Authz and the
Dashboard, which also needs Authz. A screen inside `base/audit` would close that
cycle. Bilimbi breaks it with an application-environment callback; here the
screens sit above both. The audit capabilities (`admin.audit.log.list`,
`.manage`, `.view`), the `auditor` and `system_viewer` role grants and the
"Audit Log" menu branch belong here for the same reason.

## Actions (`/audit/actions`)

Newest first. Search (event, URL, actor role, trace, actor type, payload text),
event family (HTTP, Authentication, Console, SQL console, Queue, Domain),
result (Failures, Retained), diagnostic traffic (hidden by default; failures
still show) and actor type narrow the list in SQL; the URL keeps every filter,
the sort and `page_size`, so Back and reload restore the view. The table is the
shared FlexTable over `ListColumns` (reorder, remove, row height). Bilimbi's
screen has no period filter, so there is none here.

`admin.audit.log.list` guards the route and every live event.
`admin.audit.log.manage` decides whether the bookmark control is drawn, and the
write asks it again: a grant removed after the page opened flashes a refusal
instead of changing the row. Retaining flips a bookmark in
`base_audit_action_metadata`; the recorded `is_retained` bit is evidence and
never changes. Each flip is itself an audited write
(`audit.action.retention`, mutation kind `audit_action_retention`).

## Data Mutations (`/audit/mutations`)

Newest first. Search (record kind, event, subject name/identifier/id, trace,
actor role and type) and the event (Created, Updated, Deleted) narrow the list
in SQL; the URL keeps the search, the filter, the sort and `page_size`. The
table is the shared FlexTable over `ListColumns`. Each row's Details cell names
the changed fields (at most four, then a count) and opens the field diff on a
press: the values stay on the server until the row is open, as in the source.
A value shaped like an instant renders through the page's clock with seconds;
a key naming a password, secret, token, key or hash is capture-redacted and
reads muted.

`src/diff.rs` is the one diff model both this screen and the history panel
render, so a change reads the same on both. Actor labels
(`User #7`, `Employee #3`, `System · name`, `… · impersonated by User #5`)
are `src/present.rs`, shared with Actions.

### Withheld fields

A field the record's owner withholds from this reader is still a row, so the
reader knows the record changed, but it carries no values and renders the
shared `Restricted` marker. `bilibi_authz::fields::withheld_by_record_type`
maps the reader's field restrictions to audit kinds through the Grid field
catalog's `record_types` (`CatalogTable.record_types`), and `mutations::withhold`
takes the values out of every entry each time it is drawn, so a field
restricted after the page loaded is withheld from the next diff that opens. A
screen that keeps a read keeps the stored entries and calls it per draw. A
record kind reaches this only when its owner's Grid table names it in `record_types`: Companies
(`core/company/src/contributions.rs`) and the Grid reference items do; a table
without it is not withheld in the audit views, so declare `record_types` with
every restrictable table.

## Record History (`record.history` embed)

The demoted labelled "History" disclosure a record page places in its header
(`src/history.rs`): the record's latest five mutations with their diffs, in a
popover. A host page returns the panel from `LiveState::panels`:

```rust
PanelSpec { id: "company-record-history".into(), embed: "audit.record-history".into(),
    path: [("panel", "company-record-history"), ("kinds", "company"), ("id", id)]…, query: {} }
```

`kinds` is the comma-separated list of audit kinds the record is written under
(its current one and a legacy one), `panel` the spec's own element id, and an
optional `title` query renames the heading. The page renders an empty element
with that id where the control goes, inside a `data-live-ignore` slot
(`<div data-live-ignore><div id="company-record-history"></div></div>`, as the
dashboard hosts its panels) so the page's own patches leave the mounted panel
alone; a reader without `admin.audit.log.list` keeps the empty anchor. The trigger's `aria-expanded` is the one record of
open, written by the shared `DisclosureDismiss` hook (click toggles; Escape, a
click outside and focus leaving close) and kept across patches by the
wrapper's `data-live-expanded`; the server re-reads the trail on every click and every host
update, so an open panel follows an in-page edit. The Company detail page
hosts it (`apps/core/company/src/web.rs`).

## Recent Activity (dashboard)

`bilibi_dashboard` entry `base-dashboard-recent-audit` (medium, order 30, offered
to holders of `admin.audit.log.list`, refreshed every two minutes) drawn by the
`audit.dashboard-activity` embed: the five latest mutations in the tenant. The
embed is `SignedIn` so a card already on the page survives a revoked grant and
shows an em dash without reading. The open control goes to `/audit/mutations`.

## Running it

`examples/actions-fixture.rs` inserts a fixed 64-action ledger (every actor kind
and event family, each result, diagnostic traffic, one bookmark) into a
disposable tenant and prints what each filter returns; see its header. Then open
`/audit/actions` as an account holding the capability (the fixture `admin`).

`examples/mutations-fixture.rs` inserts a fixed 40-mutation ledger (every actor
kind, created/updated/deleted, impersonation, a system principal, a legacy row,
timestamps, nested JSON, redacted secrets, a six-field change and a second
page) and makes the fixture account `staff@example.test` an audit reader whose roles
restrict `reference_items.unit_cost`. Open `/audit/mutations` as `admin` for the
values and as `staff` for "Restricted" in their place (the walkthrough fixture already
restricts the company tax ID and revenue for staff, which the Companies rows
show the same way); open `/companies/1` and press History for the trail.

Actions, Data Mutations and record history show "Client identity" separately from "Model (as reported)". Both are evidence; neither replaces the account attribution or grants permissions.
