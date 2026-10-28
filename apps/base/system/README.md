# System pages

`base/system` owns `/system/info`, `/system/localization` and
`/system/menu-inspector`. Each requires its contributed platform capability
**and** the persisted operator identity. It also owns the owner-only
`/system/database-tables` (below). The `system_viewer` role contributes
Info and Inspector read capabilities; use the normal explicit
`authz-reconcile` command to reconcile an existing installation. No grant or
operator marker is installed by visiting a page.

Pages compose the shared UI, list state, form and FlexTable components and are
eligible for chromeless workspace frames. The host guards HTTP, socket mount,
events, URL changes and refresh; owners also reauthorize calls to their page
services. No unit tests or test-only infrastructure are included.

System Info reads facts once per HTTP navigation, matching Bilimbi's static
snapshot. A mounted page retains those facts while the live transport keeps
shell panels and authorization active; background refresh does not re-probe.
Rust replaces Elixir/OTP/ERTS with the actual compiler, Tokio worker
and live process facts from Perf. The Database card exercises a fresh bounded
query, exposes no connection string or credentials, and distinguishes an
unreachable database from a missing probe. Applications lists the installed
registry modules with their workspace version, rather than pretending Rust
has loaded OTP applications. The environment is explicitly the build profile,
debug means debug assertions, and timezone describes UTC timestamp storage.
`APP_ORIGIN` follows the Web command's configured origin and default.

Unavailable probes stay visible. Disk capacity has no installed portable
probe. Queue supplies job states but no executor-heartbeat health boundary;
its health is unavailable rather than inferring an active executor from an
empty or reachable queue. Perf combines recorder and history-store health and
reports disabled collection explicitly. Settings has no value cache; its row
says that values are read directly from PostgreSQL.

Language & Region reads `Locales::resolve_global`, so an account locale cannot
masquerade as the installation choice. Options come only from `SUPPORTED`.
Every save reauthorizes inside the audited mutation transaction and calls
`Locales::put_global`: this atomically records manual provenance and removes
any inferred-country fact. Unsupported choices are refused visibly. Bootstrap
provenance remains read-only and no Core address tables are queried. English
product copy is the only installed copy; regional support is not translation.

Menu Inspector lists the complete validated Menu catalog and module
provenance. Allowed is the item's own capability rule (including any-of and
direct denials), not whether an ancestor happens to display it. Served checks
the actual contributed GET route patterns. Source/search filters and bounded
25/50/100/300 pagination survive Back and reload through `ListState`; shared
`ListColumns` owns column add/remove/move and zoom. No page-owned table or
browser event controller duplicates those components.

Runtime and screenshot evidence for slice O5 is recorded outside the checkout
under the task's `data/bilibi-o5/` directory. Follow `docs/runtime-parity.md`
from the repository root for disposable database and browser procedures.

## Instance identity (`src/instance.rs`)

`Instance::resolve(settings, client)` reads the server role, label and colour from global settings. **`instance.role` is the single source of the role**, shared by the status bar, banners, Software, agent discovery and MCP initialize.

The explicit production seed initializes `instance.role` from `BILIBI_ENV`: `development`, `staging` or `production` (unset means Production). It refuses other values. `install-server.sh` supplies this first value for servers; `start-app.sh` supplies Development for local copies. Seeding again preserves the saved role. Boot never seeds, and changing the environment after install cannot override the setting.

Click the status-bar role to open `/system/instance`. A **Core Administrator in the platform-operator organisation** can change the role, label and colour, with `admin.system.instance.manage` and no impersonation. The page and the `Instances::save` facade recheck these rules; writes recheck inside the audited mutation transaction. Every role change requires a fresh 2FA code through `base.system.instance.role.change`, declared `.always()`, including on Dev. Its `require_code` check refuses a missing submitted code even immediately after enrollment or another browser confirmation. The page warns that changing to or from Production changes warnings and 2FA rules without moving any data. A stale page cannot overwrite a newer role. The shell chooses whether to link the role with the unlogged `Instances::is_core_administrator` read plus its own capability set, so ordinary page renders write no authorization decisions.

| Setting | Default | Meaning |
| --- | --- | --- |
| `instance.role` | Install input | Dev, Staging or Production; only the protected instance page changes it |
| `instance.label` | Current role word | The name shown to people and agents |
| `instance.colour` | Blue for Dev, amber for Staging, red for Production | A shared status colour |

The label and colour also remain in the generic settings editor behind `admin.system.instance.manage`. Empty label and colour follow the stored role. The role has no generic editor, so delegated operators cannot bypass its Core Administrator and 2FA checks.

The Banner colour field uses the shared `ColourInput`: Blue, Green, Amber and Red each show their name inside that banner colour. “Use role colour (Blue)” (or Green, Amber or Red for the stored role) shows the role's default colour and clears the override when saved. `src/instance.rs` owns the role-to-colour rule.

`Instance::status` always names the role, including when the label differs. Staging keeps an extra top-bar badge. Dev and Staging show a badge on sign-in; Production shows none there. Reviews and confirmations use `Instance::banner` in every role. `Instance::agent_json` identifies plans, commits, MCP and `/.well-known/bilibi`. All use the same resolved identity.

Base Step-up binds an `Enforcement` policy supplied by `Instances`. It reads the saved role on every check across processes: ordinary operations skip 2FA on Dev; `.always()` operations never skip it. The policy holds no process-local role cache. The server layout is in [`docs/server-install.md`](../../../docs/server-install.md).

`Instance::public_address` says whether an AI app on the internet can reach this Bilibi: false for a Dev copy and for an origin that is `localhost`, a loopback, private, carrier-grade NAT (`100.64.0.0/10`) or link-local IP (an IPv4-mapped IPv6 literal counts as its IPv4 address), a bare host name, or a name under `.localhost`, `.local`, `.internal`, `.lan`, `.home.arpa`, `.test` or `.ts.net`. It is decided from the stored role and `APP_ORIGIN`, never from the browser; the status bar's AI panel uses it to say when only an AI program on this computer can connect.

Agent connection identity also includes `APP_ORIGIN` (the canonical public address, with the host's default when unset). `Instance::agent_notice` names the label, role and address; `agent_json` includes `role` and `origin` alongside the existing fields, so discovery and plan/commit results identify the same destination even when an operator gives two environments the same label.

## Database Tables (`src/database_tables/`)

Port of Belimbing `admin/system/database-tables`. `/system/database-tables`
lists the base and partitioned tables of the application schema
(`current_schema()`) with the module and migration that create them, read
from the mounted modules' migration SQL (Bilibi has no table registry; the
`bilibi_migrations` ledger is Database's). `/system/database-tables/{table}`
shows Data, Schema (columns, indexes, constraints, migration source) and
Relationships (outgoing and incoming foreign keys) as `tab` patch links. Every
list is a FlexTable through `ListColumns`; Data sorts, pages (25/50/100) and
searches text columns through `ListState`, and a referenced value links to the
other table filtered by `col`/`eq`. The tables list also offers the table's column count as an optional column (`database_tables::fields`); Table, Module, Migration and Rows stay the defaults.

The search and masking hint sit above the framed table on the page canvas. The title pin saves the tables list or an individual table's address. Data shows only stored rows: `base_settings` contains saved overrides and runtime state, while module defaults are resolved by Settings without inserting rows. Its masking hint distinguishes always-hidden columns from values hidden only on encrypted rows.

Controls:

- **Owner only.** Routes are `Access::Owner` and the menu item
  `Requirement::Owner`: the account the `user_bootstrap` receipt names, live in
  the platform-operator tenant, in its own browser session (no impersonation,
  no agent grant). The host re-proves it on every request, mount, event and
  params change, and the pages prove it again. No role or grant extends it, so
  a `core_admin` of the operator tenant gets the standard 403 and no menu item.
  Because only the owner reads, Data shows rows of every tenant.
- **Step-up.** Opening the pages is the Base Step-up operation
  `base.system.database_tables.view` ("Open the database tables pages"),
  declared here on the installation floor, so tenants cannot turn it off.
  The routes are `Access::StepUp { operation, inner: &Access::Owner }`: outside
  Dev (`instance.role`) the host sends the owner to confirm first unless
  the session's window holds, and an open page leaves for the prompt when the
  window runs out. The host re-admits the step-up on every live event. The
  page no longer asks for a password of its own.
- **Audit.** Base Step-up records each check (`step_up.*`) and
  `system.database_tables.viewed` each distinct view (table, tab, page, page
  size, sort, whether a search or which filter column was used). Search text,
  filter values and row values are never recorded; a view whose record cannot
  be written is refused.
- **Masking.** Columns Audit redacts by name, credential-like names
  (`password`, `token`, `secret`, `*_hash`, `api_key`, ...), the session id and
  CSRF value, and an encrypted setting's value are never selected; the page
  learns only whether they are set, cannot search or sort by them, and has no
  unmask control (`src/database_tables/sensitive.rs`).
- **Read-only and bounded.** No SQL entry. Identifiers come from `pg_catalog`
  and are quoted from its spelling; request text binds as parameters. Rows are
  read in a `READ ONLY` transaction with a 5 s statement timeout, at most 100 a
  page and 120 characters a cell; a count past the timeout leaves the total
  unknown rather than failing the page.

Without 2FA, the shared `bilibi_ui::StepUp` form disables the role selector and links to setup with a return address. Name and colour changes can still be saved. After setup, the role selector and optional code input are available.
