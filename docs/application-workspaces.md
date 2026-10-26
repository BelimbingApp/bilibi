# Application workspaces

An application is your own product built on Bilibi. You get one in either of two ways:

- **Start with the app maker** if you want a product: clone [`bilibi-app-maker`](https://github.com/BelimbingApp/bilibi-app-maker) and run `./bilibi start`. The clone is already your application; its README, **Start here**, shows how to make it yours.
- **Start from a Platform clone** if you began here: run `./bilibi new my-bilibi`, as below.

## Create your application from a Platform clone

From your trial's Platform root, run:

```sh
./bilibi new my-bilibi
cd ../my-bilibi
./bilibi start
```

`new` creates a sibling repository. It records the current Platform, every mounted Domain and every mounted Extension. All selected commits must be independently fetchable from their declared `origin`. Sources use credential-free HTTPS URLs; private repositories use your existing Git credential helper or `GIT_ASKPASS`.

The command copies a pinned revision of the [app maker](https://github.com/BelimbingApp/bilibi-app-maker). It reconstructs source clones without the trial's Git objects, validates the trial's resolved dependency lock against those sources, then saves the recipe, manifest and lock. It never resolves a replacement dependency selection. If the working lock needs updating after mounting, run `cargo metadata` in the trial first. Python 3.11+, Git and the trial's working Cargo toolchain are needed; creation itself does not require Docker.

**This captures sources only.** The trial's database, media, configuration and keys stay where they are. Generated composition changes may remain local to Platform; other staged, unstaged and untracked source changes must be committed in their owning repository and pushed first. A refusal prints the repository and exact commit or push step. An occupied destination is left untouched; choose another sibling name.

Review your application's saved files, then make its first commit:

```sh
git add .
git commit -m 'Create application from selected Bilibi sources'
```

The application's `main` branch starts at that maker revision, and the maker is its `upstream` remote, so later maker updates merge with your changes. Create a remote when you are ready, add it with `git remote add origin <your-repository-url>`, then run `git push -u origin main`. Trying Bilibi and creating the local application do not require a GitHub account.

## Git ownership

Each component stays an ordinary independent Git clone:

```text
my-bilibi/                           # application Git root
├── bilibi
├── setup.toml                       # source URLs and exact commits
├── composition/
│   ├── manifest.txt
│   └── Cargo.lock                   # saved application dependency lock
├── scripts/                         # application tooling
├── sources/                         # ignored, valuable working copies
│   └── platform/                    # Platform Git root
│       └── apps/
│           ├── domains/<name>/       # Domain Git root (optional)
│           └── extensions/<name>/    # Extension Git root (optional)
├── build/                           # ignored outputs and caches
└── .local/                          # ignored workspace coordination
```

| Change | Repository to commit in |
| --- | --- |
| Selected versions, saved manifest and dependency lock, application automation and docs | Application root |
| Platform source | `sources/platform/`, with unmounted generated files and Platform lock |
| Domain source | That Domain's own Git root |
| Company adaptation or Extension source | That Extension's own Git root |
| Keys, local configuration, databases, media, caches and build outputs | Never commit |

**Keep `sources/`; it contains working repositories.** Do not clean it as build output. Domains and Extensions are neither submodules nor source symlinks. The Platform safeguard still rejects mounted composition files in Platform commits. Your application's `composition/Cargo.lock` is intentionally committed in the outer repository.

## Work and release

Clone an existing application and run `./bilibi start` from its outer root. Startup obtains missing sources and preserves existing branches, staged files, untracked work and edits. The application defaults to `https://bilibi.localhost`, with its own private database. To choose another address, put `BILIBI_DEV_HOST=<name>.localhost` in `.env` in the application root; the Platform `README.md`, Local launcher, lists every line `.env` accepts. Restart the launcher after changing the address; it releases its old name when it registers the new one. If another checkout owns the chosen address, startup stops and prints the `.env` line to fix it. `./bilibi status` shows their differences from the saved recipe. Startup runs local source; it does not silently pin edits or claim a stale release identity.

After component changes are committed and pushed, run:

```sh
./bilibi pin
git diff -- setup.toml composition/
```

Review and commit those three saved files together. Domains and Extensions are optional, including a Platform-only application. Strict checks and builds require the saved commits, remotely fetchable sources and clean inputs, except recognised generated composition. The application uses the maker's pinned Docker builder for `pin`, `check` and `build`; these commands need Docker and a running daemon. Checks require a separate empty `BILIBI_CHECK_DATABASE_URL`, never your working database. See your application's README for the full command and release contract.

Publish an application release through its existing workflow. Verify the resulting artifact on Staging, then promote **that same artifact** to Production under [ADR 02](architecture/decisions/02-build-in-development-keep-everything-in-step.md) and the [server installation guide](server-install.md). A workspace migration is not a server deployment.

## Update your workspace

Run `./bilibi update`, then `./bilibi start`, from your workspace root. Use `./bilibi help` for the command list.

| Workspace | What `update` does |
| --- | --- |
| Platform clone | Fast-forward the current branch from its upstream, move clean official Domains and Extensions that sit detached at a release (as `add` leaves them) to the catalogue's recommended compatible release, then regenerate composition and `Cargo.lock` |
| Application repository | Pull the application repository and bring `sources/` to its saved commits; saved pins advance only when you run `pin` |

A published branch whose changes already reached the default branch can return there safely, including an exact squash merge. Updates stop for source edits or commits that would be lost. Recognised generated composition files and a dependency lock reproduced from the committed lock may be regenerated. Updates never reset source work, silently stash edits or force a checkout. Update never downgrades a component. A component on a branch, or one that is dirty, symlinked, unlisted or off its official origin, stays in place; the command prints its path and why, and still succeeds once the Platform is updated. A branch is never detached: update it in its own repository. If the catalogue cannot be read, component updates are skipped and composition is still regenerated.

An application created before `update` was available needs the deliberate tooling update below first. Its wrapper belongs to the application repository.

## Update application tooling

The application owns its wrapper, scripts and workflows after creation. `setup` obtains component sources; it does not overwrite application automation from a moving template branch.

An application created by `new` merges the maker's `upstream/main` on a branch and reviews it as a pull request; the maker's README, **Take maker updates**, gives the commands. Merge with a merge commit, never a squash, so the link to the template survives.

An application created before `new` kept that history has no common history with the maker. Compare its automation against a chosen commit of the maker in a separate clone. Transfer the relevant wrapper, script, builder and workflow changes deliberately, preserving application customizations. Review the full diff, run the application's checks and packaging verification, then commit the tooling update in the application repository. After that, link the histories once, as the same README section describes, so later updates merge. Keep recipe changes separate and explicit; never replace `setup.toml` or `composition/` with the template's example selection.

## Inventory an existing checkout

**Create and validate a parallel workspace before changing the original.** This also applies to a Platform checkout with Finance mounted and to old applications with editable sources under `build/platform/`.

Record paths and names in private migration notes; do not print secret values:

- Resolve the application, Platform, Domain and Extension Git roots. Record branch names, commit IDs, status including staged and untracked files, and remote host/repository names. Inspect remote URLs privately if they may contain embedded credentials.
- Locate local configuration, environment files and credential helpers. Record key file paths, permissions and backup locations; never copy key values into notes, recipes or Git.
- Inventory the PostgreSQL connection and version, database/data-directory ownership, media disk roots from settings, mail output, and other persistent storage. Record references and paths without connection passwords.
- Record the editor, shell, launcher and local service paths, exact browser origin, ports and active process IDs. Read each process's ownership before planning a switch; avoid command-line dumps that may contain secrets.
- Make verified backups of runtime data and keys according to your existing procedure. Preserve staged/unstaged patches, ignored work and nested repository histories in protected backups if you will later adopt those checkouts.

Commit and publish source edits deliberately in each owning repository, or keep them in the original while validating a parallel saved revision. `new` refuses dirty or unpublished sources; it never stashes or discards them. The generated Finance composition remains a local Platform difference, not a valid Platform commit.

## Validate the parallel workspace

From a clean, published trial, run `./bilibi new <unused-sibling-name>`. For an old application, first update the application tooling deliberately as above, keeping the old checkout available; use the new `sources/platform/` location for new independent clones. Preserve its saved recipe and composition, then run the updated `./bilibi setup`. Do not rename or delete `build/` wholesale: it may contain valuable editable repositories in the old layout.

Use an independently created empty PostgreSQL 18 database and a separate unused development port. Supply its `DATABASE_URL` privately in the validation shell, set `BILIBI_DEV_PORT`, then run `./bilibi start` from the new application root. Do not reuse the old database or copy its settings into this validation database. Newly seeded media paths, key files, mail output and workspace state must resolve inside the parallel workspace or another dedicated disposable location. Verify any environment overrides before startup.

Open the launcher's printed address, which must equal the actual `APP_ORIGIN`. Use sign-in and a mounted Domain page in the browser. Check `./bilibi status`, the development identity and saved-version differences. Run strict checks with another empty disposable database and verify release packaging with the pinned builder. Record proof under [runtime parity](runtime-parity.md). Stop only validation-owned processes by their recorded PID. Keep the original process and runtime data available throughout.

## Adopt existing sources deliberately

**Adoption is a manual move after parallel validation; setup never moves or replaces repositories.** Staying with the parallel clones is also supported.

1. Close only the original checkout's editor/build processes that you own, by PID. Leave database and shared proxy services alone. Record how to restart the original launcher.
2. Resolve the original Platform path and the new application's `sources/platform` parent with `realpath`. Confirm both are the paths you inventoried, neither contains the other, and the source is an ordinary clone with a `.git/` directory. Do not move a linked Git worktree; arrange its migration with its owner.
3. Preserve the newly created parallel source clone under a separate unused backup path outside `sources/`. Check that destination is absent, then move only that clone. Never overwrite it or use `rm -rf` to make space.
4. Confirm `sources/platform` is absent, including dangling symlinks. Move the original Platform clone there, with its nested Domain and Extension clones intact. Keep histories, branches, remotes, staged files, untracked work, `.local/`, configuration and ignored runtime files. Do not run a reset, clean or checkout. If any move crosses filesystems, stop and plan a verified metadata-preserving copy before proceeding.
5. Inspect `git status` in every Git root. Compare with the private inventory. Resolve absolute media paths, launcher/editor paths and output paths deliberately. An existing Platform `target/` conflicts with the application's output link: preserve it outside `sources/`, then let setup create its link into `build/target`. Never delete source or runtime data to satisfy setup.
6. Keep the original runtime database, media roots and original encryption/signing keys together. Do not generate new keys for existing encrypted data. Update private environment configuration and service paths only after reviewing those references. Run `./bilibi setup` and `./bilibi status`; setup keeps dirty files and branches. Pin only after committing and publishing the intended source selection.
7. Switch editor and launcher paths after the adopted checkout has been used successfully. Retain the protected pre-migration backups and the displaced parallel clone until rollback is no longer needed.

For an old template layout, the same procedure moves its existing `build/platform/` clone into the outer repository's absent `sources/platform/`. Keep the remaining `build/` contents for review. `BILIBI_BUILD_DIR` still means outputs; `BILIBI_SOURCE_DIR` means sources. Do not make either directory contain the other. Do not save a machine-specific path override in the recipe.

## Roll back

Before adoption, rollback means stopping the parallel launcher by its own PID and reopening the unchanged original checkout. Keep the disposable database separate; no original data was migrated.

After adoption, stop only the launcher processes you started at the new path. Check that the old source destination is absent, then reverse the recorded move, including its nested repositories and runtime files. Restore the old editor, launcher, output and private configuration paths from the inventory. If a temporary output symlink was added, inspect its target and remove only that symlink before restoring the preserved output directory. Verify every repository's status against the inventory, then restart the original launcher with its original database, media roots and keys.

Do not restore a database backup over a live database as part of source rollback. If runtime data was changed after the switch, use the existing database recovery procedure and agree on the intended recovery point first. Setup and migration never delete the old runtime data or backups.

## Where to go next

- [Versions and release instructions](versions.md).
- [Domain mounting and contributions](../apps/domains/AGENTS.md) and [Extension mounting](../apps/extensions/AGENTS.md).
- [ADR 03](architecture/decisions/03-application-workspaces-one-front-door.md) for the workspace decision.
