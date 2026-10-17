# Architecture

**Document Type:** Architecture overview
**Status:** Active
**Scope:** Layers, modules, composition, data, audit and security of Bilibi. The UI rules are in [`DESIGN.md`](../../DESIGN.md).
**Last Updated:** 2026-10-17

Bilibi is the Rust port of Bilimbi. Belimbing (Laravel) gives the business meaning. Bilimbi (Phoenix) gives the behaviour, the DOM and the module topology. Agents also read [`AGENTS.md`](../../AGENTS.md).

- **Status marks:** *built* means the code exists and was checked. *planned* means it is not built yet.
- **Authority:** where this file and the code differ, the code wins. Fix this file.
- **Decisions:** each architectural decision is a record in [`decisions/`](decisions/README.md). Start with [0001 Agent-first operations](decisions/0001-agent-first-operations.md).


## Layers

Four roots in one Cargo workspace, plus the web host. Dependencies point downward only.

| Layer | Path | Role | Mounted from |
| --- | --- | --- | --- |
| Base | `apps/base/<module>` | Infrastructure and cross-cutting mechanisms | this repository |
| Core | `apps/core/<module>` | The required enterprise domain | this repository |
| Domains | `apps/domains/<container>/<module>` | Optional business capabilities | one repository per container |
| Extensions | `apps/extensions/<container>/<module>` | Operator-chosen adaptations | one repository per container |
| Web | `apps/web` | Executable host: routes, sockets, seed command | this repository |

- Order is fixed: Base, Core, Domains, Extensions.
- Base never depends on Core.
- Domains and Extensions are empty in this repository. *built*: mounting, pinning and CI (`apps/domains/AGENTS.md`).
- Base and Core own no business code in their composition crates. `apps/base`, `apps/core`, `apps/domains` and `apps/extensions` are generated linkage only.

## Deep modules

A module is one Cargo crate with one facade. Callers use the facade and never see tables, queries or files.

- **Facade:** the crate root exports one service and its public types. Everything else is private.
  - *Status:* the rule is in force. Several older crates still export extra `pub mod`s. They are narrowed as they are touched.
- **Ownership:** a module owns its migrations, SQL, templates, routes, README and registered contributions.
- **Identity:** stable IDs `base/<m>`, `core/<m>`, `<container>/<m>`. Other modules and stored data name them.
- **Contract:** each module README is the contract. Do not duplicate it elsewhere.

## Registry and discovery

Modules register themselves. The host keeps no module, route, menu or migration list. *built*

| Piece | Where | What it does |
| --- | --- | --- |
| `Module` descriptor | `apps/base/module_registry` | ID, layer, dependencies, migrations, service factories, submitted with `inventory` |
| `Contribution` | same | Typed, validated entries for a consumer: routes, embeds, downloads, seeds, menu, settings, vocabulary, grid tables, ports, agent operations |
| Boot validation | `Services::install` | Rejects missing, duplicate, upward and cyclic dependencies, duplicate migrations, unknown consumers and wrong payload types |
| Discovery script | `scripts/discover-modules.py` | Generates the four composition manifests before Cargo resolves. `--check` runs in CI |
| Container check | `bilibi.container.toml` | A mounted container declares `id` and `layer`. `bilibi-web` refuses a descriptor that differs from the mounted crates |

- A service factory resolves only its own module and its declared direct dependencies.
- Contributions carry module, file and line provenance, so errors point at the owner.
- Details: `apps/base/module_registry/README.md`.

## Dependency rules

| Edge | Rule |
| --- | --- |
| Base to Core | never |
| Domain to Base or Core | allowed, declared |
| Domain to Domain | never. Cross-domain flows use Base domain events |
| Domain to Extension | never |
| Within one Domain | minimal, declared, acyclic, and only where Bilimbi has the same edge |
| Optional call into an unmounted module | `base/ports`, only where Bilimbi has the same seam |
| Extension | adapts through public contracts. It must not make a module fail when absent |

- *planned:* Base domain events at `apps/base/events`, in progress in a parallel change. Design and contract live there once it lands. Until then, no cross-domain mechanism exists.
- *built:* `base/ports` (`apps/base/ports/README.md`) lets a consumer call a provider by ID with no Cargo edge. Boot refuses a port named outside its module.

## Containers

- A Domain is its own repository, `BelimbingApp/bilibi-dom-<id>`, cloned to `apps/domains/<container>`. Extensions use `bilibi-ext-<id>` under `apps/extensions`.
- It is a nested checkout. It is not a submodule and not a symlink.
- The container root has `bilibi.container.toml` and one folder per module. Each module is a crate named `bilibi-<container>-<module>`.
- A company build pins its composition with `scripts/discover-modules.py --pin` and rebuilds it with `--restore`.
- Domain CI is `docs/domain-ci/ci.yml` and `scripts/domain-ci.sh`.
- Domain code uses meta-terms (material, resource, operation, role). Industry names are configuration data.

## Installed modules

*built.* Each has a README.

| Layer | Modules |
| --- | --- |
| Base | `agent_api`, `artifacts`, `audit`, `audit_views`, `authz`, `dashboard`, `database`, `datetime`, `design_library`, `grid`, `integration`, `locale`, `mail`, `media`, `menu`, `module_registry`, `notifications`, `party_ids`, `perf`, `ports`, `principal_directory`, `queue`, `schedule`, `security`, `session`, `settings`, `settings_editor`, `system`, `tenancy`, `tiling`, `ui`, `workflow` |
| Core | `address`, `affiliation`, `agent_access`, `company`, `geonames`, `party`, `person`, `user`, `user_administration` |

- `base/security` is a compatibility facade over Session, Tenancy and Authz.
- Not ported: a plugin system, a Laravel compatibility layer, adoption of a Belimbing database.

## Web host

The host is generic. Modules plug into it.

- Stack: Axum on Tokio, Askama templates, a native WebSocket adapter. No SPA framework and no client model.
- Pages are contributions. A module registers a `Route` that names a `PageService`. The host checks access, then the owner renders. *built*
- Access kinds: Public, SignedIn, Capability, Operator, OperatorCapability.
- Downloads are `DOWNLOADS` contributions, GET only, never public.
- Live pages: the server renders HTML, the browser patches it by element id, and every intent is queued and acknowledged by request ID. Contract: `docs/live-pages.md`.
- Agent edge: `/api/v1`, `/mcp`, device-code OAuth. Both resolve to the `base/agent_api` registry.
- Details: `apps/web/README.md`.

## Data principles

- **Fresh schema.** Bilibi creates its own PostgreSQL 18 schema. It does not adopt a Belimbing database.
- **Migrations** are embedded SQL in the owning module, ID `<layer>-<module>-NNN-<slug>`. They run only on an explicit command.
- **Tenant first.** Every statement binds its data and carries its tenant predicate.
- **Soft delete** is an explicit query policy per owner.
- **UTC storage.** `base/datetime` owns display policy: timezone, clock mode, periods.
- **No unbounded reads.** Page, or ask PostgreSQL for the aggregate.
- **Private files** go through `base/media`. Jobs go through `base/queue`. Outbound HTTP goes through `base/integration`.
- **Settings over constants.** Operator-settable values are settings with an editor. Secrets are encrypted and masked by default. An explicitly authorized, password-confirmed and audited reveal can show one (`apps/base/settings_editor/src/reveal.rs`).

## Tenancy and identity

- **Tenant** is the outer isolation boundary. **Company** is the inner organisational boundary.
- Exactly one tenant is the platform operator, recorded in a table. IDs have no meaning, and `grant_all` is not operator proof.
- `Scope` is the sealed proof of one unit of work: tenant, actor, optional impersonator. It has no public constructor and no `Deserialize`.
- Modules take a `Scope`, never a raw tenant or actor ID.
- The party model: an account links to a Person, and through an affiliation to a Company. Every session check re-proves those links through their owners.
- Archiving a company freezes its accounts for good. There is no restore.

## Writes and audit

*built.* One path for every business write.

1. A module opens a transaction on `MutationBoundary`.
2. It writes and returns each affected record as a `Change`.
3. Audit captures the change inside the same PostgreSQL transaction.
4. After commit, registered callbacks run (notifications, caches).

- A refused audit row fails the business commit. There is no best-effort mode.
- Evidence tables (`base_audit_mutations`, `base_audit_actions`) are append-only. PostgreSQL refuses UPDATE, DELETE and TRUNCATE.
- Secrets are redacted by field name. Rows outlive actors and records, with no foreign keys.
- Impersonation records both the actor and the operator.
- Contracts: `apps/base/database/README.md`, `apps/base/audit/README.md`.

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
| Known gaps | Sign-in has no attempt throttle yet. PostgreSQL connections use `NoTls`. Broadcasts are process-local. *planned* |

## Quality gates

- No unit tests. The gates are the compiler, Clippy, discovery and running the app.
- Commands are in `AGENTS.md`. Running-app evidence: `docs/runtime-parity.md`.
