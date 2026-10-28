# Affiliation owner (K18)

`AffiliationService` (`core/affiliation.affiliations`) owns `person_affiliations`.
Use its `read`, bounded `search`, `create`, `update`, `end` and `unlink` APIs.
They refresh actor/tenant eligibility and recheck contributed
`admin.affiliation.*` capabilities. HTTP/session and bearer adapters still
revalidate their credential on each call. The transaction functions in
`src/identity.rs` let authorized adapters participate in an existing audited
mutation; their caller must supply a freshly verified Scope and matching
mutation context. No API accepts a client-selected tenant.

The additive migration preserves all existing Company, Person and account IDs.
It uses K0 `AffiliationId`, `AffiliationInput`, `PersonId`, `OrganisationId` and
inclusive `EffectiveDates`. Person and Organisation foreign keys include tenant;
`(tenant_id,id,person_id,organisation_id)` is also unique for future scoped
account/Employment references. There is no uniqueness on Person, Organisation,
position or email. A Person can hold several concurrent affiliations, including
several positions at one organisation. Work contacts never replace personal
contacts, merge identities or create accounts/grants. No HR fields or People
dependency are installed.

Optional title/work contacts are trimmed and blank becomes null. Title permits
160 characters, phone 64; email uses the Person owner's shape/254-byte validator.
K0's calendar parser rejects impossible/reversed dates; SQL repeats the date
range and ordering constraints. Null date bounds mean unbounded. General search
is paged 1–100 and can filter Person, Organisation and an inclusive calendar day.
Without a day filter, it returns dated history. It excludes unlinked records
unless `include_unlinked` is explicit, and never searches hidden contacts.

Updates require the expected version. The Person/Organisation endpoints are
immutable because account and Employment references retain this identity.
Create another affiliation to move a person to a different organisation.
`end` sets the inclusive end date; `unlink` marks the retained record without
DELETE, cascade or lost foreign keys. Neither erases contacts or history.
Unlinked records remain readable but refuse edits and new binding proofs.
Reads retain ended, retired-Person and archived-company history; writes refuse
archived/deleted/missing companies and retired or protected system Persons.
Owners supply those rules through their public locking APIs.
Every write, and its preview, also authorizes the affiliation's company as a company write would (`bilibi_company::authorize_target` with the operation's own capability): the actor's own company, or any live company of the tenant with `admin.company.tenant-wide.manage`. A company out of reach is `Forbidden`; a missing or other-tenant company is `NotFound`.

Grid/Authz register work email and phone restrictions under
`person_affiliations`. Read/list results remove those values and name them in
`restricted`; a restricted value must be absent/null/blank on update, and the
stored contact is preserved. Title/date edits therefore work without exposing
or erasing hidden contacts. Audit snapshots flatten work fields so the same
catalog restrictions cover audit evidence. Mutations and actions commit in one
`MutationTransaction`; failure rolls back both and publishes nothing.
`subscribe()` publishes tenant, Affiliation, Person and Organisation IDs only
after commit. Consumers filter by tenant and reauthorize; fan-out is in process.
Existing installations run `bilibi-web authz-reconcile` explicitly to add the
new capabilities to contributed roles. Manager/tenant-owner receive all
commands, staff create/update, viewer read/list. Roles never follow from a
business affiliation.

## Contact directory columns

Contacts shows Work email, Role and Company from current affiliations by default. The column picker can add or remove them, and search matches these values plus work phone. Personal contacts stay separate.

The owner contributes Grid's `persons.work` many-link and `person_affiliations.company` one-link. The work link excludes unlinked, upcoming and ended affiliations, using inclusive dates on the UTC calendar. Multiple current affiliations roll up distinct values rather than choosing one. Grid checks affiliation and company list access and work-contact restrictions for both columns and search. History remains available in the record's Affiliations panel.

## Account and People integration

Call `AffiliationService::lock_for_account_binding` inside the consumer's
`MutationTransaction`. Its sealed, non-cloneable, non-deserializable
`LockedAffiliation` exposes tenant, acting user, Affiliation, Person and
Organisation IDs without contacts. Validate every supplied Person/Organisation
against these owner-issued IDs and store scoped foreign keys; do not accept a
DTO or matching integer as a proof. The caller independently authorizes its
account operation and holds this transaction through the account write.

People calls `AffiliationService::lock_for_employment` instead. Its distinct
`OwnCompanyAffiliation` is issued only if the affiliation's Organisation equals
the tenant's explicit `tenant_primary_companies` assignment. It never infers our
company from an ID, the first company, actor labels or an external affiliation.
The tenant row lock fences primary-company transfers; owner locks fence company
archive, Person retirement and affiliation update/unlink until commit. Missing
primary assignment, external affiliation, retired/system Person, archived
company, unlinked affiliation or cross-tenant identity refuses the proof.

Employment at any company of the tenant takes
`AffiliationService::lock_for_employment_at(tx, scope, affiliation, company)`
(PP2). People names the company its employment record will carry; the proof
authorizes it as a company write would (`bilibi_company::authorize_target` with
`admin.affiliation.prove`: the actor's own company, or any live company of
the tenant with `admin.company.tenant-wide.manage`), locks it writable, and
then issues the same `OwnCompanyAffiliation` only if the affiliation is linked
and belongs to that company and its Person is neither retired nor system.
`identity().organisation()` is the proven company; store that, never the
requested integer. A missing or other-tenant company is `NotFound`; an
archived company, a company outside the actor's reach, an affiliation of
another company, an unlinked affiliation and a retired or system Person are
`Forbidden`. The primary-only `lock_for_employment` stays for callers that
mean the tenant's own company and no other.

Use global lock order tenant (for both Employment proofs) → Company → Person →
Affiliation → User/Employment. When locking multiple identities, sort Company
and Person IDs first. Call the proof service before locking User/Employment.
The proof borrows the outer transaction; consume/check its identity before
mutably borrowing the transaction to record the downstream write. Dropping the
proof releases the Rust borrow, **not** the database locks. Never commit the
proof transaction and write in another one. People still authorizes Employment
and owns its own HR dates/status; affiliation dates do not determine employment
status. Ended affiliation identity remains provable while linked so historical
Employment can refer to it. People must decide its own date eligibility.

K18 adds no page or separate authorization policy; the panels and headless
operations below (K19) reuse these owner APIs. Core contacts and work
affiliations work with People absent.

## Personal data

**Erasure keeps each affiliation, its company link and dates, and clears job title, work email and work phone.** Store `core/affiliation.affiliations` under [`base/personal_data`](../../base/personal_data/README.md), kind `affiliation`, [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md).

- The company's records stay whole.
- **Masking.** A masked `work_email` or `work_phone` holds its partial value, named in `masked` in the read model and JSON, and is read-only in the panel. Writes refuse it.

## Runtime walkthrough (no unit tests)

Migrate a disposable DB, then run:

```sh
BILIBI_AFFILIATION_DEMO=disposable cargo run -p bilibi-affiliation --example affiliation-walkthrough --locked
```

The command uses real services and prints concurrent external positions,
normalization, independent personal contacts, paged panels, proof refusals,
the sibling-company proof and its reach, company, archive, unlink and retirement refusals,
explicit primary reassignment, tenant isolation, stale saves, field restrictions,
live capability revocation, atomic audit failure, rollback event suppression,
end/unlink retention and archived/retired owner guards. It creates no account,
People table or unit-test infrastructure. Use a fresh disposable database for
repeatable records; do not run the fixture on a shared working database.

## Person and Company panels (K19)

`src/web.rs` contributes two discovered record embeds, `person.affiliations`
(`Record { kind: "core/person" }`) and `company.affiliations`
(`Record { kind: "core/company" }`), gated on `admin.affiliation.list`. Person
detail mounts every `core/person` record embed; Company detail mounts every
`core/company` one through `record_panels` in `apps/core/company/src/web.rs`,
the counterpart of Bilimbi's `<.discovered_panel key="company.employees">`.
The host passes its own proven record ID (`id`) and a stable anchor (`panel`);
the panel never guesses a company-to-person link and reauthorizes every read
and write through `AffiliationService`.

Both panels are one shared FlexTable in a `p-0` card under the section heading,
as Bilimbi's company employees panel and employee subordinates section are:
the other party as a record link, title, work email, work phone, from, to and
status, with `ListColumns` show/hide, drag reorder, row height and Compact/
Normal presets, heading sort on party, title, dates and status, a search over
the listed rows, 25/50/100/300-row pages and a History filter. "Current"
lists affiliations in effect on the display calendar's day through the
owner's `on` filter; "All dated history" includes ended, upcoming and
unlinked records. The panel reads at most 300 rows before filtering in memory.

With `admin.affiliation.update`, title, work contacts and ISO dates edit in
place through the shared inline editors with per-fact commit outcomes:
refused shapes, stale versions and hidden contacts keep the stored value, and
a hidden work contact renders the shared restricted marker. "+ Affiliation"
(`admin.affiliation.create`, host writable) opens an add box whose company or
person combobox offers up to 300 live candidates; End opens the shared modal
with an inclusive last day defaulting to today; Unlink
(`admin.affiliation.unlink`) uses the shared confirmation. Unlinked rows are
read-only and keep their history. A viewer sees the list only; staff see add
and inline edits without Unlink. Panels refresh after the owner's committed
changes through `subscribe()`.

`core.affiliation.list`, `.get`, `.create`, `.update`, `.end`, `.unlink` and
the `core.affiliation` guide are contributed to Agent API
(`src/agent_operations.rs`). Writes are E2 safe writes: previews run the
owner's authorization, shape, restriction and owner-eligibility checks without
locking (`identity::preview_create`, `identity::preview_write`) and commits
write inside the plan's audited transaction. Records flatten the work fields
and list hidden contacts as `{"restricted":true}`.

`core.affiliation.contact.create` is Bilimbi's `core.person.contact.create`
(the key carries this module's prefix, as the registry requires): a namecard
creates a new human Person (name only) and their affiliation (organisation,
title, work email/phone, dates) in one reviewed write. Its preview is the
Person owner's `preview` plus `identity::preview_contact`, which runs every
`preview_create` check except proving the Person the write creates; its
commit is the Person owner's `create` followed by `identity::create` on the
same transaction, so an archived company or a refused field rolls both back.
It needs `admin.person.create` and `admin.affiliation.create` and creates no
account, employment or grant. The result carries the person, the affiliation
and the person's `path`.

Migrate a disposable DB (the walkthrough installs its fixture), then run:

```sh
BILIBI_AFFILIATION_DEMO=disposable cargo run -p bilibi-affiliation --example k19-operations-walkthrough --locked
```

It prints discovery/describe/guide, a viewer's read-only reach, reviewed
create/update/end/unlink commits, replay refusal, day-filtered lists and the
reversed-date, stale-version, missing-company and missing-capability refusals
a reviewer sees before anything is written. For the screens, run the app,
open a person from `/contacts` or a company from `/companies` and use the
Affiliations card; the person page's panel and the company page's panel show
the same record from either side.

## Postal addresses

Companies and contacts have linked postal addresses, owned by [Core Address](../address/README.md). The `core.address` guide explains saving an office or billing address and attaching it to a company or person. Use `core.address.company.list` or `core.address.person.list` to read those links. Address creation and attachment are reviewed writes; a namecard contact can be created first, then linked to its office address. An affiliation holds a position and work email/phone, not a postal address.

`AffiliationService::create_in` composes the same authorized creation inside a caller-owned `MutationTransaction`, so atomic account provisioning never opens an inner transaction. It retains owner locks, validation and after-commit notifications.

`AffiliationService::provision_in` is the same creation for a system principal (`Context::system`) that has authorized it itself and just created the Person in that transaction. It checks no capability, locks only the company and refuses a user context.

`AffiliationService::update_provisioned_email_in` updates a provisioned work mailbox in its caller-owned system transaction. The caller proves provisioning authority; the owner rechecks both endpoints and live linkage, and preserves audit capture and notifications.
