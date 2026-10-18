# Architecture

**Document Type:** Architecture overview
**Status:** Active
**Scope:** Layers, modules, composition, data, audit and security of Bilibi. The UI rules are in [`DESIGN.md`](../../DESIGN.md).
**Last Updated:** 2026-10-18

Bilibi is a business application platform written in Rust: a set of small modules that plug into one web host, each owning its own data, pages and rules.

## What is Bilibi?

Bilibi is the Rust port of Bilimbi. It has two ancestors, and each gives it something different:

- **Belimbing** (Laravel) gives the business meaning: what a company, a person or an address *is*.
- **Bilimbi** (Phoenix) gives the behaviour, the page structure (DOM) and the module topology: how the screens act and how modules fit together.

Here is the whole idea in one example. Say you open the Create Address page:

1. The web host receives the request. It has no list of pages of its own.
2. The Address module registered that page when the program started, so the host knows who owns it.
3. The host checks you are allowed in, then hands the request to the Address module.
4. The Address module renders the page and, when you press Create, saves the address through its own code. The audit trail records the change in the same database transaction.

Notice what is **not** there:

- The host doesn't know what an address is.
- No other module touches the address tables.
- Nobody edited a central list to add the page.

That is the architecture in miniature: **modules own everything about their subject, and they plug themselves in.** The rest of this page explains each part.

You may already have questions: how do modules find each other, what stops one from reaching into another's data, how are tenants kept apart? Don't worry, we cover each of these below.

### How to read this page

- **Status marks:** *built* means the code exists and was checked. *planned* means it is not built yet.
- **Authority:** where this page and the code differ, **the code wins.** Fix this page.
- **Decisions:** each architectural decision has its own record in [`decisions/`](decisions/README.md). Start with [01 Agent-first operations](decisions/01-agent-first-operations.md).
- **Agents** also read [`AGENTS.md`](../../AGENTS.md).

## Layers

Bilibi's code sits in four layers inside one Cargo workspace, plus the web host that runs them. **Dependencies point downward only,** toward the foundation: a layer may use the layers listed before it in this table, never the ones after it.

| Layer | Path | Role | Mounted from |
| --- | --- | --- | --- |
| Base | `apps/base/<module>` | Infrastructure and cross-cutting mechanisms | this repository |
| Core | `apps/core/<module>` | The required enterprise domain | this repository |
| Domains | `apps/domains/<name>/<module>` | Optional business capabilities | one repository per Domain |
| Extensions | `apps/extensions/<name>/<module>` | Operator-chosen adaptations | one repository per Extension |
| Web | `apps/web` | Executable host: routes, sockets, seed command | this repository |

Think of it as a building. Base is the plumbing and wiring. Core is the rooms every company needs, such as companies, people and addresses. Domains are optional wings, such as Finance or Factory. Extensions are fittings one customer chose, such as a connector to their old system.

A few rules follow from that order:

- The order is fixed: Base, Core, Domains, Extensions.
- Base never depends on Core.
- Domains and Extensions are empty in this repository. Each one is mounted from its own repository. *built*: mounting, pinning and CI (`apps/domains/AGENTS.md`).
- The composition crates `apps/base`, `apps/core`, `apps/domains` and `apps/extensions` hold no business code. They are generated linkage only.

## Deep modules

A **module** is one Cargo crate with one **facade** (the single public front door other code calls). Callers use the facade and never see the module's tables, queries or files.

We call these "deep" modules: a small public surface hiding a lot of work behind it.

- **Facade:** the crate root exports one service and its public types. Everything else is private.
- **Ownership:** a module owns its migrations, SQL, templates, routes, README and registered contributions.
- **Identity:** each module has a stable ID: `base/<m>`, `core/<m>` or `<name>/<m>`, where `<name>` is the Domain or Extension. Other modules and stored data refer to it by that ID.
- **Contract:** each module's README is its contract. Don't copy it anywhere else; point to it.

> [!NOTE]
> The one-facade rule is in force, but several older crates still export extra `pub mod`s. They are narrowed as they are touched.

## How modules plug in

**Modules register themselves.** The host keeps no list of modules, routes, menus or migrations. *built*

When the program starts, every module hands in a description of itself and the things it adds to the system. Boot checks the whole set before anything runs:

| Piece | Where | What it does |
| --- | --- | --- |
| `Module` descriptor | `apps/base/module_registry` | The module's ID, layer, dependencies, migrations and service factories, submitted with `inventory` |
| `Contribution` | same | Typed, validated entries for a consumer: routes, embeds, downloads, seeds, menu, settings, vocabulary, grid tables, ports, agent operations |
| Boot validation | `Services::install` | Rejects missing, duplicate, upward and cyclic dependencies, duplicate migrations, unknown consumers and wrong payload types |
| Discovery script | `scripts/discover-modules.py` | Generates the four composition manifests before Cargo resolves. `--check` runs in CI |
| Mount check | `bilibi.container.toml` | A mounted Domain or Extension declares `id` and `layer`. `bilibi-web` refuses a descriptor that differs from the mounted crates |

Two more things keep this safe:

- A service factory can resolve only its own module and the modules it declared as direct dependencies.
- Every contribution carries its module, file and line, so an error points straight at its owner.

See also: `apps/base/module_registry/README.md`.

## Which module may use which?

Here are the dependency rules in one table:

| Edge | Rule |
| --- | --- |
| Base to Core | never |
| Domain to Base or Core | allowed, declared |
| Domain to Domain | never. Cross-domain flows use Base events |
| Domain to Extension | never |
| Within one Domain | minimal, declared, acyclic, and only where Bilimbi has the same edge |
| Optional call into an unmounted module | `base/ports`, only where Bilimbi has the same seam |
| Extension | adapts through public contracts. It must not make a module fail when absent |

You might be wondering: if Finance may never depend on Supply Chain, how does Finance learn that goods arrived? Bilibi gives two ways for one Domain or Extension to use another without depending on it:

- **Events, for "tell me later".** Supply Chain records "goods were received". Finance reacts after Supply Chain commits, in its own transaction, and keeps its own copy. *built*: `apps/base/events/README.md`.
- **Ports, for "answer me now".** Finance asks a named question, such as "what is on this order line?", inside its own transaction. If Supply Chain isn't mounted, Finance gets `Unavailable` and carries on. *built*: `apps/base/ports/README.md`. Boot refuses a port named outside its module.

## Domain and Extension repositories

Each Domain and each Extension lives in its own repository, with one or more modules inside.

- A Domain is its own repository, `BelimbingApp/bilibi-dom-<id>`, cloned to `apps/domains/<name>`. Extensions use `bilibi-ext-<id>` under `apps/extensions`.
- It is a nested checkout: not a submodule and not a symlink.
- The repository root has `bilibi.container.toml` and one folder per module. Each module is a crate named `bilibi-<name>-<module>`.
- A company build pins its composition with `scripts/discover-modules.py --pin` and rebuilds it with `--restore`.
- Domain CI is `docs/domain-ci/ci.yml` and `scripts/domain-ci.sh`.

> [!TIP]
> Domain code uses general terms (material, resource, operation, role). Industry names, such as "tape" or "roll", are configuration data, never code.

## Installed modules

*built.* Each has a README.

| Layer | Modules |
| --- | --- |
| Base | `agent_api`, `artifacts`, `audit`, `audit_views`, `authz`, `authz_agent`, `dashboard`, `database`, `datetime`, `design_library`, `events`, `grid`, `integration`, `locale`, `mail`, `media`, `menu`, `module_registry`, `notifications`, `party_ids`, `perf`, `ports`, `principal_directory`, `queue`, `schedule`, `security`, `session`, `settings`, `settings_editor`, `software`, `step_up`, `system`, `tenancy`, `tiling`, `ui`, `workflow` |
| Core | `address`, `affiliation`, `agent_access`, `agent_plugins`, `company`, `geonames`, `party`, `person`, `user`, `user_administration` |

- `base/security` is a compatibility facade over Session, Tenancy and Authz.
- Not ported: a plugin system, a Laravel compatibility layer, adoption of a Belimbing database.

## The web host

**The host is generic; modules plug into it.** It knows how to serve pages, not what any page is about.

- **Stack:** Axum on Tokio, Askama templates and a native WebSocket adapter. There is no single-page-app framework and no client-side model.
- **Pages are contributions.** A module registers a `Route` that names a `PageService`. The host checks access, then the owner renders. *built*
- **Access kinds:** Public, SignedIn, Capability, Operator, OperatorCapability.
- **Downloads** are `DOWNLOADS` contributions: GET only, never public.
- **Live pages:** the server renders HTML and the browser patches it by element id. Every intent is queued and acknowledged by request ID. Contract: `docs/live-pages.md`.
- **Agent edge:** `/api/v1`, `/mcp` and device-code OAuth. Both resolve to the `base/agent_api` registry.

See also: `apps/web/README.md`.

## How data is kept

These are the rules every module follows for its data:

- **Fresh schema.** Bilibi creates its own PostgreSQL 18 schema. It does not adopt a Belimbing database.
- **Migrations** are embedded SQL in the owning module, with the ID `<layer>-<module>-NNN-<slug>`. They run only on an explicit command, never by themselves at start-up.
- **Tenant first.** Every statement binds its data and carries its tenant predicate.
- **Soft delete** is an explicit query policy, decided by each owner.
- **UTC storage.** Times are stored in UTC; `base/datetime` owns how they are shown: timezone, clock mode and periods.
- **No unbounded reads.** Page through the rows, or ask PostgreSQL for the total.
- **Shared services:** private files go through `base/media`, jobs through `base/queue`, and outbound HTTP through `base/integration`.
- **Settings over constants.** A value an operator may change is a setting with an editor. Secrets are encrypted and masked by default. An explicitly authorized, password-confirmed and audited reveal can show one (`apps/base/settings_editor/src/reveal.rs`).

## Tenants, companies and who you are

A **tenant** is the outer wall: one customer's data, kept apart from every other. A **company** is a room inside it: one organisation within that customer.

- Exactly one tenant is the platform operator, and that is recorded in a table. IDs carry no meaning, and `grant_all` is not proof of being the operator.
- **`Scope` is the sealed proof of one unit of work:** tenant, actor and optional impersonator. It has no public constructor and no `Deserialize`, so it can't be faked.
- Modules take a `Scope`, never a raw tenant or actor ID.
- **The party model:** an account links to a Person, and through an affiliation to a Company. Every session check re-proves those links through their owners.
- Archiving a company freezes its accounts for good. There is no restore.

## How a write is saved and audited

*built.* **Every business write takes the same path:**

1. A module opens a transaction on `MutationBoundary`.
2. It writes, and returns each affected record as a `Change`.
3. Audit captures the change inside the same PostgreSQL transaction.
4. After commit, registered callbacks run (notifications, caches).

Notice step 3: the audit row and the business row are saved together, or not at all.

> [!WARNING]
> A refused audit row fails the business commit. There is no best-effort mode.

- Evidence tables (`base_audit_mutations`, `base_audit_actions`) are append-only. PostgreSQL refuses UPDATE, DELETE and TRUNCATE on them.
- Secrets are redacted by field name. Audit rows outlive actors and records, so they have no foreign keys.
- Impersonation records both the actor and the operator.

See also: `apps/base/database/README.md` and `apps/base/audit/README.md`.

## Security model

| Concern | Rule |
| --- | --- |
| Authentication | Server-held random sessions, Argon2id passwords, 24 h absolute expiry, idle expiry from settings |
| Session cookie | `HttpOnly`, `SameSite=Lax`, `Secure` on HTTPS |
| CSRF | Non-GET requests need the configured Origin. An authenticated scope also needs `X-CSRF-Token`. The token rotates on impersonation |
| Authorization | Declared capability vocabulary. Unknown key denies. Direct deny beats every grant. Platform capabilities need operator proof |
| Decisions | Each capability check writes a tenant-scoped decision-log row |
| Field masking | Operator-set restrictions mask fields before rendering, in audit values and JSON projections |
| Re-check | Access is read afresh. The owner re-authorizes inside its transaction |
| Headers | Strict CSP: same-origin scripts and styles only, no inline script |
| Files | Private disks, content-type checks, signed streams, `nosniff` |
| Agents | Machine access goes through `agent_api` with the same `Scope` rules. System scopes cannot enter |

### Known gaps

*planned.* These are not covered yet:

- **Sign-in throttling:** sign-in has no attempt throttle yet.
- **Database encryption:** PostgreSQL connections use `NoTls`.
- **Broadcasts:** they are process-local, so they don't reach other server processes.

## How we check quality

**Bilibi has no unit tests.** The gates are the compiler, Clippy, module discovery and running the app.

The commands are in `AGENTS.md`. Evidence from the running app goes in `docs/runtime-parity.md`.

## Where to go next

- **The UI rules:** [`DESIGN.md`](../../DESIGN.md).
- **Why things are the way they are:** the decision records in [`decisions/`](decisions/README.md).
- **Linking Domains and Extensions:** `apps/base/events/README.md` and `apps/base/ports/README.md`.
- **Writing a module:** `apps/base/module_registry/README.md`, then any module README as an example.
- **Running it on a server:** [`server-install.md`](../server-install.md), the instance layout, services and release switch.
