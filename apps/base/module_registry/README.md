# Registry contracts

Module IDs are `base/<module>` and `core/<module>` for the Platform layers, and
`<name>/<module>`, where `<name>` is the Domain or Extension (both snake_case, `<name>` not `base`, `core`, `web`
or `src`) for Domains and Extensions, matching the mounted folders; a mounted
module names its migrations `<name>-<module>-NNN-<slug>`. The product
binary calls `verify_composition` with the generated `bilibi_domains::MODULES`
and `bilibi_extensions::MODULES` lists before any command, so a mounted crate
whose descriptor is missing, extra or wrongly layered refuses to boot. Examples
that link their own reference Extension descriptors skip that check.

`composition` tells a program what it is made of: the Platform version, its
commit and release tag, and each mounted Domain or Extension with its commit
and version. `build.rs` compiles those facts in from
`.local/composition/manifest.txt`, the pin `scripts/discover-modules.py --pin`
writes; without a pin only the Platform commit is known. It checks the pin
against the checkouts it compiles, and a pin that names another set of
containers, another Platform version, or other commits than the Platform and
Domain checkouts are at is reported stale rather than shown. The host passes
the same `MODULES` lists it gives `verify_composition`. The scheme is
`docs/versions.md`.

Deep modules register `Module` descriptors and `Contribution::new` entries with
`inventory`. Use these contracts rather than adding host-owned module lists or
reading another module's private storage. `Services::install` sorts and validates
the graph, validates contributions, builds services, checks adapter readiness,
and publishes a read-only `Services::catalog()` only after successful boot.

`src/contributions.rs` defines shared `Route`, `Embed`, `Moved`, `Seed` and
`PageWrites` payloads and the `ROUTES`, `EMBEDS`, `MOVED`, `SEEDS`,
`PAGE_WRITES` keys. `PageWrites` declares a page service's write events and
the operation contract each calls, or a listed allowance; its contract and
check live in `apps/base/agent_api/README.md`. Each entry
includes its stable contribution ID and
`Provenance { module, source: file!(), line: line!() }`. Duplicate IDs within
a consumer, unknown consumers/owners, incorrect payload types, invalid routes,
duplicate route patterns and seed versions, and unknown/undeclared service
references fail discovery. Routes describe method/path, access, framing and a
service operation. Embeds describe slot/context, access and a service operation.
A `Moved` entry keeps a renamed page's old path (`from`) answering with a
permanent redirect to the new one (`to`, naming only `from`'s parameters).
These are metadata contracts; Web's route/event adapter owns execution and must
apply guards. Seed execution and ledger tracking belong to Database; a seed
selects Production/Development and SQL or a named service operation.

A new consumer (Menu, Settings, Grid, Queue, Agent API, etc.) registers
`Consumer::new::<OwnerPayload>("base/owner.catalog", "base/owner", validator)`.
The owning validator sees the complete sorted `&[Entry]`, including an empty
catalog, and reads its typed payload with `entry.get::<OwnerPayload>()?`.
Validation must be deterministic and perform no IO. Contributors supply static
payloads; registry validation never calls Core business APIs. Catalogs preserve
provenance and enforce the same type when retrieved with `entries::<T>(key)`.
Consumers own semantic checks and cross-entry rules beyond generic duplicates.

Service factories may resolve only their own module and declared direct
module dependencies. `bootstrap.*` values are explicit host-supplied
infrastructure: the database pool (`bootstrap.database`) and the program's
`Composition` (`bootstrap.composition`, for Base Software). Base cannot depend upward on Core.
Base-owned provider boundaries can use `Ready` to reject unbound adapters after
all factories finish; `apps/base/notifications/src/lib.rs` demonstrates this.
Company records through that sink inside the company transaction. User binds
its adapter at boot, audits the notification inside the transaction, and emits
the bell after commit. Do not import User from Company or import Core from Base.

Company contributes its HTTP page routes, and User contributes the shell bell.
Web binds route services through `bilibi_ui::page::PageService` and enforces
access and framing before dispatch (see `apps/web/README.md`). Production seeds
are executed by `bilibi_database::seeds` through the host's explicit command.
No unit tests are used.

Menu is the first consumer: `base/menu.items` carries `MenuItem` payloads (see
`apps/base/menu/README.md`). PrincipalDirectory is bound through a service
factory instead, because its providers need live database access.
