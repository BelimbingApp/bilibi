# Person identity and personal contacts

`PersonService` (`core/person.persons`) and the transaction APIs the crate
root exports from the private `src/identity.rs` (`read_person`,
`search_persons`, `preview_person`, `create_person`,
`lock_for_account_binding`, `account_standing`) own `persons`. They use K0's `PersonInput`, `PersonKind` and
`PersonId`; an identity is never an employee or login account. Core works with
People absent. `/contacts` and `/contacts/{id}` (Bilimbi's Contacts) provide the capability-gated directory and inline record/contact editor. Creating a contact opens a dialog on the list; `/contacts/new` redirects there with that dialog open. The earlier `/persons` addresses answer with a permanent redirect to the Contacts addresses (`Moved` contributions in `src/contributions.rs`).

The additive module migration creates tenant-scoped bigint identities, personal
email/phone, explicit human/agent kind, version and creation/change metadata.
It changes no company or account ID. No HR table exists to map in Bilibi.
No employee number, employment state/dates/type, department, supervisor, account,
password or grant can be supplied in a Person input. It rejects unknown keys.

Names are trimmed and contain 1–160 characters; optional email/phone are trimmed
and blank becomes null. Email permits the source's `local@domain` shape, forbids
whitespace/control characters and is limited to 254 UTF-8 bytes; phone has a
64-character limit. Email/name are deliberately non-unique. `duplicates` matches
exact normalized name/email (case insensitive) or trimmed phone, bounds results
to 1–100, can exclude an existing ID and includes retired identities. It proposes
candidates without choosing, merging or linking them.

Every public read and write gates on contributed `admin.person.*` capabilities.
The tenant-owner role receives all Person commands; the walkthrough fixture's
manager does too, its staff can create and edit and its viewer can read/list. Existing installations run explicit
`bilibi-web authz-reconcile` after migration; the walkthrough explicitly
reconciles its disposable fixture before making calls.
`admin.person.system.manage` additionally requires the explicit platform operator.
No Person identity, agent kind or role declaration grants capabilities itself.

`persons` is registered with Grid and Authz. Personal email/phone restrictions
remove values from every read result, including duplicates and agent output.
General search only searches visible contacts; duplicate lookup refuses a
restricted contact criterion to avoid existence probes. A read model's
`restricted` names hidden fields; agent JSON returns `{"restricted":true}`.
The same catalog maps restrictions to Person audit evidence.

`update` replaces visible identity/contact fields and requires the expected
version. A missing/null/blank visible contact clears it. A restricted contact
must be missing/null/blank and is preserved, so an editor can rename a person
without learning or erasing hidden data. Row locks serialize writes; stale
versions fail. `retire` retains the identity, contacts and history while refusing
new edits/proofs/bindings. It is independent of People employment lifecycle.

Writes use `MutationTransaction::mutate`; old/new snapshots and the action
commit atomically. Audit failure rejects the write. `PersonService::subscribe`
receives tenant/Person ID notifications only after commit; dropping/rolling back
a transaction publishes nothing. Consumers filter tenant and reauthorize their
own reads; notifications contain no contacts. Broadcast is process local.

`core.person.list`, `.get`, `.duplicates`, `.create`, `.update`, `.retire` and
the `core.person` guide are contributed to Agent API. Writes use E2's reviewed
plans and their outer transaction, including replay protection. Agent input
schemas use the currently supported schema dialect: omit optional contacts on
create and use empty strings to clear visible contacts on update.

## System identity and optional account binding

`ensure_platform_orchestrator` explicitly provisions Lara Bilimbi as an agent
in the operator tenant, resolved by `platform-orchestrator`, never a numeric ID
or employee number. Provisioning serializes concurrent calls and returns the
same record without overwriting edits. Its system key is not a general create
field, its kind cannot become human, and it cannot retire or use the generic
account-binding seam. Generic agents are ordinary contacts. Provisioning creates
no credentials/account, no automatic login and no Authz system-principal grant.
The bootstrap administrator signs in under the operator's company through an
ordinary agent Person under its own name and an affiliation, created once per
bootstrap receipt by Core User (`bilibi_user::bootstrap`) through `create`;
the walkthrough fixture links its AI Agent account the same way.
The reserved key is globally unique. Moving the operator marker to another
tenant requires explicit identity/reference reconciliation; provisioning refuses
to manufacture a replacement in that tenant.

User owns optional account links (K9); Person does not add competing account
storage or depend on User. `bilibi_person::lock_for_account_binding` supplies the
small integration seam: a scoped, live, non-system Person locked on the caller's
mutation transaction. It returns a non-deserializable `BindingIdentity` borrowed
from that transaction with tenant, Person ID and kind, containing no contacts.
The caller authorizes its account operation and proves the Company/Affiliation
eligibility before storing a tenant-scoped optional FK. Keep locks in Company →
Person (ascending IDs) → Affiliation → User order and hold them through commit.
The seam never creates or implies a link, and retired/system identities refuse.
Person creation never writes users, sessions, credentials or grants.

Tenancy's `PartyProofs::person` uses this owner to prove live scoped Person IDs;
that proof conveys existence, not a capability or account binding. `files`
(K15) is the private-file owner: the Media attachment subject `core.person`
and the Artifacts owner `core.person` (`bilibi_person::FILES`), deciding on the sealed
proof connection with `admin.person.view` for reads, `.update` for writes and
`.retire` for purge, a person of the actor's tenant (unretired for a write),
and the filing company reproved through `PartyProofs`. Use these, not a
hand-rolled check, for any file attached to a person. Principal
Directory providers name only bounded, live Person/agent candidates within the
request tenant. Agent candidates must have `kind=agent`. Base never reads Core
tables directly; the providers contain the owning queries.

`bilibi_person::read_person(client, scope, id)` accepts `&dyn bilibi_tenancy::ReadClient`. Composed reads, including billing-address owner checks, keep the caller's connection and the same `admin.person.view` check and contact masks. Retired Persons remain readable.

## Runtime walkthrough without unit tests

Use a freshly migrated disposable database, then run:

```sh
BILIBI_PERSON_DEMO=disposable cargo run -p bilibi-person --example person-walkthrough --locked
```

The example calls real app services and prints records/refusals for normalization,
duplicate candidates, concurrent stale saves, cross-tenant IDs, masked contacts,
live capability revocation, retirement, system-agent protection, optional binding
locks, transactional rollback and a deliberately refused audit action. It also
executes create/update/retire through the real safe-write plan service and prints
replay refusal without printing credentials or plan codes. It creates a temporary
operator marker only in the disposable DB and removes it. No People module,
unit tests, test runner or mocks are involved.

## Person screens and record panels (K8)

The Contacts menu entry (`admin.contacts`) is a Core contact directory. It has no employee number,
employment type/status/dates, supervisor, department or automatic account
controls. Human/agent is identity kind; current/retired is identity availability.
Retirement preserves history and is independent of HR employment.

The directory composes shared `ListState` and Grid `PageColumns`, with bounded 25/50/100/300 row pages, search through visible contacts, kind and retired filters, name sorting, accepted kind/identity/creation-date URL sorts and stable ID ties. Search, filters, sort, pagination and Grid `cols/lens/z/since` state survive reload and Back; column arrangements use shared per-account Grid persistence. The default columns are Name, Work email, Role, Company, Personal email and Personal phone. Scalar identity/contact facts share their detail labels with Grid through the record-field schema; Affiliations contributes the work fields, including optional Work phone. Work details follow the affiliation owner's discovered Grid paths (`work.work_email:list`, `work.title:list`, `work.company.name:list`); multiple current affiliations roll up distinct values. Personal contacts come from the redacted Person read model, without an implicit work-contact fallback. Search also matches visible current work email, phone, role and company before pagination. Fresh restrictions prune both built-in and walked contact columns and suggestions, including on empty pages.

Creation opens a dialog on the Contacts list, the same shared modal that legal entity types use. The fields are the shared floating-label controls. It uses the owner validator, shared form errors and non-binding duplicate suggestions, and creates no login. A refused save stays in the dialog. A saved contact opens its page. Detail fields use shared inline text/choice
editors and per-fact commit states. Enter/blur commits, Escape cancels, nullable
contacts can clear, stale versions refuse, and lost write access keeps the page
with readonly facts and an explanatory outcome. Retirement uses a held record
version and shared confirmation dialog. History is the shared audit embed.

`person.contacts` is a reusable discovered embed. A company, affiliation or
account owner first resolves its relation through its public owner API, then
mounts it with `{panel: <stable anchor>, id: <Person ID>}`. It reauthorizes Person
reads and writes, masks restricted contacts, and edits only personal email and
phone. Never pass a company ID as a Person ID. K19 owns the company affiliation
list/editor and work contacts; no guessed company-to-person link exists here.

Person detail automatically mounts contributed embeds whose context is
`Record { kind: "core/person" }`, except its own `person.contacts`. The path
carries `panel`, `id` and `person_id`; anchors are stable and browser-owned.
Affiliation, Address, User and relationship owners can contribute their panels
without Person importing them or requiring People. Each panel is separately
guarded by the host. HTTP rendering leaves child anchors for live mounting.

The manual `person-contact-panel` example serves an independent record host at
`/person-contact-reference/{id}` so the shared contact embed can be tried outside
Person detail. Run it with a migrated disposable `DATABASE_URL` (it installs the walkthrough
fixture):

```sh
APP_ORIGIN=http://127.0.0.1:4499 BIND_ADDR=127.0.0.1:4499 \
  cargo run -p bilibi-person --example person-contact-panel --locked
```

Sign in, open the reference route with a Person ID created with the New contact dialog on `/contacts`,
edit a personal contact, and verify it in the ordinary Person page. The reference
host resolves the Person through its public API on every update; a production
company host additionally resolves its owned affiliation before supplying that ID.

The directory follows Bilimbi Contacts' FlexTable configuration, retaining the approved Person model's identity filters, name sort and additional 300-row page size. It omits HR columns; company work contacts come only from explicit current affiliations. Large pages load walked columns in
batches bounded by Grid's `query::MAX_ROWS`; they retain every visible row.
The customization notch offers show/hide, drag reorder, row-height steps,
Compact/Normal presets and Reset. The account's column order, lenses and row
height are saved automatically by shared `PageViews`; an explicit column URL
wins on load. Bilimbi documents no named/shared Grid views, so the persisted
account arrangement and shareable URL are the saved-view contract here too.

## Step-up

- Retiring a person is the Base Step-up operation `core.person.retire` (`src/step_up.rs`), off by default.
- A tenant that turns it on in `/authz/step-up` gets the account holder's code asked for both the page facade (`PersonService::retire`) and the agent write (`core.person.retire`).
- See `apps/base/step_up/README.md`.

## Postal addresses

Companies and contacts have linked postal addresses, owned by [Core Address](../address/README.md). The `core.address` guide explains saving an office or billing address and attaching it to a company or person. Use `core.address.company.list` or `core.address.person.list` to read those links. Address creation and attachment are reviewed writes; a namecard contact can be created first, then linked to its office address. An affiliation holds a position and work email/phone, not a postal address.

`PersonService::create_in` composes the same authorized creation inside a caller-owned `MutationTransaction`, so atomic account provisioning never opens an inner transaction. It retains validation, field policy and after-commit notifications.

`PersonService::provision_in` is the same creation for a system principal (`Context::system`) that has authorized it itself, such as company sign-in creating an account. It checks no capability and refuses a user context.

`PersonService::update_provisioned_contact_in` updates a provisioned Person in the same caller-owned system transaction as account provisioning. The caller proves its ownership; the owner retains validation, retirement checks, audit capture and notifications.
