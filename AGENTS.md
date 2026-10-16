# Bilibi agent guidelines

Bilibi is the Rust port of Bilimbi (Elixir/Phoenix), which is the Phoenix implementation of Belimbing (Laravel). Read this file and `DESIGN.md` before changing code, templates, migrations, assets or docs.

- **Belimbing** (<https://github.com/BelimbingApp/belimbing>) is the reference for business meaning. It is not a template for Laravel details.
- **Bilimbi** (<https://github.com/BelimbingApp/bilimbi>) is the direct parent. Match its behaviour, DOM and module topology. Say in a comment which Bilimbi module a file mirrors.
- Bilibi creates a fresh schema. It does not adopt a Belimbing database.

Folder guides hold the mistakes that recur in that folder. Open the one you are about to work in:

- `apps/AGENTS.md`: authorization, actor, clocks and routes in any module
- `apps/base/ui/AGENTS.md`: shared components, forms, lists, shell, live pages
- `apps/base/media/AGENTS.md`: uploads, private files, attachments
- `apps/base/dashboard/AGENTS.md`: dashboard panels
- `apps/domains/AGENTS.md`, `apps/extensions/AGENTS.md`: mounting containers

Authoritative contracts (point to them, do not copy them):

- `apps/base/module_registry/README.md`: descriptors, contributions, boot validation
- `apps/web/README.md`: page routes, downloads, access, agent edge
- `docs/live-pages.md`, `docs/inline-editing.md`: live page and inline edit contracts
- `apps/base/database/README.md`, `apps/base/audit/README.md`: write and audit contracts
- the README of the module you edit

## 1. Stack

`Cargo.toml` and `Cargo.lock` are authoritative. Today: Rust 1.99, Axum 0.8 on Tokio, tokio-postgres with deadpool, PostgreSQL 18, Askama 0.16, `inventory` for registration, and a pinned Tailwind build for the committed CSS.

- Declare a dependency in the crate that uses it, as `x.workspace = true`. Add it to `[workspace.dependencies]` first.
- Check the standard library and the existing dependencies before adding a crate. Give the reason in the PR.
- Do not turn on features of a shared crate from one module. Cargo unifies features across the workspace.
- Keep `Cargo.lock` committed. CI builds with `--locked`.

## 2. Principles

- **Production grade, no demo mode.** No demo seeds, no fake data paths, no feature flags for hypotheticals. Examples and walkthroughs install `fixtures/walkthrough` into a disposable database.
- **Low entropy.** Fix small drift when you see it. For a larger fix, write down the plan in the PR.
- **Deep modules.** A small, stable interface over hidden complexity. See section 4.
- **Honesty.** Names, stored values, APIs, docs and UI copy tell the truth and match the code.
- **One blessed path.** Prefer it over option sprawl. Business modules stay adaptable. The shell and platform conventions do not.
- **Configurable.** A value an operator could reasonably set is a setting with an operator UI (`base/settings`, `base/settings_editor`), not a constant, a config file or an environment-only value. Secrets are encrypted, never shown back, never logged. Only bootstrap values needed before the settings store is reachable stay in the environment.

## 3. Layers and placement

Four roots, one Cargo crate per module. Order: Base, Core, Domains, Extensions. `apps/web` is the host.

- `apps/base/<module>`: infrastructure and cross-cutting mechanisms. Never depends on Core.
- `apps/core/<module>`: the required enterprise domain. Depends on Base and declared Core APIs.
- `apps/domains/<container>/<module>`: optional business Domains. Each container is its own repository, mounted as a nested checkout.
- `apps/extensions/<container>/<module>`: operator-chosen adaptations. Same mounting rules.
- `apps/web`: the executable, route and socket transport, seed command. It owns no module list.

Rules:

- Module IDs are `base/<module>`, `core/<module>` and `<container>/<module>` (snake_case, matching the mounted folders). They are stable: other modules and stored data name them.
- A module owns its migrations, SQL, templates, assets contract, README and routes. Nothing of it lives in the composition crate or the host.
- Run `python3 scripts/discover-modules.py` after adding, removing or moving a module or container. It generates the four composition manifests and `lib.rs` files. CI runs `--check`. Do not edit those generated files by hand.
- Mounting, unmounting and pinning are in `apps/domains/AGENTS.md`. Never commit a mounted composition or its `Cargo.lock` entries to the Platform.
- Domain and Extension code uses meta-terms (material, resource, operation, role). Industry, product and line names are configuration data, never identifiers. This includes agent operation keys and guides.

## 4. Deep modules and dependencies

- **One small facade per module.** The crate root exports the facade, its service ID and its public types. Everything else is private (`mod`, not `pub mod`). A caller never needs to know which table, query or file implements an operation. Do not widen a module's surface to reach a helper; move the need into the facade. Several existing crates still export extra `pub mod`s. Do not add to them, and narrow them when you touch them.
- **Register, do not list.** A module submits one `bilibi_module_registry::Module` descriptor (ID, layer, dependencies, migrations, services) and `Contribution::new` entries (routes, embeds, seeds, menu, settings, capabilities, grid tables, agent operations, ports) with `inventory`. The host keeps no module, route or migration list. Boot refuses missing, duplicate, upward or cyclic dependencies.
- **Declared dependencies only.** A service factory resolves only its own module and its declared direct dependencies. Keep the `Cargo.toml` edges and the descriptor `dependencies` equal.
- **No dependencies between Domains.** A Domain may depend on Base and Core. It never depends on another Domain, and never on an Extension. Cross-domain flows use Base domain events: `apps/base/events`, in progress in a parallel PR. Reference it by path and design. Do not invent another bus, and do not add a Cargo dependency or descriptor edge to another container.
- **Within one Domain,** dependencies between modules are minimal, declared and acyclic. Add an edge only where Bilimbi has the same edge. If you need one Bilimbi lacks, change the design, not the graph.
- An optional call into an unmounted module goes through `base/ports` (`apps/base/ports/README.md`), only where Bilimbi has the same seam.
- Base never imports Core. Core modules collaborate through public facades, contributions or ports, never another module's tables.
- A Domain must work with no Extension mounted. An Extension must not make a module fail to boot when the Extension is absent.

## 5. Data, tenancy, audit, security

- **Tenant scope.** An API that reads or writes tenant-owned data takes a sealed `bilibi_tenancy::Scope`, never a raw tenant ID, actor ID or party ID. A `Scope` has no public constructor and no `Deserialize`. Derive tenant and actor from the session, never from request parameters. Every application SQL statement binds its data and carries its tenant predicate. The exceptions are session-token bootstrap and the explicit migration and seed commands.
- **Writes go through `MutationBoundary`.** Open one transaction with `MutationBoundary`, return every affected record as a `Change` (bulk writes included), and pass the `MutationTransaction` to nested owners. Do not insert audit rows, open an inner transaction, or publish notifications by hand. Audit failure rolls the business write back. Contract: `apps/base/database/README.md`.
- **No best-effort audit.** Evidence is append-only, secrets are redacted by field name, and PostgreSQL refuses UPDATE and DELETE on it. Do not redact at the call site.
- **Authorization.** Capabilities are declared in a module's vocabulary contribution. An unknown key fails closed. Direct denies win over every grant. Platform capabilities need the persisted platform-operator proof. Route access is checked before the owner renders, and the owner rechecks inside its mutation transaction.
- **Migrations are explicit.** Embedded SQL in the owning module, ID `<layer>-<module>-NNN-<slug>` (mounted: `<container>-<module>-NNN-<slug>`). Never edit an applied migration. Nothing migrates or seeds on boot; use `bilibi-web migrate`, `seed` and `seed-production`.
- **Time.** Store UTC. Render through `bilibi_datetime` so the reader's clock and timezone apply.
- **Outbound HTTP** goes through `base/integration` so the exchange is recorded and redacted.
- **Files** go through `base/media`, generated documents through `base/artifacts`, jobs through `base/queue`, mail through `base/mail`. Do not hand-roll storage, download routes, PDF output, job loops or SMTP.
- **Agents.** Expose a module operation to agents with an `agent_api` contribution that calls your facade with the scope it receives. No endpoint per module.
- **Never** commit or log secrets, session IDs or tokens, including in fixtures and evidence. `.local/` and `.env` are ignored.

## 6. UI

`DESIGN.md` is the design source. `apps/base/ui/AGENTS.md` holds the recurring mistakes.

- Use the shared Askama macros and typed models of `base/ui` for every page, card, button, badge, form control, dialog, tab, flash and icon. No hand-written markup or copied classes.
- Every list uses `FlexTable` (`apps/base/ui/src/flex_table.rs`), never the plain `Table`. Build it on `ListState` and host its columns with `ListColumns` or `bilibi_grid::PageColumns`.
- A record's page reads first and edits in place with the inline components. No separate Edit page.
- Pages are live: server-rendered HTML patched by element id over the socket. No SPA framework, no client model, no permission logic in the browser.
- No `window.confirm`, no `data-confirm`. Use `ConfirmDialog`.
- Account preferences (theme, time mode, pins) live on the account, not in `localStorage`.
- A Domain or Extension owns no CSS or JavaScript. Run `./scripts/build-assets.sh --check` after any change to templates, classes or assets.
- Copy is plain and honest. Do not add a nav item for a page that does not exist.

## 7. Checks

Bilibi has **no unit tests**. Do not add `#[test]`, `#[cfg(test)]` or a test crate. The checks are:

```sh
python3 scripts/discover-modules.py --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

- Then **run the app** and use the change: `./scripts/start-app.sh` (`README.md` "Manual HTTP run" has the manual route). Prove behaviour in the running system, not by reading source.
- CI also runs `./scripts/build-assets.sh --check` and `cargo build --workspace --release --locked`.
- A runnable specimen is an example under `apps/web/examples/` or a screen in the Design Library. Extend it when you add a primitive.
- Running-app evidence and parity checks: `docs/runtime-parity.md`.

## 8. Rust conventions

- One concern per file; split a file before it becomes a wall.
- Return typed errors (`thiserror`) at module boundaries. Define impossible states out of existence with types: sealed proofs, validated newtypes.
- Askama escapes by default. Keep `|safe` inside the shared macros. Request data goes through `Html::text` or a template, never `Html::raw`.
- Do not build identifiers or SQL fragments from request input. Validate and bound every untrusted value.
- Do not load a growing table whole. Page it, or ask PostgreSQL for the aggregate.
- Name constraints and indexes on purpose.
- Run `cargo fmt` and clippy clean. Do not silence a lint without a comment giving the reason.

## 9. Docs

- A module's contract lives in its `README.md`. Cross-cutting contracts live in `docs/`. Do not create a second hand-kept registry of what the code already declares.
- Human-facing docs (`README.md`, `DESIGN.md`, `docs/*.md`) are scan-first: a one-line answer at the top of each section, short headings, bullets, commands in code blocks, tables for comparisons, short sentences. This file and the folder guides are for agents and keep their own style.
- When a convention matters to agents, make it a compiler or discovery check, a documented contract or a deterministic command. Do not rely on an unwritten rule.
- Keep README and `DESIGN.md` true. Mark anything not built as planned.

## 10. Version control

- Keep commits focused and descriptive. Preserve unrelated changes.
- Never commit secrets, local credentials, build output (`target/`, `.local/`) or a mounted container.
- No destructive commands (`git reset --hard`, `git checkout --`) without explicit authorization.
- Land cross-repository changes together and note the merge order.

## Maintaining this file

Keep this file for knowledge useful to almost every agent session. Do not repeat what the codebase shows; point to the authoritative file or command. Prefer rewriting or pruning over appending. Keep entries concise.
