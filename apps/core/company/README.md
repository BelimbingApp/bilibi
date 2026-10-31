# Core Company

`apps/core/company` owns the Organisation of the approved party model
(`apps/core/party/CONTRACT.md`): the `companies` table, its reference catalogs
(legal entity types, department types), a company's departments, the tenant's
primary company and the provisioning commands. The public API is the
`core/company.companies` service (`CompanyService`) and what the crate root
exports: the functions other owners call on their own connection or
transaction (`authorize_target`, `require_writable`, `lock_account_company`,
`create`, `create_department`, `apply_lifecycle`, `assign_relationship`, the primary-company
functions, ...) and the types they take and return. Every module is private, so
table layout and SQL stay private too. The module-owned pages (`web.rs`, `web/views.rs`) serve
`/companies`, `/companies/create` and `/companies/{id}`, and
`web/relationships.rs` serves `/companies/relationship-types`,
`/companies/{id}/relationships` and the `company.relationships` embed,
through shared UI components.

## Schema (K1, `migrations/004-organisation.sql`)

Additive over the frozen original migration: every existing company keeps its id, tenant and `/companies/{id}` URL, and `OrganisationId` is that id. `companies` gains Bilimbi's columns (`parent_id`, `code`, `legal_name`, `registration_number`, `legal_entity_type_id`, `jurisdiction`, `website`, `scope_activities`, `metadata`, `updated_at`). Creation suggests a code of at most 12 characters using whole name words and omitting trailing legal forms such as Sdn Bhd, Berhad and Ltd. It truncates the first word only when needed to fit the limit. For example, `PONG CODAN RUBBER (M) SDN. BHD.` becomes `pong_codan`, and `Scicom (MSC) Berhad` becomes `scicom_msc`. Taken suggestions get `_2`, `_3` and so on within the limit. A person can replace the form's suggestion with a code up to 255 characters, including an imported code. If they clear the field, Bilibi chooses the code. The shared rule lives in `bilibi_party::default_code`. The code stays fixed after creation and is **unique within a tenant**: Bilimbi's compatibility constraint spans tenants, which would let one tenant learn another's codes. A parent is a company of the same tenant (`(tenant_id, parent_id)` foreign key) and never the row itself.

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
`bilibi-web authz-reconcile` reconciles an older database. A write to a company
record is authorized with `bilibi_company::authorize_target`: the capability
now, and either `Scope::current_company()` or the tenant-wide reach for any
other company of the tenant. Operational records owned by a workplace use
`authorize_work_target` instead. Extra desks do not widen Company-record
administration. A named system principal's scope
(`Scope::principal()`, SP1) is affiliated with no company: once granted the
capability in the tenant it reaches every live company of that tenant, in
`authorize_target`, `Directory::selectable` and `Directory::target` alike;
`require_writable` is the same archived check for it. The pages and Grid catalog ask these same durable keys. List and detail use
`.list` and `.view`; field saves and lifecycle commands use `.update` plus
company reach. The generic field editor never accepts `status`.

## The current company

**Contacts you trade with, workplaces you work in.** The top bar's **Switch desk** lists the person's explicit desks. A company manager can add or remove desks, including their own, on **People with a desk here** on the company record or **Desks** on the person's record. Both call `core.user.desk.set`; Core User owns the staff-only assignments and their audit. A desk grants no action capabilities.

`Scope::current_company()` is the chosen company while the person still has access, else their remembered or first valid desk, or no desk. Tenant-wide management still permits managing company records but adds no desks. Use `authorize_target` for company-record administration, and `authorize_work_target` plus the owner's capability for company-owned operational records. `authorize_work_target` accepts the caller's object-safe `ProofClient`, so owner transactions and proof callbacks keep the same connection; an explicit browser target also satisfies that company's sign-in policy, while trusted nonbrowser callers and captured Queue jobs retain desk proof. Decision: [ADR 18](../../../docs/architecture/decisions/18-company-work-access-and-page-scope.md).

| Piece | Owner | Contract |
| --- | --- | --- |
| `bilibi_company::reachable(client, scope, id)` | Company | `true` for a live, unarchived company of the tenant the account reaches: one assigned through an explicit desk. Never an error for a missing, archived or other-tenant id |
| `Directory::reachable` (`OrganisationDirectory`) | Company | The same desk access, listed: what the top bar's switcher offers. No capability is asked, because the list is the reach alone |
| `sessions.company_id` and `Sessions::choose_company` | Base Session | The stored choice; `NULL` lets Tenancy select the remembered or first valid desk. See [Base Session](../../base/session/README.md#current-company) |
| `ActorVerifier::reaches_company` | Core User, through `reachable` | Proves the stored choice on every authentication; a choice the account no longer reaches is dropped from the scope and the session edge forgets it |
| `UserService::choose_company` | Core User | The switch: proves the reach, writes the row and records `user.company.switched` in one transaction (a refused one records `user.company.switch_refused`) ([Core User README](../user/README.md#current-company)) |
| `POST /session/company` | Web | The top bar's form ([Web README](../../web/README.md)) |
| `core.company.reachable` | Company | The same list for an agent, under `admin.company.view`; reviewed writes capture one proved workplace for the whole plan |

The dashboard's Companies strip and Current company card follow `current_company()`. The switcher lists every live desk by name, with a type-to-find box past eight. Viewing as another account starts from that account's remembered or first valid desk, and leaving restores the operator's choice. Archived companies are never offered, and switching desks never crosses a tenant.

## Public company summaries

`organisation::Directory` returns `bilibi_tenancy::NamedOrganisation` with the company's `id`, display `name`, full `status` and existing `archived` flag.

| Field | Meaning |
| --- | --- |
| `status` | Stored lifecycle state: `active`, `pending`, `suspended` or `archived` |
| `archived` | `true` exactly when `status == "archived"`; retained for existing readers |

All four directory methods (`in_scope`, `selectable`, `target`, `lookup`) carry the status. They retain their existing tenant, capability and reach rules. `selectable` excludes archived companies but still includes pending and suspended companies when otherwise permitted. Status grants no authority and does not change account eligibility.

Domains that need an active-company chooser can carry `summary.status` through their own summary and filter with `summary.status == "active"`. Keep authorization at the existing directory or owner boundary; do not query Company's tables. This Platform change supplies the status only; each Domain owns its chooser policy.

Existing readers can keep using `archived`. A custom `OrganisationDirectory` implementation that constructs `NamedOrganisation` must also set `status` from its company lifecycle state and keep `archived` consistent. Exhaustive destructuring must include `status` or `..`.

## Reads in another module's transaction

**Pass your existing connection to Company when previewing or saving a Finance customer.** These reads take `&dyn bilibi_tenancy::ReadClient`, so an operation preview and a mutation transaction use the same API. They never acquire a pooled connection or commit the caller's transaction.

| Function | Result and access |
| --- | --- |
| `preview_create(client, scope, new)` | Normalized company fields and `CreateMatches` under `admin.company.create`, using the create page's validation, references, restrictions and matching rules. Returns `Ok(Ok(CreatePreview))`, `Ok(Err(Vec<FieldError>))` for invalid fields, or an outer access/database error |
| `preview_create_with_fields(client, scope, new, fields)` | The same preview with Extension fields, including their restrictions and current active pick-list choices |
| `create_matches(client, scope, new)` | Duplicate suggestions under `admin.company.create`: `likely` remains available without list access; `candidates` is `None` unless the caller also holds `admin.company.list` |
| `duplicates(client, scope, query)` | Scored duplicate candidates under `admin.company.list`, with the same bounds and matching settings as the create page |
| `detail(client, scope, company_id)` | Company facts and restricted field names under `admin.company.view`; masks match the ordinary detail page, including Extension field aliases. A missing, deleted or other-tenant company is `NotFound` |
| `relationship_role_by_code(client, scope, code)` | `Option<RelationshipRole>` under `admin.company.view`. The tenant comes only from `scope`. Retired entries remain visible with `retired_on`; this read never installs defaults |
| `current_relationship(client, scope, source, target, role)` | `Option<Relationship>` for that directed pair and role under `admin.company.view`. Both dates include their endpoints; null dates are unbounded. Future, ended and unlinked intervals do not qualify. Overlaps choose the latest start, then highest id. Role retirement keeps existing links effective |

A relationship grants no access. Company facts and relationship reads retain the existing view capability and tenant boundaries; writes still require company reach. Use [`bilibi_address::billing_address`](../address/README.md#billing-address-in-another-modules-transaction) to freeze an address for a document date.

**Preview on the connection you already hold, then pass the reviewed input to `create` in your mutation transaction.** `CreatePreview::company` contains trimmed values, the chosen or generated fixed code and initial status; `CreatePreview::matches` contains duplicate suggestions. The Core create page and `core.company.create` agent preview call this same implementation. Uncommitted companies, reference changes, restrictions and settings are visible through the caller's transaction.

Preview does not reserve a code, lock a choice or approve a duplicate override. Duplicate suggestions are advisory, including when `company.duplicates.on_create` is `require_reason`; the page can show candidates before asking for a reason. At save, `create` rechecks authorization, references, code uniqueness and duplicate review under its existing locks. Supply the reviewed candidate ids and reason through `NewCompany::create_anyway` when creating despite matches. A caller without list access supplies a reason without candidate ids. `CompanyService::create_matches` and `CompanyService::duplicates` remain convenient when the caller holds no connection; use the crate-root functions when you already hold one.

`bilibi_company::create(tx, scope, new)` composes company creation in the caller's `MutationTransaction`. It keeps validation, duplicate review (`NewCompany::create_anyway`), field restrictions, reference checks, locks and audit capture. The caller commits or rolls back the whole operation. There is no second transaction.

To inspect uncommitted reads, rollback, dates, masking, direct denies and reads with every pool connection held, use a freshly migrated disposable database. Initialize its Dev instance role through the production seeds before running the walkthrough; ordinary grant changes otherwise require an enrolled second factor:

```sh
BILIBI_ENV=development cargo run -p bilibi-web --locked -- seed-production
BILIBI_CALLER_READS=disposable cargo run -p bilibi-company --example caller-reads-walkthrough --locked
```

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
  uniqueness on the write's transaction under a per-code advisory lock. Automatic suggestions are allocated under the tenant lock.
- `bilibi_company::require_writable` is the one archived check: a live company
  whose status is `archived` is read-only for good, for its own facts and
  every record it owns. Reads never ask it.
- `organisation::Provider` is the `OrganisationProvider` Tenancy's
  `PartyProofs` proves targets with: a live, unarchived company of the
  actor's tenant, bound at the service factory.
- `bilibi_company::Directory` is Tenancy's `OrganisationDirectory` (K15), bound
  beside it: the companies a Base picker may offer (`in_scope`: every live
  company, archived flagged; `selectable`: the unarchived ones the actor may
  target with a capability, current company or tenant-wide) and `target`, the
  `authorize_target` rule on the sealed proof connection. The settings
  editor's company picker and the Authz role create picker read it; Base
  never queries `companies`. Migration `006-authz-role-owner` contributes the
  scoped foreign key behind Authz's `roles.company_id`.
- `files` (K15) is the private-file owner: the Media attachment subject
  `core.company` and the Artifacts owner `core.company` (`bilibi_company::FILES`).
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

`agent_operations.rs` registers `core.company.list`, `core.company.get`,
`core.company.reachable` (the companies the caller may work in, see [The
current company](#the-current-company)), `core.company.duplicates` and
`core.company.hierarchy` (reads; see
[Duplicate candidates](#duplicate-candidates) and
[Company hierarchies](#company-hierarchies)) and `core.company.create`
(a reviewed safe write: preview shares create's validation and code check, then reserves its code within the ordered plan; commit re-previews each operation and writes through the plan's transaction), plus `core.company.update` (a reviewed
custom-field-only write using the supplied Company version, a nonempty `fields`
patch, null to clear an optional field, and commit-time reach, restriction,
version and value checks). Preview requires Company view permission before it
shows before/after values;
the result returns the Company ID, new version and path for Get readback. The
`core.company` guide documents both. A restricted
field arrives as `{"restricted": true}`. The handlers read through a pool
bound once by the service factory.

## Static Extension-owned fields

Company accepts build-time `CompanyFieldContribution` entries on
`core/company.company_fields`. An Extension depends on Company and contributes
stable, globally unique lowercase field IDs and labels; Company never depends
on the Extension. Discovery rejects duplicate IDs, built-in collisions and
unsupported lengths before the app starts. The contract supports optional text fields up to 10,000 characters so JSON-escaped values fit in a Company live-form event frame, and optional calendar dates stored by the contributing Extension as PostgreSQL `DATE` values and passed as canonical `YYYY-MM-DD` strings.

The Extension owns its field table and migration. Its Grid contribution declares
the tenant-scoped source and a direct `One` link with an `On` join from
`companies`; its `RecordFields` contribution supplies the direct
`extension_table.field` Grid path and detail label. Company
uses the static field declaration for create and detail controls, and calls the
owner's scoped read callback for Company pages and agent reads. The callback
receives the read-only `OperationClient`, so reviewed previews use the plan's
existing transaction rather than checking out a second database connection.
The owner must bind the sealed scope's tenant in every read and write. Direct links let Company
join each contributor source once for list sorting and filtering. Each contributor table must enforce a unique Company key (for example, `UNIQUE (tenant_id, company_id)`); otherwise the join can duplicate Company rows and inflate counts.

Create and update callbacks run inside Company's mutation transaction through a
borrowed SQL transaction and return field values before and after their write. Company
captures those values as a `company` audit change; an inline update also
increments the Company record version. Callbacks do not open or commit a
transaction and do not record audit rows themselves. Company rechecks reach
and current field restrictions before invoking a write. A restriction on either
the Company field ID or its Extension table field ID masks the Company value and
refuses writes. Authz maps both identities to the Company's audit kind and the
Extension table's declared audit kinds, so the same value is withheld under
either record identity, including when the target field is protected from new
restrictions. Grid prunes the corresponding dotted path from each fresh scoped
catalog.

Contributed inline saves call `core.company.update` through `PageContracts`,
the same operation agents use. Only the remaining built-in fact, activity,
lifecycle and create events retain the page's pending write allowance. Empty
controls send `null` to clear an optional field; agent text and choice inputs
also normalize an empty string to `null`. The page and agent therefore share
the operation's value semantics.

The same fields appear in the reviewed `core.company.create` and
`core.company.update` schemas (`format: date` for dates) and are accepted by
their preview and commit handlers. The update schema nests only installed
field IDs under `fields`, declares optional fields nullable for explicit
clearing, and never allows built-in Company facts in that patch. Unknown keys,
empty patches and invalid values are refused. A choice field names a standalone tenant pick-list declared by that same Extension; dependent lists are rejected at discovery because Company has no parent selector for them. Company stores only the stable code and uses `base/picklists` for fresh tenant-scoped choices, defaults, localized labels and transactional write checks. The agent schema describes the stable code and pick-list key without freezing tenant values. Create, inline updates and reviewed agent updates recheck assignments in Company's mutation transaction; retired current values remain readable with a retired marker, unchanged retired choices may be retained, and retired codes cannot be newly assigned. Grid projects the stored code and renders its current pick-list label. Choice sorting and filtering are described below. Date fields use native date controls on create and detail; detail displays dates as `DD/MM/YYYY` while the editor and agent API use canonical `YYYY-MM-DD`. List and detail agent output uses
the same field IDs, with restricted values replaced by the shared restricted
marker. Runtime tenant-defined fields, JSON storage and reference fields are
not part of this static contract.

### Upgrade notes for field contributors

`CompanyFieldKind` now includes `Date` and `Choice`. Update exhaustive matches. Date contributions use canonical `YYYY-MM-DD` calendar dates backed by PostgreSQL `DATE` columns. A choice contribution names a standalone tenant pick-list declared by the same Extension; add the `base/picklists` dependency, declare the list with stable codes, and contribute its Production reconcile seed. Company stores those codes and resolves their current labels through the PickLists facade; do not add a field-owned options table or freeze tenant codes into the agent schema.

Read callbacks now receive `&dyn bilibi_agent_api::OperationClient` instead of
`&Client`. Use its read methods and do not check out another pool connection;
the adapter keeps reviewed preview reads on the plan's existing transaction.

Existing callers of `CompanyService::create_with_context` must pass the new
`CompanyFieldValues` argument between `new` and `activities` (use an empty map
when no contributed values are supplied). Owners constructing Authz
`CatalogField` values must also populate `restriction_aliases` with the actual
target table, field identity and that table's `record_types`; for linked Grid
paths, resolve the path instead of treating its first segment as a table ID.
Write callbacks must return exactly the values supplied by Company; callbacks
that normalize a reviewed value are refused and rolled back. Callers of Grid's
`PageColumns::with_record_slots` must handle its `Result` and propagate or map
invalid and duplicate slot errors. Callers that construct `InlineField` with a
struct literal must initialize `display_value` and `input_type`; prefer
`InlineField::new` with its builder methods. Contributed field IDs must not use Company
form transport keys or Company agent request/response envelope keys; discovery
rejects those names before the app starts.

## Contributed field sorting and filters

The Company list sorts contributed text, date and governed choice fields from their headings. Choice order uses stable codes, while cells retain current labels; missing values sort last and Company ID breaks ties. Only declared Company scalar contributions are eligible, not arbitrary linked or aggregate columns.

Add up to ten field filters, combined with AND and the existing search/status filters. Text offers contains (case-insensitive, literal text) and equals; dates offer on, before, on or before, after, and on or after; choices offer equality against a current active code. Retired stored choices remain readable but cannot be selected for a new filter. Date operands must be canonical `YYYY-MM-DD` in years 1–9999. Text operands are limited to 256 Unicode characters. The `field_filters` URL parameter is a JSON array of `{field, operator, value}` conditions, bounded to 16 KiB; `field` is the contribution ID and operators are `contains`, `eq`, `lt`, `lte`, `gt`, `gte` as supported by its type.

Each request resolves current restrictions on both Company and Extension field identities and a fresh Grid catalog. Unavailable fields are omitted from the query with a generic notice, and an unavailable sort returns to name ascending. Invalid drafts remain visible with per-filter feedback and are not executed. Count and row statements each pass Grid's cost admission and run under its two-second statement and 500 ms lock limits. A limited result says the list query hit its bound instead of showing the no-companies message. A disposable walkthrough sorted and filtered contributed fields with 50,062 Companies; larger or more complex queries may be refused by the shared cost and time bounds.

API upgrade: `list` takes `&mut tokio_postgres::Client` to own its read-only transaction and applies its query bounds to all callers. `Params` adds `field_filters` (empty by default); `Page` adds filter feedback, effective sort and the limit-refusal flag. Callers constructing `Params` explicitly must supply the new field or use its default. Contributor storage and field declaration interfaces are unchanged.

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
Open company). The current company is the one the session works in
(`Scope::current_company`: the chosen, remembered or first valid desk). An
account with no live desk sees "No company" and is told to ask an
administrator. No account is shown a guessed company. The widget and section need no
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
(`bilibi_company::authorize_target`), locks the source and an organisation
target ascending through `bilibi_company::lock_account_company` (so an archive
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
  as history. Every opening and write asks `bilibi_company::authorize_target`
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
with an expected version for a dialog. `apply_lifecycle` shares an existing
`MutationTransaction` for headless/owner composition. The transition table
is `LifecycleOperation`; `lifecycle_operations` returns the offered actions in
activate/reactivate/suspend/archive order. Pending → activate → active;
active → suspend → suspended; suspended → reactivate → active. Archive
accepts pending/active/suspended and is final. Creation still accepts only
pending/active. No generic field edit accepts `status`.

Each command requires `admin.company.update` and, for a company other than
`Scope::current_company()`, `admin.company.tenant-wide.manage`.
An explicit operator identity or admin grant does not bypass tenant scope,
standing checks, field restrictions, or the final archive state. Archive
and suspend refuse the tenant's primary company and the current company.
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

`bilibi_company::account_eligibility` supplies the company fact to the User
account binder (`bilibi_user::standing`) for login, session/bearer
validation and reset issue / redeem. Only archived/deleted/missing freezes an
account; pending and suspended do not. No role or business relationship confers
account eligibility. The binder calls this on every validation and caches no
derived account status; impersonation (K14) must do the same. Core remains
independent of People.

Account/affiliation/attached-owner writes take the company lock **before**
the child row and retain it until commit. Use `bilibi_company::lock_account_company`
for an account write and `lock_writable_companies` for moves / relationships
whose two companies must both be writable. The latter sorts and deduplicates
IDs before locking; use the same order in either direction. A write that wins
finishes before archive; a write that loses sees `WriteError::Archived`.
`bilibi_company::require_writable(..., false)` alone is a preview, never an
atomic write guard. Existing department writes already hold the company lock.
`bilibi_company::list_tenant_company_ids` includes archived and deleted IDs for
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
only from `lifecycle_operations`, hold the record version in server state,
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
from `available_department_types`, one event per status change, Set
Department Head, and Remove behind the confirm dialog. Every persisting event
passes one gate (`admin.company.update` now, then `require_writable`) before
the API's own checks, so an archived company answers in Bilimbi's words. The
detail page's Departments card (`views::departments_section`, built-ins
`department_builtins`, event `departments_grid`) leads here through Manage.

A department's head is an affiliation. Company cannot name one across the
module boundary, so `HeadDirectory` is the seam: the affiliation
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

`CompanyService::preview_tenant_companies` and `provision_new_tenant_in` compose tenant provisioning with [Core Tenant Administration](../tenant_administration/README.md). Names use ordinary Company validation, short generated codes and the person's field restrictions; duplicate suggestions receive numeric suffixes. The first company becomes primary.

The write requires Tenancy's sealed `CreatedTenant` receipt, bound to the current transaction and original actor. It accepts no arbitrary target tenant id and never changes the caller's scope. Tenant-create capability and live operator standing authorize the complete provisioning action; its contract enforces Base Step-up. Every company and primary assignment is a `Change` on that same transaction. Existing tenant business data remains behind its ordinary scoped facade.

## Postal addresses

Companies and contacts have linked postal addresses, owned by [Core Address](../address/README.md). The `core.address` guide explains saving an office or billing address and attaching it to a company or person. Use `core.address.company.list` or `core.address.person.list` to read those links. Address creation and attachment are reviewed writes; a namecard contact can be created first, then linked to its office address. An affiliation holds a position and work email/phone, not a postal address.

## Duplicate candidates

**Check `CompanyService::duplicates(scope, &DuplicateQuery)` before creating a company.** For example, a new “Acme Sdn. Bhd.” with the website of “ACME Berhad” returns that company at 0.90 with `website` in `matched_facts`. The read requires `admin.company.list`, includes archived companies with their status, excludes soft-deleted companies, and never chooses or merges records. It can exclude one company ID. Results contain only id, name, status, score and matched field names; a supplied criterion on a restricted field is refused before matching.

Migration `core-company-008-match-keys` adds stored generated `name_tokens`, `website_host`, `email_domain`, `phone_digits` and `registration_key`. Names use lower-case alphanumeric words and a GIN index; the other indexes lead with tenant ID and omit empty keys. Website hosts drop the scheme, `www.`, port and path. Registration keys keep only letters and digits, upper case. No PostgreSQL extension is required.

A candidate's score is its best matching fact:

| Fact | Score |
| --- | --- |
| Registration key | 1.00 |
| Website host or non-public email domain | 0.90 |
| Phone digits, at least seven | 0.85 |
| Name after removing configured words | Trigram intersection divided by union |

Name comparison runs in Rust over at most 200 rows preselected by overlapping indexed name tokens, the rows sharing the most tokens with the query first. Exact-key candidates are selected separately, so the name bound cannot hide a registration match. Results sort by descending score, then ID, at or above the threshold; `limit` is 1–100, default 10. Public email domains such as Gmail never count as an email match.

Create Company shows candidates below the form. **Use existing** opens the record; **Create anyway** submits the form's current values and opens a confirmation for the reason. `NewCompany::create_anyway` carries `CreateAnyway { candidate_ids, reason }` for pages, agent writes and other module callers. The reason is trimmed and must contain 1–500 characters. The create's audit `Change` records the reason and IDs under `create_anyway`; this read-only context does not become a company scalar field.

In `suggest` mode, a create proceeds with or without an override, and a supplied reason is kept as given. In `require_reason` mode, the transaction reruns matching and, if candidates exist, requires an override whose IDs name every returned candidate and no others. Creates serialize on the tenant before company locks and take an advisory lock on the lower-case trimmed name. A missing reason or changed candidate list returns a `create_anyway` field error.

The create path checks under `admin.company.create`, not `admin.company.list`. `CompanyService::create_matches` returns the scored candidates only to a caller who also holds list; any other caller learns only that likely duplicates exist (`likely`), with no names, IDs or count, and under `require_reason` gives a reason without naming candidates. `core.company.create` previews `duplicate_candidates` (null without list) and `duplicates_likely`; `core.company.duplicates` exposes the list-gated read for agents.

## Company hierarchies

**Use `CompanyService::hierarchy(scope, id)` or `core.company.hierarchy` to read relatives.** Both require `admin.company.view`, use current Company's tenant-wide read visibility, and return id, name, status and level. Ancestors are root first and keep their true depth from the root; when the limit cuts the chain, the nearest ancestors stay with their real levels and `truncated` is set. Descendant levels count the requested company as level 1. The read caps traversal at the tenant's depth limit and the returned relatives at 1,000 nodes, with `truncated` when more exist. The requested company is not repeated in either list. The record page links its parent chain, starting with “… N higher levels” when the limit hid some.

Every ordinary create with a parent and every parent edit shares the tenant lock and the same hierarchy checks. A loop returns `Parent company cannot be one of its own subsidiaries.` The new parent's chain depth plus the moving company's subtree height must fit the limit. The default refusal is `Parent company would make the company tree deeper than 10 levels.` Existing deep trees can remain or become shallower; a move is refused only if it increases depth beyond the limit. `save_fact` returns `WriteError::Invalid` with these field messages, preserving the inline draft.

The operator can edit these tenant settings under Companies in the shared Settings editor:

| Key | Default | Allowed values |
| --- | --- | --- |
| `company.duplicates.threshold` | 0.80 | 0.50–1.00 |
| `company.duplicates.on_create` | `suggest` | `suggest`, `require_reason` |
| `company.duplicates.ignored_words` | `sdn`, `bhd`, `ltd`, `inc`, `llc`, `gmbh` | Up to 200 words |
| `company.duplicates.public_mail_domains` | Common public mail providers | Up to 200 domains |
| `company.hierarchy.max_depth` | 10 | 2–20 levels |

For the fixed code and name of an already authorized linked record, and bounded code lookup across Company and Person, use the [shared party identity facade](../../base/tenancy/README.md#linked-party-identities-and-code-lookup). Company's adapter owns its tenant-scoped SQL and excludes removed companies; contact and legal-identifier reads keep their existing permissions.

## Linked agent steps

`core.company.create` declares its newly created company for same-plan references. Relationship assignment accepts a new source company or target company/person, checks the existing role, dates and reachable records during review, and retains its complete commit checks. See [same-plan record references](../../base/agent_api/README.md#new-records-in-later-steps) for the wire format and owner contract.
