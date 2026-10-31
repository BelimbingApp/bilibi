# Core party contract (K0)

The authoritative Rust wire types are `src/lib.rs`, `src/dates.rs`, and
`apps/base/party_ids/src/lib.rs`. `schema.json` (also `INPUT_SCHEMAS`) publishes
JSON Schema definitions for the four creation inputs; cross-field dates, UTF-8
email byte limits and owner authorization require the Rust/owner checks. Base holds only typed IDs so tenancy/authorization
can accept provider results without depending on Core. `core/party` publishes
contracts; it installs no tables, seeds, services or routes. Consumers use these
contracts rather than treating a Person ID as an account or employee ID.

This implements the approved port-plan model and the current requirements of
https://github.com/BelimbingApp/bilimbi/issues/947, inspected 2026-10-14. The issue
is open and has not published final replacement code/API names. Its current
Bilimbi sources use Employee, RelationshipType, and `company_relationship_types`.
The names below are the approved Rust seam, not a claim that those source modules
have already been renamed. A later upstream name change must be an explicit
alias/deprecation, not a second identity or taxonomy.

## Ownership and dependencies

| Entity | Schema/API owner | Table | Typed identity | Delivery |
| --- | --- | --- | --- | --- |
| Organisation/company | `core/company` | existing `companies` | `OrganisationId` | K1/K2 |
| Person | `core/person` | `persons` | `PersonId` | K7 |
| Affiliation | `core/affiliation` | `person_affiliations` | `AffiliationId` | K18 |
| Relationship role | `core/company` | `company_relationship_roles` | `RelationshipRoleId` | K5 |
| Directed relationship | `core/company` | `company_relationships` | `RelationshipId` | K5 |
| Login account/link | `core/user` | existing `users`; optional person/affiliation links | existing account ID | K9 |
| Employment | People domain | People-owned employment storage | refers to `AffiliationId` | outside K0 |

`OWNERSHIP` publishes these stable owner/table/operation names in Rust. Future
owners declare dependencies on `core/party`; Base imports `base/party_ids` only.
The DTO crate has no dependency on People, User, Company, or security. This avoids
introducing cycles when User later depends on Company/Person/Affiliation.

## Schema obligations for owning migrations

All IDs are positive PostgreSQL bigint values. Wire IDs are JSON integers (not
UUIDs or strings); Rust construction and deserialization reject zero/negative IDs.
IDs in different tables can have the same number. `PartyId` always retains its
`kind` discriminator. An ID or deserialized DTO is never an authorization proof.

Every owner table has a trusted `tenant_id`, `id`, `version >= 1`, creation/change
metadata and a unique `(tenant_id, id)`. Tenant is derived from the authorized
scope, absent from create/update input. Every relationship FK includes tenant:

- `person_affiliations(tenant_id, person_id)` → `persons(tenant_id, id)`;
  `(tenant_id, organisation_id)` → `companies(tenant_id, id)`.
- `company_relationships(tenant_id, source_organisation_id)` → companies;
  `(tenant_id, role_id)` → relationship roles.
- Store targets as nullable `target_organisation_id` and `target_person_id`, each
  with a scoped FK, plus an exactly-one-target CHECK. Translate to/from `PartyId`.
  Do not use an unvalidated polymorphic numeric FK.
- Account links use scoped FKs, are optional, and must prove that any linked
  affiliation belongs to the same Person and Organisation. Creating a Person
  must never create a user, credentials, grants or an account link.

Person stores a fixed tenant-unique Core code, name, human/agent kind and optional personal email/phone. The Person owner suggests a short code using `bilibi_party::default_code`, shared with Company; the person can choose a longer code at creation. Collisions get numeric suffixes. Use this Core code when a person becomes a customer or supplier. No employee
number, employment type/status/start/end, supervisor or HR department lives here.
Name must be nonblank and at most 160 characters; optional email is at most 254
bytes and valid when supplied; phone at most 64 characters. Normalize blank
optional contact values to null at the owner API. Email/name searches permit
candidate duplicates; do not infer identity from email or impose account login
uniqueness on contacts. Agent identity is explicit; it grants no login/access.

Affiliation stores Person, Organisation, optional title (160 characters), work
email/phone (same limits), and optional inclusive `effective_from/effective_to`
calendar dates. Null bounds mean unbounded. `EffectiveDates` rejects reversed
bounds and impossible dates (Gregorian YYYY-MM-DD, years 0001..9999). SQL repeats
`effective_to IS NULL OR effective_from IS NULL OR effective_from <= effective_to`.
Several affiliations per Person, including concurrent different organisations or
positions, are valid. Do not enforce unique(person_id) or merge affiliations by
contact/email. End/unlink retains history and account/Employment references.

Relationship roles store stable tenant-local unique nonblank code (64 characters),
nonblank name/inverse_name (160 characters),
retired_on, version. Default codes: vendor (supplier synonym), customer, prospect,
partner. No hardcoded default role numeric IDs. Seed/reconcile defaults by code
within each tenant. `source → target: vendor` means target supplies source;
customer means target buys from source; partner is symmetric in meaning but the
stored link still has explicit direction. Inverse labels are presentation, not
an automatic reverse row or authorization grant. An organisation or person may
be the target. Organisation self-links are refused by the owner. Different roles
may overlap for the same source and target; the same role may have separate dated
intervals. Reject exact duplicate assignment; never impose one role per pair.
Renaming keeps the role ID. Retirement forbids new assignments but retains
historical links. Ending/unlinking retains dated history, not cascade deletion.
Relationship role IDs cannot be used as Authz role IDs.

People Employment references an affiliation with **a company of our tenant** (the
primary one, or any the actor may target) and obtains a fresh owner-issued proof
under the actor scope. A client-provided AffiliationId,
DTO or matching integer is not that proof. K18/F3 implement the
sealed proof/provider seam and lock/revalidation boundary; K0 intentionally
publishes no forgeable `OwnCompanyAffiliation` constructor. People must enforce
scoped affiliation links and keep HR employment dates separate from affiliation
dates. Core remains usable when People is not installed.

## Owning API obligations

The listed methods are contracts for the delivery slices above, not callable
services in K0. Each read/search accepts a fresh authorized Scope and bounded
pagination; results exclude masked contact values. Each write accepts trusted
Scope, typed input, and expected version (for updates) in the owning transaction;
reject stale versions, cross-tenant targets, missing/archived owners and forbidden
fields, record audit atomically, publish changes after commit. UI and headless
adapters call the same methods. Neither accepts a client-selected tenant.

| Operation prefix | Required owner methods |
| --- | --- |
| `core.company` | search/read/create/update and explicit lifecycle commands |
| `core.person` | search/read/create/update; explicit retire, duplicate candidate lookup, optional agent identity |
| `core.affiliation` | search/read/create/update/end/unlink for Person or Organisation |
| `core.company.relationship_role` | search/read/create/update/retire |
| `core.company.relationship` | search/read/assign/update/end/unlink with tagged target and dates |

Creation inputs are `PersonInput`, `AffiliationInput`, `RelationshipRoleInput`,
`RelationshipInput`. They deny unknown fields, including HR/account/grant fields.
Their public strings still require owner semantic validation described above;
serde parsing is not a substitute for authorization/contact validation. Ownership
metadata is not operation registration. K1/K7/K18/K5 register real operations with
schemas/guides; E2 owns preview/plan/approval/commit semantics. No advertised
operation may be enabled until its live owner and capability gate exist.

`core.employee.*` is deprecated by explicit operation aliases/adapters only when
migration/link mapping is available. Do not alias employee_id to person_id by
cast, or let the old create operation manufacture HR defaults. Existing system
agents, user/employee links, integration and foreign keys need an explicit mapping.

## Legacy migration mapping

`Company::organisation_id()` exposes the typed identity on Company read
results without changing route/edit contracts: a company keeps its numeric ID,
tenant and `/companies/{id}` URL as its `OrganisationId`. Accounts are not
people; a Person link is never inferred from an account. K9 resolves accounts
that share an email across tenants before introducing globally unique account
login; never merge them silently.

Bilibi currently has no employee/HR tables to migrate. If legacy Bilimbi records
are imported later, stage a durable `(source_system, source_tenant, source_kind,
source_id) → (tenant_id, person_id, affiliation_id, account_id, employment_id)`
mapping, with source payload/checksum and reconciliation status. Preserve IDs
where unambiguous; explicitly map one employee record's identity and affiliation
when splitting it. Retain HR numbers/fields in a restricted durable staging store
when People is absent; do not put them in Core Person or discard them. Only mark
migration complete after every reference is remapped and HR data is reconciled.
No Laravel adoption or compatibility migration is introduced by this slice.

## Runtime inspection without unit tests

Run `cargo run -p bilibi-party --example default-code-contract` to check the role-aware short suggestions for companies and people.

Run `cargo run -p bilibi-party --example contact-contract` to print a deterministic
contact, two work affiliations and concurrent vendor/customer relationships, plus
ownership. These are contract specimens, not persisted
records or enabled endpoints. No People module or login/grant is involved.

Run the app against an isolated DB holding the walkthrough fixture
(`fixtures/walkthrough`) and inspect Companies 1 and 100 under their tenants:
company pages keep their URLs and accounts stay accounts.
Actual persistence, UI contact creation and sealed proofs land in the named
owner slices. No unit tests, test-only infrastructure or HR dependency belongs
in this contract module.
