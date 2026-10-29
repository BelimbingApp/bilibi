# ADR 03: Application workspaces: two front doors

**Document Type:** Architecture Decision Record
**Status:** Accepted; amended on 2026-10-22 from one front door to two. Implemented across the Platform and the app maker.
**Agents:** codex/gpt-6 (author); claude/claude-opus-5-5 (2026-10-22 amendment)
**Scope:** Trying Bilibi, creating an application repository, owning source changes and recording a reproducible composition
**Last Updated:** 2026-10-22

> The `Agents` value is self-reporting. Only known parts are included.
>
> Source: [issue 106, amended on 2026-10-21](https://github.com/BelimbingApp/bilibi/issues/106), including its Oracle and Fable reviews. The owner's decision of 2026-10-22 replaced the single front door with two (see **Amendment of 2026-10-22**). This workspace decision has no Bilimbi counterpart. It extends [ADR 02](./02-build-in-development-keep-everything-in-step.md) without changing server delivery.

## Context

**People come to Bilibi for one of two reasons, and each needs a different start.** Most want a product of their own: Bilibi at a tested version, the Domains they choose, and releases for their servers. A few want to change Bilibi itself, or build a Domain or Extension for everyone. A lasting application still needs its own recipe and dependency lock, separate from component source repositories.

The first version of this record sent everyone to `bilibi` and created the application later with `./bilibi new`. That kept one place to begin, but made a product builder start on the Platform's moving `main` and then move to a second repository.

## Decision

### Two front doors

**Product builders start with the app maker; Platform and component contributors start in `bilibi`.** Both start the same way: clone, then `./bilibi start`, which installs anything missing, runs the app and opens the sign-in page in the browser.

| You want to… | Start in | What you get |
| --- | --- | --- |
| Build your own product on Bilibi | `BelimbingApp/bilibi-app-maker` | An application: the Platform only (Base and Core) at a tested commit, ready to add Domains and Extensions, release and install |
| Change the Platform, or build a Domain or Extension for everyone | `BelimbingApp/bilibi` | The Platform's latest source, to change and contribute as pull requests |

**The app maker's recipe is the Platform only.** Domains and Extensions are the product builder's choice, added with `./bilibi add` or the command the Software page shows. The maker pins a tested Platform commit; moving that pin forward is the maker maintainers' job.

### Starting with the app maker

1. Clone `https://github.com/BelimbingApp/bilibi-app-maker`, enter the clone and run `./bilibi start`. No GitHub account is needed to try it.
2. Add Domains and Extensions with `./bilibi add <name>`, then `./bilibi start` again.
3. To keep it, make it yours: rename the maker remote to `upstream`, create your own repository, add it as `origin` and push. The clone is already the application; no `new` step is needed. Keeping the maker's history lets later maker updates merge.
4. Check and build with `./bilibi check` and `./bilibi build`, then publish with `./bilibi release`.

**Clone the app maker.** A copy made without its history, such as through GitHub's "Use this template" button or a downloaded archive, can no longer merge maker updates. The maker repository is therefore not marked as a GitHub template.

Contributions follow the remotes: application changes go to the application's `origin`. Platform, Domain and Extension changes are pull requests to their own repositories, from the checkouts under `sources/`, whose `origin` stays the URL in `setup.toml`.

### Starting from the Platform

**Clone once to try; create an application repository when you want to keep your own composition.** Steps 1–3 start in a Platform clone; step 4 uses the resulting application repository:

1. Clone `https://github.com/BelimbingApp/bilibi`, enter the clone and run `./bilibi start`. No GitHub account or template step is needed.
2. To try an official Domain, run `./bilibi add <domain>` in that clone, then `./bilibi start` again. Extensions use the same `add` command and their existing mount location.
3. To keep the trial's source composition, run `./bilibi new <name>` from the Platform root. It creates a separate sibling application repository by default and leaves the trial untouched. The application keeps the maker's history, with the maker as its `upstream` remote.
4. In the application root, use the same `./bilibi` entry point. Commit the application recipe and composition there. Create a remote and push when ready; remote creation is not part of trying Bilibi.

`new` records the current Platform, Domains **and Extensions**. Each exact commit must be fetchable from its declared source. Prove reconstruction independently of the trial's Git objects; object reuse is only an optimization.

**Refuse uncommitted source edits, unpublished commits and an occupied destination.** Recognised generated composition changes are allowed. Give the exact commit or push action needed; never omit edits or substitute revisions. This captures sources only: it does not migrate the trial's database, media, configuration or keys.

Direct Domain and Extension clones remain supported for focused contributions. There are no adopter, developer or owner modes; repository permissions govern publishing.

### Command roots

**Run commands from the Platform clone root or the outer application root, as shown below.** A component repository remains the place to commit that component's changes.

| Command | Platform root | Application root |
| --- | --- | --- |
| `./bilibi start` | Check prerequisites, run missing setup, start local sources with the development seed, and open the sign-in page in the browser when one is available (`BILIBI_OPEN_BROWSER=0` turns it off) | Same, using editable sources |
| `./bilibi add <component>` | Verify the commit safeguard, then mount an official Domain or Extension | Same, inside `sources/platform/` |
| `./bilibi new <name>` | Create an application from the trial under the refusal rules above | Not applicable; the application already exists |
| `./bilibi setup` | Check prerequisites and install repository safeguards | Also obtain missing sources; preserve existing work |
| `./bilibi status` | Show component paths, branches and changes; say no saved application recipe exists and point to `new` | Also show differences from saved versions |
| `./bilibi update` | Fast-forward safe Platform and official component branches, move clean official mounts detached at a release to a newer recommended one (never a downgrade), leave unsafe and other mounts alone, and regenerate composition | Pull the application and restore sources to saved commits; never advance the saved record |
| `./bilibi help` | Say which kind of repository this is, then list the commands grouped by when to use them, in plain words; no command and `-h` also work | Same |
| `./bilibi check` | Refuse and point to `new` | Ask about new commits, save accepted versions and run strict checks |
| `./bilibi build` | Refuse and point to `new` | Ask about new commits, save accepted versions and build the verified composition |
| `./bilibi release` | Refuse and point to `new` | Commit the saved record and start the release workflow |

Local startup permits branches and uncommitted edits. It must report differences from the saved record without rewriting it or claiming stale versions identify the running code. Checks and builds ask about new commits; accepting saves the record, while declining uses saved versions. Uncommitted source changes stop the command. Strict checks and release builds require clean source inputs, except recognised generated composition files, and must retain locked dependency resolution and remote-fetch validation. Release commits the recipe, manifest and lock together; GitHub builds from that record.

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

- Install the direct-Platform pre-commit safeguard with the first `start`/`add` implementation. `add` must verify it even before the first `start`. Reuse `scripts/discover-modules.py --check --staged` (staged files only, so a commit that removes a mounted composition at HEAD is allowed); CI runs the full `--check`, which also judges HEAD; CI remains authoritative because hooks can be bypassed.
- Install safeguards through direct Platform setup, application setup and the agent setup path. Keep installation repository-local and repeatable. Preserve existing hooks and `core.hooksPath`; report unsafe integration with a documented resolution. Never use global Git changes or index flags to hide generated differences.
- Validate the application's recipe and saved composition in its own repository. Leave Domain and Extension commits under their own checks. Refusals identify the repository and files needing attention.
- Setup and restore must never silently reset, clean, switch or remove an existing checkout. Preserve branches, staged files, untracked work and dirty sources; report mismatches with a concrete recovery action.
- First-run startup must activate newly installed tools in the same shell and verify they work. Document supported apt-based Ubuntu/Debian and WSL2 prerequisites, Python limitations, network and installation privileges. Keep the HTTP fallback when local HTTPS trust needs manual setup.
- Inventory runtime data, media, keys, configuration, ports, origin and active processes before migration. Validate a parallel workspace with a disposable database and separate data paths. Preserve the original checkout and runtime data for rollback; stop only migration-owned processes by PID. `.local/` reserves workspace state, not a decision to relocate runtime storage. Never put secrets in recipes, commits or release artifacts.

## Amendment of 2026-10-22

**Two front doors replace one.** The owner decided that a product builder starts with the app maker, which ships the Platform only, and that `./bilibi start` opens the browser at the sign-in page. Starting in `bilibi` and using `new` stays fully supported for contributors and for anyone who began there.

What changed with it:

- The template repository was renamed, first `bilibi-app-template` and then `bilibi-app-maker`: the front door, which will grow guided commands for making an application. It is no longer marked as a GitHub template, so no button creates unlinked copies.
- Its recipe dropped the sample Finance Domain and pins the Platform alone.
- Both `./bilibi help` texts say which kind of repository they are in and group commands by when to use them.
- `new` keeps the maker's history as `upstream` (BelimbingApp/bilibi#152), so applications from either door take maker updates with `git merge upstream/main`.

## Consequences

**Trying stays simple, while application releases remain independently reconstructible.** Servers still consume finished, verified artifacts under ADR 02; promote the same artifact to Staging and Production.

- Implement the Platform front door and direct-Platform safeguard first. Then separate editable sources from outputs, add the application wrapper and `new`, install application safeguards, and migrate documentation and existing workspaces together.
- Before making this the default, prove fresh-shell startup, `add` before `start`, an actual rejected mounted-composition commit, independent `new` reconstruction and every refusal rule in disposable workspaces. Repeated setup must preserve local work and custom hooks. Follow the issue's full acceptance list.
- The issue retains implementation decisions about hook integration, runtime paths, application tooling updates and deliberate adoption of existing checkouts. Its review comments also identify strict-build isolation and compiled development identity risks. Those are implementation work, not safeguards already provided by this record.

## Current implementation

**The common entry point and application workspace are implemented.** Platform supplies `start`, `setup`, `add`, `status`, `update`, `help` and `new`; the [app maker](https://github.com/BelimbingApp/bilibi-app-maker) is the product builder's front door and supplies the application wrapper, independent editable sources, saved recipes, commit safeguards and strict release commands. `new` pins the maker revision, independently fetches every selected source and validates the trial's resolved lock before saving it. `check`, `build` and `release` in Platform explain how to create an application.

[Application workspaces](../../application-workspaces.md) documents Git ownership, deliberate tooling updates, parallel migration, manual adoption of ordinary clones and rollback. Setup never adopts or relocates an existing checkout automatically. Runtime data retains its existing paths; output conflicts require a deliberate preserved move. Server delivery remains under ADR 02.
