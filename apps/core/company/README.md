# Core Company

`apps/core/company` owns the Organisation of the approved party model
(`apps/core/party/CONTRACT.md`): the `companies` table, its reference catalogs
(legal entity types, department types), a company's departments, the tenant's
primary company and the provisioning commands. The public API is the
`core/company.companies` service (`CompanyService`) and the free functions of
`organisation`, `reference_types`, `departments`, `lifecycle`,
`eligibility`, `relationship_roles` and `relationships`; primary-company functions and types are re-exported at the crate root. table layout and
SQL stay private. The module-owned pages (`web.rs`, `web/views.rs`) serve
`/companies`, `/companies/create` and `/companies/{id}`, and
`web/relationships.rs` serves `/companies/relationship-types`,
`/companies/{id}/relationships` and the `company.relationships` embed,
through shared UI components.

## Schema (K1, `migrations/004-organisation.sql`)

Additive over the frozen original migration: every existing company keeps its
id, tenant and `/companies/{id}` URL, and `OrganisationId` is that id.
`companies` gains Bilimbi's columns (`parent_id`, `code`, `legal_name`,
`registration_number`, `legal_entity_type_id`, `jurisdiction`, `website`,
`scope_activities`, `metadata`, `updated_at`). `code` is backfilled from the
name as Belimbing slugs it (`organisation::slug_code`) and is **unique within
a tenant**: Bilimbi's compatibility constraint spans tenants, which would let
one tenant learn another's codes. A parent is a company of the same tenant
(`(tenant_id, parent_id)` foreign key) and never the row itself.

`company_legal_entity_types` and `company_department_types` are global
catalogs with no tenant column (code unique, `version` for optimistic
updates). `company_departments` carries `tenant_id` with a scoped foreign key
to its company, one row per type per company, and `head_affiliation_id`: the
head's affiliation with the company under the party model, which K18 binds
with a foreign key and an API. `tenant_primary_companies` is the explicit
tenant → primary company relationship; nothing infers it from an id or the
oldest company. The migration also advances the `companies` identity past
rows inserted with explicit ids.

The production seed `company-legal-entity-types-v1` inserts Belimbing's eleven common legal forms, from sole proprietorship to branch office, each with a one-line description. It uses `ON CONFLICT (code) DO NOTHING`. After the first apply the seed ledger skips that version, so an edit, a deactivation or an added type stays on the next `bilibi-web seed`.

## Capabilities and reach

`contributions.rs` declares Belimbing's keys: `admin.company.view`, `.list`,
`.create`, `.update`, `.delete` and `admin.company.tenant-wide.manage`, and
extends `tenant_owner` (all). The walkthrough fixture grants its `manager`
(all), `staff` (view, list, update) and `viewer` (view, list) the same keys;
`bilibi-web authz-reconcile` reconciles an older database. A write to a
company or into it is authorized with `organisation::authorize_target`: the
capability now, and either the company the actor is affiliated with
(`Scope::actor().organisation()`, bound by K9) or the tenant-wide reach for
any other company of the tenant. The pages and Grid catalog ask these same durable keys. List and detail use
`.list` and `.view`; field saves and lifecycle commands use `.update` plus
company reach. The generic field editor never accepts `status`.

## Writes

Every write takes the sealed scope, checks its capability at the moment of
the call and records its rows through the caller's `MutationTransaction`
(`tx.mutate`); the service opens one transaction per call and commits only a
saved outcome. A refusal (`CreateOutcome::Refused`, `Outcome::Refused`) lists
`FieldError`s in the source's wording and is an outcome, never an `Error`;
`WriteError` names `NotFound`, `Archived`, `InUse` and `Stale`.

- `organisation::create` / `preview` (Bilimbi `create_company/3`,
  `preview_create_company/2`): trims and defaults (`status` active, code from
  the name), refuses a field the actor's role restricts, proves the parent,
  legal entity type, jurisdiction (`bilibi_geonames::country_known`, so no
  jurisdiction is recordable until countries are imported) and code
  uniqueness on the write's transaction under a per-code advisory lock.
- `organisation::require_writable` is the one archived check: a live company
  whose status is `archived` is read-only for good, for its own facts and
  every record it owns. Reads never ask it.
- `organisation::Provider` is the `OrganisationProvider` Tenancy's
  `PartyProofs` proves targets with: a live, unarchived company of the
  actor's tenant, bound at the service factory.
- `organisation::Directory` is Tenancy's `OrganisationDirectory` (K15), bound
  beside it: the companies a Base picker may offer (`in_scope`: every live
  company, archived flagged; `selectable`: the unarchived ones the actor may
  target with a capability, own company or tenant-wide) and `target`, the
  `authorize_target` rule on the sealed proof connection. The settings
  editor's company picker and the Authz role create picker read it; Base
  never queries `companies`. Migration `006-authz-role-owner` contributes the
  scoped foreign key behind Authz's `roles.company_id`.
- `files` (K15) is the private-file owner: the Media attachment subject
  `core.company` and the Artifacts owner `core.company` (`files::FILES`).
  Reads need `admin.company.view` and a live company of the tenant; writes
  need `.update` (purge `.delete`), the actor's reach and an unarchived
  company; a document's reference must name the company it is filed under.
  Use these, not a hand-rolled check, for any file attached to a company.
- `reference_types`: list/get, create, update (versioned), toggle active and
  delete, which refuses a type a live company or a department uses (`InUse`).
- `departments`: list, available types, create, status update (versioned)
  and delete, each beginning with the writable-company guard.
- `primary::assign` locks the tenant row, then the company row, judges the
  company under that lock (archived refused) and assigns or transfers.

Inline edits on the original detail page (`save`) now accept code, legal name,
registration number, jurisdiction and website with the same rules; `status`
is read-only there; K3 adds the lifecycle dialog. Forged generic status
edits are rejected in the domain too.

## Agent operations

`agent_operations.rs` registers `core.company.list` and `core.company.get`
(reads) and `core.company.create` (a reviewed safe write: preview shares
create's validation and code check without reserving it, commit writes
through the plan's transaction) plus the `core.company` guide. A restricted
field arrives as `{"restricted": true}`. The handlers read through a pool
bound once by the service factory.

## Provisioning

```sh
cargo run -p bilibi-web -- tenant-provision --tenant-name "Acme tenant" --company-name "Acme Sdn. Bhd." --company-code acme
cargo run -p bilibi-web -- platform-provision --company-name "Example Operations" --company-code example_operations
```

`primary::provision_tenant` atomically creates a tenant and its primary
company; `provision_platform_operator` resolves the explicit operator tenant
or creates and marks it, and re-running creates neither a second operator
nor a second company (`bilibi-web create-owner` calls it first when no
operator company exists, then creates the first account). Both are deployment writes recorded as one retained
`company.*.provisioned` audit action by the `provision` system principal,
through Tenancy's `provision_tenant` / `provision_platform_operator`
seams. Ids come from PostgreSQL.

## Platform-operator setup and dashboard panels (K17)

`/setup/platform-operator` (`src/web/setup.rs`, Bilimbi
`PlatformOperatorSetupLive`) is the signed-in counterpart of
`platform-provision`: an operator page (`OperatorCapability(admin.company.update)`,
so another tenant gets the host's refusal rather than Bilimbi's redirect) that
designates a live company of the operator tenant, or creates one, as its
primary company. `CompanyService::designate_operator_company` and
`create_operator_company` prove the operator again on their transaction; the
create commits the company and the assignment together. Once the operator
stands on a company the page sends the reader there. The shell's status bar
links here as "Operator company not set" while the operator tenant has no
primary company.

`src/web/dashboard.rs` draws the `dashboard.companies` widget
(`base-dashboard-company-stats`: total, active, the current company's code)
and the `dashboard.company` section (`current-company`: name, code, status and
Open company). The current company is the one the account signs in under
through its affiliation, else the tenant's first live company. Neither needs a
capability to read, as in Bilimbi; the links show only to a reader holding
`admin.company.list` / `admin.company.view`.

## Runtime inspection

Migrate a disposable database, then run

```sh
BILIBI_COMPANY_DEMO=disposable cargo run -p bilibi-company --example k1-organisation-walkthrough --locked
```

It prints identities and tenancy proofs for the fixture companies, refused and
accepted creates, the administration search with and without restricted
columns, the reference catalogs, departments on company 1, the archived
refusals, primary assignment, the agent operations and a plan commit with
replay, and both provisioning commands. It creates a tenant and, when none
exists, the platform operator. No unit tests belong here.

## Relationship roles and links (K5)

`migrations/005-relationships.sql` adds the party model's
`company_relationship_roles` and `company_relationships`
(`apps/core/party/CONTRACT.md`). Bilimbi's global
`company_relationship_types` and company-to-company links become a
**tenant-local** catalog and **directed** links whose target is an
organisation of the tenant or a Person, with scoped foreign keys, an
exactly-one-target check and no cascade. Company therefore depends on
`core/person` (its table must exist first) and on `core/party` for the
wire types; a Person target is proven through Tenancy's `PartyProofs`,
never read from Person's table, and is named through the Principal
Directory.

`relationship_roles`: a role reads from the source company's side
(`name`: what the target is to the source; `inverse_name`: what the source
is to the target) and confers no capability; it is never an Authz role. The
contributed defaults (`vendor`, `customer`, `prospect`, `partner`) are
reconciled **by code** into each tenant: the migration does it for existing
tenants, provisioning for new ones, and `CompanyService::relationship_roles`
for a tenant whose catalog is still empty (the walkthrough fixture creates its
tenants after the migration). An administered role is never overwritten; no default
has a fixed id. Codes are trimmed, lower-cased, 1–64 characters of letters,
digits, `_`, `-` and `.`, unique within the tenant under an advisory lock.
`create` needs `admin.company.create`; `update` (a rename: id and code never
change) and `retire` need `admin.company.update`; `list`/`get` need
`admin.company.list`/`.view`. A retired role refuses new assignments and
keeps every link; `retire` locks the row `FOR UPDATE`, so it waits for an
assignment holding the role `FOR SHARE`.

`relationships`: `source → target: vendor` means the target supplies the
source. `assign` needs `admin.company.update` with reach over the source
(`organisation::authorize_target`), locks the source and an organisation
target ascending through `eligibility::lock_account_company` (so an archive
on either side serializes with it), proves a person target live and
unretired, locks the role unretired, and refuses a self link, an archived or
missing target and an exact duplicate (same source, target, role and
start); different roles may overlap for one pair and one role may have
separate dated intervals. `update` replaces the inclusive dates, `end` sets
the last day, `unlink` sets `unlinked_at`: every row stays as history and
`list_for_company`/`list_for_person` show it with `history`. Either company
of a link may administer it, as in Bilimbi; a person target confers nothing.
Each entry carries the link, its direction from the listed party, the other
party's name and the label from that party's side (`name` outgoing,
`inverse_name` incoming), sorted by other party then label. Refusals are
`Outcome::Refused` field errors; `Stale`, `NotFound` (including an unlinked
link) and `Archived` are `WriteError`s.

`relationship_operations.rs` contributes `core.company.relationship_role.
{list,get,create,update,retire}`, `core.company.relationship.{list,get,
assign,update,end,unlink}` and the `core.company.relationship` guide. Writes
are reviewed safe writes: previews share the owner's checks without locking
or writing, commits write through the plan's audited transaction. Assign
input is flat (`source_organisation_id`, `target_kind`, `target_id`,
`role_id`, optional `effective_from`/`effective_to`); omitted dates are
unbounded.

```sh
BILIBI_COMPANY_DEMO=disposable cargo run -p bilibi-company --example k5-relationships-walkthrough --locked
```

The walkthrough prints the reconciled catalogs of both tenants, refused and
accepted roles, a rename keeping id and code, overlapping and duplicate
assignments, self/archived/cross-tenant/retired-person targets, viewer and
staff refusals, both companies' and the person's pages with inverse labels,
a retirement waiting behind a concurrent assignment, date moves, ending and
unlinking with history, an edit from the target's side, the agent reads and
the reviewed assign/create commits with replay refusal, then bounded audit
counts. It creates persons and companies with `K5`/`k5_`/`wt_` prefixes in
the disposable database only. The screens are described under "Relationship screens (K6)".

## Relationship screens (K6)

`web/relationships.rs` serves the K5 catalog and links through the shared
UI (`core/company.relationship_pages`):

- `/companies/relationship-types` (`admin.company.list`; menu "Relationship
  Roles", `admin.company.relationship-type` under Companies, as Bilimbi names
  both; the earlier `/companies/relationship-roles` redirects here): the
  tenant's roles in a `FlexTable` with code, name, inverse name and status, retired roles on
  request. "+ Relationship Type" (`admin.company.create`) and the row's edit
  (`admin.company.update`) share one modal; a rename keeps the code, which
  the edit form shows disabled. Retire confirms through the shared dialog
  and acts on the role held when the dialog opened, at that version. The
  page copy says what the model guarantees: a role describes the business
  relationship only and grants no access.
- `/companies/{id}/relationships` (`admin.company.view`; Bilimbi
  `RelationshipsLive`): the company's links as the source or the target,
  with the related party (a company with its code, or a person), direction,
  the role's label from this company's side, the inclusive dates and the
  status (`active`, `inactive`, `unlinked`). "Show history" brings ended and
  unlinked links back; a retired role stays visible on its links. "Add
  Relationship" chooses a company or a person of the tenant, an unretired
  role and optional dates, validating as values change so the picker
  follows the kind; a person picker needs `admin.person.list`, which the
  form says when the reader lacks it. Edit changes only the dates, so
  ending a link is setting its last day; Unlink confirms and keeps the row
  as history. Every opening and write asks `organisation::authorize_target`
  and `require_writable` again; a denial, an archived company (either
  side), a stale version and an already unlinked link are flash messages,
  never a fatal error.
- Embed `company.relationships` (slot `record.relationships`, operation
  `panel`, `admin.company.view`): the Relationships card of a record page
  (Bilimbi `ShowLive` section 6), mounted as a panel with path
  `{panel: <element id>, kind: organisation|person, id}`. The company
  detail page mounts it for the company (`web.rs` `panels()`, slot
  `#company-relationships` in `views::detail`) with a Manage link to the
  administration page. The same panel is also registered as
  `company.person-relationships` for record kind `core/person`, which
  Person's detail page discovers and mounts with its own path (`person_id`,
  `kinds: person`), showing the companies that relate to the person with a
  link to each. The card's `FlexTable` targets the panel, so its column and
  zoom events reach the panel's own state.

The pages hold no client-chosen version: edits, unlinks and retirements
carry the version of the row read when the dialog opened, so a concurrent
change is refused as stale rather than overwritten. Rows keep the K5 order
(other party, then label); none of these tables sorts, as in Bilimbi.

The relationship table and its record panels also offer **Effective From** and **Effective To** as separate columns. Their labels come from the date editor’s shared record fields. The default Effective Period column stays in place.

## Lifecycle and account freeze (K2)

Use `CompanyService::{activate_company,reactivate_company,suspend_company,
archive_company}` for explicit business commands, or `transition_company`
with an expected version for a dialog. `lifecycle::apply` shares an existing
`MutationTransaction` for headless/owner composition. The transition table
is `lifecycle::Operation`; `operations_from` returns the offered actions in
activate/reactivate/suspend/archive order. Pending → activate → active;
active → suspend → suspended; suspended → reactivate → active. Archive
accepts pending/active/suspended and is final. Creation still accepts only
pending/active. No generic field edit accepts `status`.

Each command requires `admin.company.update` and, for a company other than
the actor's own proven organisation, `admin.company.tenant-wide.manage`.
An explicit operator identity or admin grant does not bypass tenant scope,
standing checks, field restrictions, or the final archive state. Archive
and suspend refuse the tenant's primary company and the actor's own company.
The primary assignment and lifecycle commands both lock the tenant before
the company, so the standing check is atomic with primary transfer. Tenant
locks use `FOR NO KEY UPDATE`, compatible with the `KEY SHARE` needed by a
child insert's tenant foreign key while it already holds the company lock.

An optional reason is trimmed, blank becomes absent, and the limit is 500
Unicode characters. A version conflict is `WriteError::Stale`; a disallowed
transition carries its current status in `LifecycleError::InvalidTransition`.
The company row lock covers transition judgment, version increment and the
status write. `tx.mutate` captures the status change and writes a retained
semantic `company.activated/reactivated/suspended/archived` action with
subject, summary, old/new status and reason. The outer transaction also
records its usual `company.lifecycle` boundary action. Either audit failure
rolls everything back, even if a caller catches it. The returned company
honors the actor's field restrictions.

`eligibility::account_eligibility` supplies the company fact to the User
account binder (`bilibi_user::identity::standing`) for login, session/bearer
validation and reset issue / redeem. Only archived/deleted/missing freezes an
account; pending and suspended do not. No role or business relationship confers
account eligibility. The binder calls this on every validation and caches no
derived account status; impersonation (K14) must do the same. Core remains
independent of People.

Account/affiliation/attached-owner writes take the company lock **before**
the child row and retain it until commit. Use `eligibility::lock_account_company`
for an account write and `lock_writable_companies` for moves / relationships
whose two companies must both be writable. The latter sorts and deduplicates
IDs before locking; use the same order in either direction. A write that wins
finishes before archive; a write that loses sees `WriteError::Archived`.
`organisation::require_writable(..., false)` alone is a preview, never an
atomic write guard. Existing department writes already hold the company lock.
`organisation::list_tenant_company_ids` includes archived and deleted IDs for
User's historical read model; the caller authorizes the read separately, and
these IDs grant no write/login authority.

For manual runtime inspection (fresh disposable migrated database):

```sh
BILIBI_COMPANY_DEMO=disposable cargo run -p bilibi-company --example k2-lifecycle-walkthrough --locked
```

The walkthrough prints transitions and refusals, own-company versus tenant-wide
reach, Unicode reason validation, stale versions, a deliberate PostgreSQL audit
failure/rollback, two connections contending for an account/company lock, the
final archive freeze and retained-action count. Its own-company scopes use a
trusted specimen provider through the real authentication boundary until K9
binds durable affiliation facts. It never creates an account link.

## Company pages (K3)

The list composes shared `FilterToolbar`, `Pagination` and Grid `PageColumns`:
Name, Code, Parent, Status and unrestricted Jurisdiction are built-ins. Search
matches only readable name/code/legal-name/email/jurisdiction fields. Search,
status, sort, page and `per_page` live in the URL; the supported sizes are
25/50/100/300. `q` and `size` remain accepted for old links. Column/lens/
zoom URL parameters override the account's saved arrangement; resetting forgets
it through Grid's `PageViews` service.

Create uses K1 validation and records optional business context on the same
audited transaction. Detail uses shared read-first inline text, choice and
long-text controls, activity chips with confirmed removal, per-field commit
outcomes and version fencing. Legacy activity JSON objects remain editable
without silently converting their business meaning. Parent
changes serialize on the tenant and reject a cycle. Lifecycle buttons come
only from `lifecycle::operations_from`, hold the record version in server state,
and confirm with an optional reason; archive remains final. Record pins use
the shell's existing account pin API, with a resolved record label and URL. The Audit history
panel retains its mount through detail patches. Child-owned panels are attached
by their later owner slices.

Subsidiaries and Departments keep independent `ListColumns` arrangements, including column re-add suggestions and density controls. Subsidiaries mirror Bilimbi's Name, Status, Legal Entity Type and masked Jurisdiction columns and workspace-aware record links. The list is bounded to the first 300 direct children in ID order; a truncation notice points to the full Companies directory.

Relationship administration also exposes End separately from Edit dates and Unlink, through `CompanyService::end_relationship`. It holds the opened version, retains the start date and records an inclusive end date; ending today does not make the relationship inactive until tomorrow. Invalid dates and stale writes keep the modal draft.

## Reference-type and department pages (K4)

`web/reference_types.rs` serves `/companies/legal-entity-types` and
`/companies/department-types` as one modal table driven by a `Spec` (nouns,
DOM ids, whether the category filter is on), as Bilimbi's
`ReferenceTypesLive` does: `ListColumns`/`FlexTable` with built-in columns,
the New/Edit dialog validating as values change through
`reference_types::check_new`/`check_update` (the same validators the write
runs), Activate/Deactivate, and delete behind the shared confirm dialog
acting on the held row. Both routes are `OperatorCapability(admin.company.list)`
(Bilimbi `operator: true`) and the menu entries nest under Companies; a
`can_*` only decides which controls render, each write asks `CompanyService`
again, and a refusal is a page flash, never a page-clearing error.
`NewType.is_active` carries the dialog's Active checkbox.

`web/departments.rs` serves `/companies/{id}/departments` (Bilimbi
`DepartmentsLive`): the company header, the archived alert, Add Department
from `departments::available_types`, one event per status change, Set
Department Head, and Remove behind the confirm dialog. Every persisting event
passes one gate (`admin.company.update` now, then `require_writable`) before
the API's own checks, so an archived company answers in Bilimbi's words. The
detail page's Departments card (`views::departments_section`, built-ins
`department_builtins`, event `departments_grid`) leads here through Manage.

A department's head is an affiliation. Company cannot name one across the
module boundary, so `departments::HeadDirectory` is the seam: the affiliation
owner binds one at boot with `bind_head_directory` (candidates and names,
scoped by the query); `departments::update_head` accepts only an affiliation
the directory offers for that company at the moment of the write, under the
company lock and the row version. Until a directory is bound, the Head column
shows "—", the dialog offers no one and says so, and no head can be stored.
The Grid catalog also exposes `departments` (tenant-scoped through the
company) and the global `legal_entity_types`, with the `legal_entity_type`,
`departments` and `company` links Bilimbi declares.

## Owner binders walkthrough (K15)

```sh
BILIBI_COMPANY_DEMO=disposable BILIBI_K15_DEMO_ROOT=/tmp/k15-files \
  cargo run -p bilibi-company --example k15-owner-binders --locked
```

On a disposable migrated and seeded database it archives company 4, then
prints the organisation directory (counts, archived flagged and never
selectable, cross-tenant `NotFound`, denied `Forbidden`), a company-scoped
settings write accepted for a live company and refused for the archived one,
the locale bootstrap before and after a Swiss operator address plus its
adoption, company-owned role creation with the blank, archived and
cross-tenant refusals and the visibility rule, and the Media and Artifacts
owners accepting an authorized upload or document and refusing the viewer,
the archived company, the missing person and the other tenant. In the running
app, Administration → System → Settings offers the company picker and
Administration → Authorization → Roles → Create asks for the owning company.

Company scalar facts share a record-field schema with Grid. Optional columns include Legal Entity Type, Parent Company, Website and the other visible scalar facts. Related columns project readable names; business activities and metadata remain nested or long content on the record. Default column order is unchanged.

## Companies in a newly created tenant

`CompanyService::preview_tenant_companies` and `provision_new_tenant_in` compose tenant provisioning with [Core Tenant Administration](../tenant_administration/README.md). Names use ordinary Company validation, generated codes and the person's field restrictions; duplicate generated codes are refused. The first company becomes primary.

The write requires Tenancy's sealed `CreatedTenant` receipt, bound to the current transaction and original actor. It accepts no arbitrary target tenant id and never changes the caller's scope. Tenant-create capability and live operator standing authorize the complete provisioning action; its contract enforces Base Step-up. Every company and primary assignment is a `Change` on that same transaction. Existing tenant business data remains behind its ordinary scoped facade.
