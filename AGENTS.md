# Bilibi agent guidelines

Read this file and `DESIGN.md` (UI design) before changing code, templates, migrations, assets or docs. Architecture is in `docs/architecture/README.md`. Decisions that shape it are records in `docs/architecture/decisions/`; read the index before changing a convention they cover.

- **Bilimbi** (<https://github.com/BelimbingApp/bilimbi>) is the reference for behaviour, DOM and module topology. Say in a comment which Bilimbi module a file mirrors.
- **Belimbing** (<https://github.com/BelimbingApp/belimbing>) is the reference for business meaning, not for Laravel details.
- Bilibi creates a fresh schema. It does not adopt a Belimbing database.

Folder guides hold the mistakes that recur in that folder. Open the one you are about to work in:

- `apps/AGENTS.md`: authorization, actor, clocks and routes in any module
- `apps/base/ui/AGENTS.md`: shared components, forms, lists, shell, live pages
- `apps/base/media/AGENTS.md`: uploads, private files, attachments
- `apps/base/dashboard/AGENTS.md`: dashboard panels
- `apps/domains/AGENTS.md`, `apps/extensions/AGENTS.md`: mounting Domains and Extensions

Authoritative contracts (point to them, do not copy them): `apps/base/module_registry/README.md` (descriptors, contributions, boot validation), `apps/web/README.md` (routes, downloads, access, agent edge), `apps/base/database/README.md` and `apps/base/audit/README.md` (writes and audit), `docs/live-pages.md` and `docs/inline-editing.md`, and the README of the module you edit.

## Principles

- **Production grade, no demo mode.** No demo seeds, fake data paths or feature flags for hypotheticals. Examples and walkthroughs install `fixtures/walkthrough` into a disposable database.
- **Deep modules.** One small facade per module; the crate root exports the facade, its service ID and its public types, everything else is private (`mod`, not `pub mod`). Do not widen a surface to reach a helper. Narrow existing extra `pub mod`s when you touch them.
- **Configurable.** A value an operator could reasonably set is a setting with an operator UI (`base/settings`, `base/settings_editor`), not a constant or an environment-only value. Secrets are encrypted, never shown back, never logged.

## Modules and dependencies

- **Register, do not list.** A module submits one `bilibi_module_registry::Module` descriptor and `Contribution::new` entries (routes, menu, settings, capabilities, agent operations, ports, …) with `inventory`. The host keeps no module, route or migration list.
- Keep `Cargo.toml` edges and descriptor `dependencies` equal. Declare a dependency as `x.workspace = true` after adding it to `[workspace.dependencies]`. Never turn on a shared crate's features from one module: Cargo unifies them across the workspace.
- Run `python3 scripts/discover-modules.py` after adding, removing or moving a module, Domain or Extension. Never edit its generated files, and never commit a mounted composition or its `Cargo.lock` entries to the Platform.
- **No dependencies between Domains.** Cross-domain flows use Base events (`apps/base/events/README.md`). Within one Domain, edges are minimal, declared, acyclic, and only where Bilimbi has the same edge. Optional calls into an unmounted module go through `base/ports`.
- Domain and Extension code uses meta-terms (material, resource, operation, role). Industry and product names are configuration data, never identifiers, including agent operation keys.
- A Platform change that breaks an official Domain or Extension (listed in `catalogue.json`) includes a companion PR in that repository, linked from the Platform PR; ones outside the catalogue are not changed from a Platform task. Keep companions green and merge together; see `docs/versions.md#platform-changes-and-companion-prs` for CI branch selection.

## Data, tenancy, audit, security

- **Tenant scope.** Tenant-owned reads and writes take a sealed `bilibi_tenancy::Scope`, never a raw tenant, actor or party ID. Derive tenant and actor from the session. Every SQL statement binds its data and carries its tenant predicate.
- **Writes go through `MutationBoundary`**, returning every affected record as a `Change`. Never insert audit rows, open an inner transaction or publish notifications by hand.
- **Authorization.** Capabilities are declared in the module's vocabulary contribution; an unknown key fails closed; direct denies win. The owner rechecks inside its mutation transaction.
- **Migrations** are embedded in the owning module, ID `<layer>-<module>-NNN-<slug>` (mounted: `<name>-<module>-NNN-<slug>`, where `<name>` is the Domain or Extension). Never edit an applied migration. Nothing migrates or seeds on boot.
- **Use the platform, do not hand-roll:** files `base/media`, documents `base/artifacts`, jobs `base/queue`, mail `base/mail`, outbound HTTP `base/integration`, time `bilibi_datetime` (store UTC).
- **Agents.** Expose an operation with an `agent_api` contribution that calls your facade with the scope it receives. No endpoint per module.
- Askama escapes by default: keep `|safe` inside the shared macros, never `Html::raw` on request data. Never build identifiers or SQL fragments from request input.
- Never commit or log secrets, session IDs or tokens, including in fixtures and evidence.

## UI

`DESIGN.md` is the design source; `apps/base/ui/AGENTS.md` holds the recurring mistakes.

- Use the shared Askama macros and typed models of `base/ui` for every page element. No hand-written markup or copied classes.
- Every list uses `FlexTable` on `ListState`, never the plain `Table`.
- Records read first and edit in place; no separate Edit page. Use `ConfirmDialog`, never `window.confirm`.
- A Domain or Extension owns no CSS or JavaScript. Run `./scripts/build-assets.sh --check` after changing templates, classes or assets.

## Checks

Bilibi has **no unit tests**. Do not add `#[test]`, `#[cfg(test)]` or a test crate.

```sh
python3 scripts/discover-modules.py --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

- Then run the app (`./scripts/start-app.sh`) and use the change. Prove behaviour in the running system, not by reading source. Evidence and parity checks: `docs/runtime-parity.md`.
- Stop only a server you started, by its process ID; never by name (`pkill`, `killall`, `pgrep -x`), which also kills the developer's running app.
- `APP_ORIGIN` must equal the exact address you browse, or sign-in loops.
- A change to a page, template or live behaviour is done only after you have used it in the browser at the exact `APP_ORIGIN`. Proof from rendered HTML alone is not done; if the browser cannot be used, report that you are blocked. A docs-only or non-UI change needs only the three checks above, with no app run.
- CI also runs `./scripts/build-assets.sh --check` and `cargo build --workspace --release --locked`. A pull request that cannot affect the build still runs the CSS check and passes the rest without compiling; see `docs/domain-ci/README.md`.

## Docs

- A module's contract lives in its `README.md`; cross-cutting contracts in `docs/`. Keep README and `DESIGN.md` true.
- Human-facing docs are scan-first: a one-line answer per section, short headings, bullets, tables. This file and the folder guides keep their own style.
- Markdown prose is one paragraph per line; do not hard-wrap.

## Maintaining this file

Keep only knowledge useful to almost every agent session that an agent would otherwise get wrong. Do not repeat what the codebase shows; point to the authoritative file. Prefer pruning over appending.
