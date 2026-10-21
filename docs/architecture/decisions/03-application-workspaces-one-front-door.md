# ADR 03: Application workspaces: one front door

**Document Type:** Architecture Decision Record
**Status:** Accepted direction; planned, not implemented. The `./bilibi` commands below do not exist yet.
**Agents:** codex/gpt-6 (author)
**Scope:** Trying Bilibi, creating an application repository, owning source changes and recording a reproducible composition
**Last Updated:** 2026-10-21

> The `Agents` value is self-reporting. Only known parts are included.
>
> Source: [issue 106, amended on 2026-10-21](https://github.com/BelimbingApp/bilibi/issues/106), including its Oracle and Fable reviews. This workspace decision has no Bilimbi counterpart. It extends [ADR 02](./02-build-in-development-keep-everything-in-step.md) without changing server delivery.

## Context

**Everyone starts in `bilibi`, with or without an AI agent.** Choosing between a Platform checkout and a setup template before trying the software adds an unnecessary decision. A lasting application still needs its own recipe and dependency lock, separate from component source repositories.

## Decision

### Trial to application

**Clone once to try; create an application repository when you want to keep your own composition.** These are the planned steps, not instructions runnable today:

1. Clone `https://github.com/BelimbingApp/bilibi`, enter the clone and run `./bilibi start`. No GitHub account or template step is needed.
2. To try an official Domain, run `./bilibi add <domain>` in that clone, then `./bilibi start` again. Extensions use the same `add` command and their existing mount location.
3. To keep the trial's source composition, run `./bilibi new <name>` from the Platform root. It creates a separate sibling application repository by default and leaves the trial untouched.
4. In the application root, use the same `./bilibi` entry point. Commit the application recipe and composition there. Create a remote and push when ready; remote creation is not part of trying Bilibi.

`new` records the current Platform, Domains **and Extensions**. Each exact commit must be fetchable from its declared source. Prove reconstruction independently of the trial's Git objects; object reuse is only an optimization.

**Refuse uncommitted source edits, unpublished commits and an occupied destination.** Recognised generated composition changes are allowed. Give the exact commit or push action needed; never omit edits or substitute revisions. This captures sources only: it does not migrate the trial's database, media, configuration or keys.

The setup template supplies content to `new`; it is no longer a user-facing starting point. Direct Platform, Domain and Extension clones remain supported for focused contributions. There are no adopter, developer or owner modes; repository permissions govern publishing.

### Command roots

**Run commands from the Platform clone root or the outer application root, as shown below.** A component repository remains the place to commit that component's changes.

| Planned command | Platform root | Application root |
| --- | --- | --- |
| `./bilibi start` | Check prerequisites, run missing setup, start local sources with the development seed | Same, using editable sources |
| `./bilibi add <component>` | Verify the commit safeguard, then mount an official Domain or Extension | Same, inside `sources/platform/` |
| `./bilibi new <name>` | Create an application from the trial under the refusal rules above | Not applicable; the application already exists |
| `./bilibi setup` | Check prerequisites and install repository safeguards | Also obtain missing sources; preserve existing work |
| `./bilibi status` | Show component paths, branches and changes; say no saved application recipe exists and point to `new` | Also show differences from saved versions |
| `./bilibi check` | Refuse and point to `new` | Run strict composition and build checks against saved versions |
| `./bilibi pin` | Refuse and point to `new` | Explicitly record selected component commits and resolved dependencies |
| `./bilibi build` | Refuse and point to `new` | Produce a release through the pinned builder and verified composition |

Local startup permits branches and uncommitted edits. It must report those differences without rewriting the recipe or claiming stale pins identify the running code. Pin after component commits; review the recipe, manifest and lock together. Strict checks and release builds require recorded commits and clean source inputs, except recognised generated composition files, and must retain locked dependency resolution and remote-fetch validation.

### Git ownership

**The application owns the recipe; each component owns its source.** Domains and Extensions remain ordinary nested Git clones, without submodules or symlinks.

| Location | Commit policy |
| --- | --- |
| Application root | Commit `setup.toml` (source URLs and exact commits), `composition/manifest.txt`, `composition/Cargo.lock`, application automation and docs |
| `sources/platform/` | Independent Platform repository; commit Platform source changes with the unmounted generated files and lock |
| `sources/platform/apps/domains/<name>/` | Independent Domain repository; commit its own changes here |
| `sources/platform/apps/extensions/<name>/` | Independent Extension repository; commit its own changes here |
| `sources/` in application Git | Ignored, valuable working copies; never treat them as disposable outputs |
| `build/`, `.local/` | Ignored outputs, caches and machine-local state; commit nothing |

**Never commit the trial's generated mounted `Cargo.lock`, `apps/domains/Cargo.toml` or `apps/domains/src/lib.rs` to Platform.** Their committed Platform forms stay unmounted. The same ownership rule covers Extension linkage. The application's saved `composition/Cargo.lock` is intentionally committed in the application repository. Today's mounting procedure remains in [Domain guidelines](../../../apps/domains/AGENTS.md) and [Extension guidelines](../../../apps/extensions/AGENTS.md).

### Safeguards

**Ship protection with mounting, and preserve work before convenience.**

- Install the direct-Platform pre-commit safeguard with the first `start`/`add` implementation. `add` must verify it even before the first `start`. Reuse `scripts/discover-modules.py --check`, including staged-file inspection; CI remains authoritative because hooks can be bypassed.
- Install safeguards through direct Platform setup, application setup and the agent setup path. Keep installation repository-local and repeatable. Preserve existing hooks and `core.hooksPath`; report unsafe integration with a documented resolution. Never use global Git changes or index flags to hide generated differences.
- Validate the application's recipe and saved composition in its own repository. Leave Domain and Extension commits under their own checks. Refusals identify the repository and files needing attention.
- Setup and restore must never silently reset, clean, switch or remove an existing checkout. Preserve branches, staged files, untracked work and dirty sources; report mismatches with a concrete recovery action.
- First-run startup must activate newly installed tools in the same shell and verify they work. Document supported apt-based Ubuntu/Debian and WSL2 prerequisites, Python limitations, network and installation privileges. Keep the HTTP fallback when local HTTPS trust needs manual setup.
- Inventory runtime data, media, keys, configuration, ports, origin and active processes before migration. Validate a parallel workspace with a disposable database and separate data paths. Preserve the original checkout and runtime data for rollback; stop only migration-owned processes by PID. `.local/` reserves workspace state, not a decision to relocate runtime storage. Never put secrets in recipes, commits or release artifacts.

## Consequences

**Trying stays simple, while application releases remain independently reconstructible.** Servers still consume finished, verified artifacts under ADR 02; promote the same artifact to Staging and Production.

- Implement the Platform front door and direct-Platform safeguard first. Then separate editable sources from outputs, add the application wrapper and `new`, install application safeguards, and migrate documentation and existing workspaces together.
- Before making this the default, prove fresh-shell startup, `add` before `start`, an actual rejected mounted-composition commit, independent `new` reconstruction and every refusal rule in disposable workspaces. Repeated setup must preserve local work and custom hooks. Follow the issue's full acceptance list.
- The issue retains implementation decisions about hook integration, runtime paths, application tooling updates and deliberate adoption of existing checkouts. Its review comments also identify strict-build isolation and compiled development identity risks. Those are implementation work, not safeguards already provided by this record.

## Current implementation

**Only this decision is recorded here.** No wrapper, hook installation, template migration or quick-start change is implemented by this ADR. Continue using the existing scripts and mounting guides until the corresponding parts of [issue 106](https://github.com/BelimbingApp/bilibi/issues/106) are delivered and verified.
