# Core Address (K11)

`AddressService` (`core/address.addresses`) owns `addresses` and
`address_attachments`. Pages, panels and agent operations call its
APIs; no other module reads or writes these tables. Service calls refresh the
actor's scope and recheck contributed `admin.address.*` capabilities; no API
accepts a client-selected tenant. The `/addresses` pages and the two owner
panels (`src/web/`) are built on these APIs.

Base Records kind `address` names and links addresses through the same tenant, non-deleted and `admin.address.view` rules as the detail page.

## Operation contracts

Agents can find, save and link the same addresses shown on company and contact pages. Read [the address guide](guide.md) for a business-card workflow.

| Action | Contract |
| --- | --- |
| Search and read | `core.address.list`, `core.address.get` |
| Owner addresses and attach picker | `core.address.company.list`, `.available`; `core.address.person.list`, `.available` |
| Location choices and cascade | `core.address.location.options`, `.suggest` |
| Address facts | `core.address.create`, `.update`, `.delete` |
| Company links | `core.address.company.attach`, `.create_and_attach`, `.update_attachment`, `.detach` |
| Person links | `core.address.person.attach`, `.create_and_attach`, `.update_attachment`, `.detach` |

Write inputs nest facts under `address` or link settings under `attachment`. Owner writes name `company_id` or `person_id`; attaching an existing address requires its `address_version`, checked after locking; edits and deletion require the current record's `version`. Updates replace visible optional facts; omitted visible fields clear, while an omitted restricted phone stays stored. Timestamps in operation output are ISO UTC strings, and withheld phones use the restricted marker.

All doors use [reviewed writes](../../base/agent_api/README.md#reviewed-safe-writes). The handler calls the Address facade on the plan's transaction; existing owner locks, version fences, field restrictions, audit changes and after-commit notifications apply. Attachment contracts use the owner's update capability, matching the panels; they cannot cross owner kinds. A review that shows an existing address (attach, update, delete) also needs `admin.address.view` and applies the same field restrictions as a read. A create-and-attach starts a manual, unverified address and its date default appears in the preview. No additional sensitive action or step-up policy is introduced.

Page write events use `PageContracts`, with `Pending::None`. Delete and unlink retain a browser-bound preview code in their confirmation state. No address page or panel writes through a separate facade transaction.

## Address facts and normalization

An address holds label, phone, three street lines, locality, postcode, country
(`country_iso`) and division (`admin1_code`), plus provenance (`raw_input`,
`source`, `source_ref`, `parser_version`, `parse_confidence`) and
`verification_status` (`unverified`, `suggested`, `verified`). Bilimbi's quoted
camelCase columns are snake_case here.

- `AddressService::validate` is the only way to a `ValidAddress`, which every
  store write takes. It trims, turns blanks into `None`, uppercases the
  country, bounds lengths, checks the confidence is a 0–1 decimal with at most
  four places, then checks against Geonames that the country is imported and
  the division belongs to it. Postcode and locality stay free text.
  Failures are `WriteError::Invalid` with per-field messages for a form.
- Country and division are foreign keys into Geonames, which is empty on
  install: an address can have no country until an operator imports countries.
- `suggest`/`options` (`src/location.rs`) port Bilimbi's `LocationSuggestion`
  for the create form, the detail page's grouped Apply and a panel's
  create-and-attach form: a country change clears the dependents; a postcode
  change fills the division and a single unambiguous locality and clears what
  was only suggested; typed values are never replaced. Use them; do not
  re-implement the cascade in a page.

## Owner attachments

Bilimbi's polymorphic `addressables(type, id)` becomes one scoped foreign key
per party kind (K0 contract): an attachment names exactly one Organisation
(`companies`) or Person (`persons`) of the same tenant, with `PartyId` in the
API. One attachment per address and owner carries kinds (`headquarters`,
`billing`, `shipping`, `branch`, `other`), the primary flag, priority (0–32767)
and inclusive `valid_from`/`valid_to` (`EffectiveDates`, null = unbounded).
Several owners may share one address.

- Reads: `attached(owner)` (primary first, then priority, then id),
  `primary(owner)` (Bilimbi `primary_company_address`), `available(owner, q)`
  (the attach picker, 50 rows) and `detail(id)` with country/division names and
  every live owner named through its owner. Archived companies' and retired
  Persons' addresses stay readable; `detail` flags those owners `read_only`.
- Writes: `attach`, `create_and_attach` (a `manual`, `unverified` address
  starting today unless dated), `update_attachment` and `detach`. They are
  writes to the owner, so they need `admin.company.update` or
  `admin.person.update`, and go through the owner's public lock, which refuses
  archived companies (`Archived`) and retired or system Persons
  (`OwnerReadOnly`). Attaching twice is `AlreadyAttached`.
- `update` of an address any archived company uses is refused (`Archived`), as
  in Bilimbi. A Person does not freeze an address: in the party model a Person
  has no employer, and the address may be shared.
- `delete` is a soft delete, refused while any attachment remains (`InUse`).
- Updates take the expected version (`Stale` otherwise); attachments carry
  their own version. Lock order is Company (ascending) → Person → Address →
  Attachment; a write never locks a company after its address.

Work addresses: neither the K0/K18 contract (`apps/core/party/CONTRACT.md`) nor
issue 947 gives an Affiliation an address, so none is linked here. A work
address is the affiliated Organisation's attached address.

Grid/Authz register `phone` as restrictable under `addresses`; reads remove a
restricted phone and name it in `restricted`, a write must not supply it, and
the stored value is kept. Mutations (`address`, `address_attachment`) and the
action commit in one `MutationTransaction`. `subscribe()` publishes the tenant,
address and affected owners after commit only. Grid links: `addresses.country`,
`companies.addresses`, `companies.primary_address` and `persons.addresses`.
Existing installations run `bilibi-web authz-reconcile` to grant the new
capabilities to contributed roles: the tenant owner gets all of them. The
walkthrough fixture's manager gets all, staff all but delete, viewer view/list.

## Billing address in another module's transaction

**Call `bilibi_address::billing_address(client, scope, owner, as_of)` to freeze a billing address for a document date.** It takes `&dyn bilibi_tenancy::ReadClient`, a `PartyId` owner and a `CalendarDate`; every query uses the supplied connection, including authorization and phone restrictions.

The read requires `admin.address.view`. The owner must exist in the scope's tenant; a Person also requires `admin.person.view`, as with `attached`. Archived companies and retired Persons remain readable. A missing or other-tenant owner is `NotFound`.

Only attachments effective on `as_of` qualify: both endpoints are inclusive and null dates are unbounded. Among live addresses, a `billing` attachment wins, ordered by primary flag, priority, then address id. If no billing attachment qualifies, the same ordering selects the owner's primary address, falling back to the first address by priority. No qualifying address returns `None`. Deleted addresses never qualify, and a restricted phone is removed from the result.

The returned `AttachedAddress` includes the chosen attachment and masked address facts. The caller owns document snapshotting and transaction completion. Existing `attached` and `primary` reads keep their current behavior; this date-aware selection is explicit.

## Platform-operator locale bootstrap

`platform_operator_bootstrap()` (`src/bootstrap.rs`) returns the
`bilibi_locale::Bootstrap` facts for one-time installation-locale inference:
the live operator tenant's primary company, its first address (primary, then
priority, then id) and that country's Geonames name, languages and currency.
Any missing step is `None`, never an error. Base Locale does not query Core:
the service factory binds `bootstrap::Source` as Locale's `BootstrapSource`
(K15), so every `Locales::resolve(.., None)` infers from these facts until a
global locale is stored; `adopt_inferred` persists them once under operator
proof; the shell does so on the first operator page that finds these facts
(`adopt_installation_locale` in `apps/web/src/shell.rs`).

## Pages and owner panels (K12)

`src/web/` ports Bilimbi's `IndexLive`, `CreateLive`, `ShowLive` and
`AddressesPanel`; the Addresses menu entry sits under Administration.

- `/addresses`: search, sort (label, country, status) and 25/50/100-row pages
  in the URL under `search`/`sortBy`/`sortDir`/`perPage`, built-in
  `ListColumns` on the shared FlexTable, and delete confirmed on the listed
  record and its version. An attached address is refused (`InUse`).
- `/addresses/create`: the shared location fields (`src/web/location.rs`,
  which only draws what `suggest`/`options` decide) and provenance. Country
  comes first and postcode second, so the state and locality it suggests land
  below it. Street lines 2 and 3 wait behind "+ Add address line", provenance
  is folded under its one-line summary and opens on an error, and after
  Create the problems are listed above the form.
- `/addresses/{id}`: read first; text facts and the verification status
  commit in place, the four location facts commit together through Apply
  with the version the edit began from. Linked Entities sorts in the URL
  (`linked_sort_by`/`linked_sort_dir`); `?company=` or `?person=` naming a
  linked owner adds the way back. History is the shared audit embed.
- `company.addresses` / `person.addresses` embeds: Company and Person detail
  discover them by record kind and pass `company_id`/`person_id`. Attach,
  Create & Attach (both owners: a Person has no employer whose addresses it
  would borrow), kinds, primary, priority and confirmed unlink. Controls show
  only to an owner's updater while the owner is writable; every write is
  still refused by the service. Pages refresh on `subscribe()` changes.

## Personal data

**Erasure clears lines, phone, label, postcode, locality and raw input of an address only this person uses, and keeps its country.** Store `core/address.addresses` under [`base/personal_data`](../../base/personal_data/README.md), kind `address`, [ADR 05](../../../docs/architecture/decisions/05-personal-data-registry.md).

- An address another party also uses (a company office) is kept and reported.
- Attachments stay as links to the anonymised address.
- **Masking.** A masked `phone` holds its partial value, named in `masked`, keeps its list column and is read-only on the record. Writes refuse it.

## Runtime walkthrough (no unit tests)

On a fresh disposable database: `migrate`, then import the Geonames
sample (`apps/core/geonames/sample`, served by any static server with the
`export/dump` and `export/zip` layout, `cities15000.txt` and `MY.txt` zipped):

```sh
bilibi-web geonames-import --base-url http://127.0.0.1:PORT/dump \
  --postcodes-url http://127.0.0.1:PORT/zip --postcodes MY --cache-dir DIR
BILIBI_ADDRESS_DEMO=disposable cargo run -p bilibi-address --example address-walkthrough --locked
```

It prints normalization and refusals, the location cascade, CRUD with stale
and cross-tenant refusals, the phone restriction, Organisation and Person
attachments, primary resolution, the bootstrap facts before and after the
operator's address, archived-company and retired-Person freezes, refused and
soft delete, and audit counts. It marks tenant 1 as the platform operator when
none exists, so use it only on a disposable database.

The directory shares its detail-field vocabulary through `web::fields` and `ListColumns::with_record_fields`. Street lines, phone, location names and provenance scalars are optional columns; raw input remains long text on the record. The original five default columns stay in place. Phone restrictions refresh even when a search returns no rows.

## Linked agent steps

Company and person `create_and_attach` accept the appropriate record created earlier in the same plan. Their preview still validates address facts, lookup names, restrictions, attachment kinds, dates and defaults; the ordinary commit proves the new owner and writes both records. See [same-plan record references](../../base/agent_api/README.md#new-records-in-later-steps) for the wire format and owner contract.
