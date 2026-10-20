# Registry contracts

Module IDs are `base/<module>` and `core/<module>` for the Platform layers, and
`<name>/<module>`, where `<name>` is the Domain or Extension (both snake_case, `<name>` not `base`, `core`, `web`
or `src`) for Domains and Extensions, matching the mounted folders; a mounted
module names its migrations `<name>-<module>-NNN-<slug>`. The product
binary calls `verify_composition` with the generated `bilibi_domains::MODULES`
and `bilibi_extensions::MODULES` lists before any command, so a mounted crate
whose descriptor is missing, extra or wrongly layered refuses to boot. Examples
that link their own reference Extension descriptors skip that check.

`Composition` tells a program what it is made of: the Platform version, its commit and release tag, and each mounted Domain or Extension with its commit and version. The executable host builds it and supplies it as `bootstrap.composition`; consumers use that value.

The registry owns only the stable types, so a Git commit does not rebuild every registry consumer. Pin compilation and drift checks belong to `apps/web/build.rs` and `apps/web/src/composition.rs`. The scheme is `docs/versions.md`.

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

## Runtime checks

Page routing has no unit tests. Run these against an isolated database set up as in [docs/runtime-parity.md](../../../docs/runtime-parity.md). Register a small reference module through the normal inventory contracts, with its `PageService` rendering a recognizable operation label, in a separate fixture executable so production has no fixture routes.

| Case | Exercise | Expected observation |
| --- | --- | --- |
| Literal overlap | Contribute `/companies/reference` beside Company's `/companies/{id}` | Literal operation renders; `/companies/1` still renders Company detail. |
| Ambiguous overlap | Contribute GET `/dispatch/{one}/end` and `/dispatch/start/{two}` | Startup fails with both paths in the contribution error. |
| Host shadow | Contribute `/live` | Startup fails before binding the server. |
| Wrong service | Reference an existing service with a different concrete type | Startup fails with missing/wrong service type. |
| Signed in | Visit a SignedIn page anonymously, then with a valid session | Anonymous GET redirects to login; authenticated GET renders. |
| Capability | Visit a Capability page with a direct denial | 403; the page owner is not called. |
| Operator | Visit OperatorCapability before and after adding the explicit operator identity to this disposable tenant | 403 before; allowed after only with the capability. Other tenants and explicit capability denials remain forbidden. |
| Unsafe method | POST with missing Origin, then missing CSRF, then both valid | 403, 403, then owner dispatch. |
| Shell isolation | Deny the capability a shell or dashboard embed requires (`companies.view` on `dashboard-reference`) and visit an allowed page | Page still renders; the denied embed reads no data. |
| Framing | Navigate an iframe to Companies | `#app-shell[data-framed=true] > main#app-content`, without menu, header or bell. One vertical scroller. |
| Frame refusal | Navigate an iframe to a route declaring `framed: false` | 403 before owner dispatch. A top-level query `?framed=true` does not hide the shell. |
| Live framing | Sort and wait for refresh inside the Company frame | Framed markup stays chromeless; typed URL state changes normally. |
| Forbidden mount | Connect an authorized socket, revoke companies.view, then send its mount | `Access denied`, `clear: true`, socket closes; no rows arrive. |

For download routes, serve a reference download on a database with the walkthrough fixture (`manager@example.test` holds `reference.exports.view` through the fixture manager role; `viewer@example.test` does not).

| Case | Exercise | Expected observation |
| --- | --- | --- |
| Attachment | Manager GETs `/reference/exports/1/csv` | 200, `text/csv; charset=utf-8`, `content-disposition: attachment; filename="…"; filename*=UTF-8''…`, `cache-control: private, no-store`, `nosniff`, `default-src 'none'; sandbox; frame-ancestors 'self'`. |
| Inline | Manager GETs `/reference/exports/1/print` | 200 `text/html` shown in the tab; the page's inline script is blocked by `default-src 'none'`. |
| Encoded name | GET a return whose name carries `é`, an em dash and quotes | ASCII fallback replaces them; `filename*` percent-encodes the UTF-8 name. |
| Anonymous | GET a download with no session | 303 to sign in; the owner is not called. |
| No capability | Viewer GETs a download | 403; the owner is not called. |
| Revoked grant | Manager downloads, then a direct deny (or the role's unassignment) is written, then the same session GETs again | 200, then 403 without a new sign-in and with no owner line in the server log; lifting the deny restores 200. |
| Missing file | Manager GETs `/reference/exports/9/csv` | Bare 404, no missing-record redirect. |
| Overlap | Contribute a download at a page's path, or at `/live` | Startup fails naming both paths. |
