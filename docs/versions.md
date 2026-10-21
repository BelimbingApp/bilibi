# Versions, releases and the catalogue

Every release of Bilibi and of each Domain is a Git tag `vX.Y.Z`. `catalogue.json` lists those releases and the pairs CI proved build and run together. The running program knows exactly which commits it was built from and shows them in `bilibi-web status` and in the status bar.

## One example

Say you run `bilibi-web status` on a server that was built from a pin with Finance mounted:

```
Bilibi 0.1.0, release v0.1.0, commit e3fe31709a93bb43ae8c91c20cc0ee2b96301db9
Pin: .local/composition, compiled in at build time
Domains:
  finance 0.1.0, commit 24938fb69a4da94275ae74085ec03671a8efd51e, modules: finance/attachments, finance/einvoice, …
Extensions: none mounted
```

Notice what you learn without opening a file: the Platform is release 0.1.0 (not some commit after it), Finance is release 0.1.0, and the exact commits both came from. The status bar at the bottom of every page shows the same in short form: `v0.1.0` and `finance 0.1.0`, with the commits in the tooltip.

You may already have questions: where does a version number come from, how do you make a release, what counts as "tested together", and what happens on a developer's machine without a pin? We cover each below.

## What a version is

**A version is a Git tag `vX.Y.Z` on the repository it belongs to.** Bilibi (the Platform) and each Domain repository (`bilibi-dom-finance`, `bilibi-dom-factory`, `bilibi-dom-supply-chain`) are versioned on their own, because an adopter picks a Platform version and a version of each Domain separately.

| | Where the number lives | The tag |
| --- | --- | --- |
| Platform | `version` under `[workspace.package]` in `Cargo.toml` | `vX.Y.Z` on the commit whose `Cargo.toml` says `X.Y.Z` |
| Domain | nowhere in the repository | `vX.Y.Z` on any commit of `main` |

A Domain has no version file: its module crates inherit the Platform's `version.workspace`, which is deliberate (a Domain never builds alone), so the tag is the version. `scripts/discover-modules.py --pin` refuses a Platform commit whose tag and `Cargo.toml` disagree.

The numbers follow [semantic versioning](https://semver.org), read for software that is still `0.x`:

| Change | Platform | Domain |
| --- | --- | --- |
| Bug fix, wording, same tables and contracts | patch `0.1.0 → 0.1.1` | patch |
| New page, module or operation; a migration that only adds | minor `0.1 → 0.2` | minor |
| A contract a Domain depends on changes (an `Operation` field, a port, an event shape) | **minor**, and every Domain needs a release that says so | — |
| A Domain needs a newer Platform than before | — | **minor**, with the new `platform` requirement in the catalogue |

> [!NOTE]
> Before `1.0.0`, a minor change may break the Domains. That is why the catalogue records which pairs were tested, rather than trusting the numbers.

## How to release

The Platform and every official Domain and Extension stay in step. [ADR 02](architecture/decisions/02-build-in-development-keep-everything-in-step.md) is the decision. A release tag is created only after **Official compatibility** has passed for that exact commit, against every catalogue entry at its latest release. Nobody adds a version to the catalogue by hand: the Catalogue workflow lists each release from its tag.

**The Platform.** `Cargo.toml` on `main` already says the version you are releasing, for example `0.1.1`. Every merged pull request since the previous tag has an `Upgrade notes for agents` section. Gather them and commit the file:

```sh
python3 scripts/release.py gather
git add docs/upgrade-notes/v0.1.1.md
git commit -m "Document upgrade notes for v0.1.1"
git push origin main
```

Then run **Release** from the Actions tab, on `main`. The workflow refuses any other branch. It refuses when the notes file is missing or when Official compatibility is not green for that commit. When the check passes, the workflow pushes the tag `v0.1.1`, publishes the GitHub release and lists the version in the published catalogue. The release body is `docs/upgrade-notes/v0.1.1.md`.

Do not run `git tag` and `git push` for a release tag yourself. A hand-pushed `v*` tag skips the gate. The tag push from the Release workflow uses the built-in token, which does not start a second run, so the tested sets are recorded in that same run.

After the tag exists, open a pull request that bumps `Cargo.toml` to the next number, so `main` never claims a number that was released.

**A Domain or Extension.** Copy [`docs/domain-ci/ci.yml`](domain-ci/ci.yml) to `.github/workflows/ci.yml`. No secret is needed. From a commit whose CI is green:

```sh
git tag v0.1.1 <commit>
git push origin v0.1.1
```

The tag runs the mounted gates. Bilibi's Catalogue workflow runs once a day and lists the release when the `Mounted in Bilibi` check passed on the tagged commit, so a new release can take up to a day to appear. Anyone with write access to Bilibi can run it sooner from the Actions tab. A repository that is not listed in `catalogue.json` is never fetched.

Write `.github/bilibi-platform` as one line, for example `>=0.1.0, <0.2.0`, only when this release needs a different Platform than the previous one. Otherwise the previous requirement is kept. The release is not listed when the Platform commit pinned in `.github/bilibi-revision` does not satisfy the requirement; the Catalogue run reports why.

To record the pair as tested before the next Platform release, run `Domains on this change` by hand from the Actions tab on Bilibi `main` with `domains_at` left at `latest-release`.

**Fixing a tag.** A tag is a promise other people build from. If a release is wrong, release the next number instead of moving the tag.

## Application releases

**Keep your application's saved composition in its own repository.** After trying a Platform clone, run `./bilibi new <name>` to create it. Domains and Extensions are optional. The [workspace guide](application-workspaces.md) explains Git ownership and migration; direct Platform, Domain and Extension clones remain supported for focused contributions.

In the application root, commit and push component changes in their owning repositories, run `./bilibi pin`, then review and commit `setup.toml`, `composition/manifest.txt` and `composition/Cargo.lock` together. The application's `./bilibi check` and `./bilibi build` use its pinned builder, independently fetchable commits and saved lock. They refuse dirty source inputs outside generated composition; they do not resolve an unlocked replacement lock. Checks need their own empty disposable database.

Follow the generated application's README and release workflow to publish its verified archive. Validate it on Staging and promote that same archive to Production under ADR 02. This does not replace the Platform release workflow above or the server installer's backup and rollback rules.

## Upgrade notes for agents

Every pull request has a heading `Upgrade notes for agents`. Say what a Domain or Extension owner must change, and the exact steps for their agent. Write `None` when nothing of theirs changes. The pull request template carries the heading. `python3 scripts/release.py require-notes` refuses a pull request that leaves it empty, and the Upgrade notes workflow runs that on every pull request and again when its body is edited.

`python3 scripts/release.py gather` writes `docs/upgrade-notes/vX.Y.Z.md` from the pull requests merged after the previous `vX.Y.Z` tag. The GitHub release body is that file. An agent updating a private Domain or Extension reads the file for the release it is moving to.

## The catalogue

`catalogue.json` at the root of Bilibi lists the software an adopter can choose from. On `main` it names the official Domains and Extensions; a person adds or removes one by pull request. The published copy adds every released version and the tested sets, and CI writes it to the `catalogue` branch with its built-in token. It is at:

```
https://raw.githubusercontent.com/BelimbingApp/bilibi/catalogue/catalogue.json
```

Read the published copy, never `main`'s: `main`'s `tested` list may hold sets recorded by hand, each with the run that proved it, but only the published copy has the sets CI recorded. CI rebuilds the published copy from `main`'s file plus every version and set recorded so far, so a change merged to `main` reaches the published copy at the next run. `main`'s own version lists are only a starting point and are not kept in step with the published copy.

The file has five required parts and an optional extension list:

| Part | What it holds | Who writes it |
| --- | --- | --- |
| `format` | `1` | nobody; it changes only with this document |
| `platform` | name, repository, and each released version with its date and commit | the Release workflow, after Official compatibility passes and the tag exists |
| `domains` | per Domain ID (the folder it mounts at): name, one-line description, repository, module folders, and each released version with its date, commit and the Platform versions it needs (`">=0.1.0, <0.2.0"`) | the Catalogue workflow, from the Domain's release tags |
| `extensions` (optional) | Extension IDs with descriptions, repositories, modules and release requirements, shaped like `domains`; no tested pair is inferred from a requirement | the Catalogue workflow, from the Extension's release tags, the same way as a Domain |
| `plugins` | per plug-in ID: name, publisher, repository, the Domain it belongs to (`null` for a Platform plug-in) and its versions, in the same shape | a person; empty until the plug-in runtime ships |
| `tested` | one entry per Platform commit and Domain commit that passed the gates together, with both versions, the time and the CI run | CI, through the `catalogue` branch; a person may also record a set by hand on `main` with the run that proved it, and CI merges those with its own |

A `platform` requirement is comparators joined by commas: `>=0.1.0, <0.2.0`. The operators are `>=`, `<=`, `>`, `<` and `=`. `python3 scripts/catalogue.py --check` refuses a catalogue that is not well formed, and CI runs it on every change, so a typo never reaches the published copy.

## How releases reach the catalogue

**Bilibi fetches; no Domain pushes.** A Domain repository holds no secret and cannot write to Bilibi. `.github/workflows/catalogue.yml` runs once a day and on demand, using only its own `GITHUB_TOKEN`:

1. It reads `catalogue.json` from `main` for the list of official Domains and Extensions, and the `catalogue` branch for what is already published.
2. It reads the `vX.Y.Z` tags of Bilibi itself first and lists each Platform tag that has a GitHub release, which only the Release workflow makes. A tag with no release is reported as waiting and is not listed. Then, for each listed repository, it reads the `vX.Y.Z` tags (these repositories are public, so no token is needed to read them). A tag that is not listed yet becomes a version when its tagged commit has a passed `Mounted in Bilibi` check, its Platform requirement is valid (`.github/bilibi-platform` at the tag, else the previous version's), and the Platform commit in `.github/bilibi-revision` satisfies that requirement. The release date is the date the tag was made.
3. It writes the result to the `catalogue` branch. Because the workflow pushes that branch itself and `main` is never written, `main` keeps its pull request rule and the required `Official compatibility` and `check` checks.

A release whose check has not finished is retried at the next run. One that cannot be listed (a failed check, a requirement no listed Platform satisfies, a tag moved after it was listed) is reported as an error on the run and does not stop the other releases. The Release workflow calls the same workflow for the Platform version it just tagged, so a Platform release is listed at once. If that call fails, the next daily run lists the Platform release itself.

To list a release sooner than the next daily run, run **Catalogue** from the Actions tab. Tick `dry_run` to see the change in the run summary without writing it. GitHub stops scheduled workflows after 60 days without repository activity; running **Catalogue** by hand starts them again.

## Tested sets

**A tested set is one Domain, mounted alone on one Platform commit, that passed the release gates in `scripts/domain-ci.sh`:** discovery, the committed CSS, a pinned `Cargo.lock`, format and lint, a locked release build, a fresh migration, the production seeds and a `GET /`. It is recorded as the two commits, each with the release tag it sits on (or `null` for a commit between releases), the time, and the CI run to read.

```json
{
  "platform": {"version": "0.1.0", "commit": "e3fe31709a93bb43ae8c91c20cc0ee2b96301db9"},
  "domain": {"id": "finance", "version": "0.1.0", "commit": "24938fb69a4da94275ae74085ec03671a8efd51e"},
  "tested_at": "2026-10-19T09:12:44Z",
  "run": "https://github.com/BelimbingApp/bilibi/actions/runs/39143900504"
}
```

> [!WARNING]
> Each Domain is tested on its own. Three Domains that each have a set with the same Platform commit were not necessarily built together, because the gates do not mount them side by side.

How CI records a set (`.github/workflows/domains.yml`):

1. On a push to `main`, and on a pull request that can affect a mounted Domain or Extension, every official entry in `catalogue.json` is mounted separately at its `main` HEAD and the gates run. A pull request can select companion branches as described below. Nothing is recorded: these runs protect `main`, they are not releases. A pull request lints and boots a debug build. A push to `main` does the locked release build, and also the debug build so that pull requests start from a warm cache.
2. A pull request passes **Official compatibility** without mounting when every changed file cannot affect a Domain or Extension. The unmounted `Rust` workflow still checks the committed CSS on those pull requests, because Tailwind scans the whole repository. That is `docs/`, Markdown that no Rust source compiles in with `include_str!` or `include_bytes!`, `LICENSE`, `.gitignore`, `.editorconfig`, `.gitattributes`, `compose.yml`, `.agents/`, `.amp/`, `.claude/`, and `.github/` except `.github/workflows/domains.yml`. Anything else mounts. A `Bilibi-Companion:` line mounts even when the files would not. `scripts/compatibility.py` is the list.
3. On a release tag `v*`, each Domain and Extension is mounted at its latest release tag instead, and the locked release build runs. On a hand-started run, `domains_at` chooses between the two. The Release workflow calls the same workflow with `domains_at` set to `latest-release` before the tag exists. Companion branches never apply to tags, manual runs, pushes to `main` or that call. The unmounted Rust workflow skips the same kind of docs-only pull request, except for the CSS check, and it skips `.github/` except `.github/workflows/ci.yml`.
4. After the gates pass, `python3 scripts/catalogue.py tested` reads the pin the gates made (`.local/composition/manifest.txt`) and writes the set. The Release workflow passes `--platform-version` from `Cargo.toml`, because the tag is created after this run.
5. The `Record the tested sets` job then runs `python3 scripts/catalogue.py publish`, which merges `catalogue.json` from the run's checkout, the sets already on the `catalogue` branch and the new ones, and pushes the result to that branch. The same pair recorded twice keeps the newer run. A tag pushed by hand still records this way. The Release workflow's own call records when Official compatibility passed, which is the run that publishes a release, because the tag it pushes does not start a second workflow.

Extension checks use the same gates. The existing tested-set format records Domain pairs only; Extension checks do not add records or change catalogue publishing.

## Platform changes and companion PRs

**Fix a broken official Domain or Extension in the same task as the Platform change.** Official means listed in `catalogue.json`, whether it is a Domain or an Extension. Public visibility alone does not make a repository official; a private Domain is possible too.

- Open a companion PR in each affected repository and link it from the Platform PR. Link back to the Platform PR from each companion.
- Keep the Platform and companion checks green, and merge the changes together. For a repository owned by someone else, open the companion PR and notify its owner.
- Do not change Domains and Extensions outside the catalogue, including private repositories, from a Platform task.
- Require the stable `Official compatibility` check from `Domains on this change` in branch protection. It fails if catalogue selection or any mounted job fails or is cancelled; no official entry is allowed to fail. A pull request that cannot affect a mount passes this check with the mount skipped.

To check a companion branch with a Platform PR, add one standalone line to the Platform PR body for each affected entry:

```text
Bilibi-Companion: domains/finance=fix/platform-change
```

Use `extensions/<catalogue-id>=<branch>` for an Extension. The branch must exist in that entry's catalogue repository. Fork repository overrides, duplicate entries, unknown entries and malformed directives are refused. Entries without a directive still use `main`. Link the companion PR alongside the directive; the directive selects a branch, it does not replace that link.

Opening or editing the Platform PR body starts a new run, as does pushing a Platform commit. After pushing a new companion commit, rerun the workflow or edit the Platform PR body to start a fresh run. A companion push alone does not trigger Platform CI. Check the mounted job summary for the exact repository, ref and commit used. For the companion's own CI to check the new Platform API, update its `.github/bilibi-revision` to the Platform commit under review.

The matrix comes from `scripts/compatibility.py`, reading `domains` and optional `extensions` from the checkout's `catalogue.json`. Each job runs the shared `scripts/domain-ci.sh` gates. A pull request formats, lints, builds a debug `bilibi-web`, migrates, seeds and loads `/`. A push to `main`, a tag and a hand-started run also do the locked release build and boot that binary. A failing job names the Domain or Extension and points back to this companion-PR rule.

## Versions inside the program

**The program is the truth about what it contains.** `scripts/discover-modules.py --pin` records the Platform commit, its version and release tag, and each mounted Domain's commit and release in `.local/composition/manifest.txt`. The executable host's build script (`apps/web/build.rs`) compiles that manifest into the program, and `bilibi_web::composition` hands it to `bilibi-web status`, the status bar and anything else that asks. Nothing is read from a file or the database at run time, so the shown versions cannot drift from the running code.

Pin, then build:

```sh
python3 scripts/discover-modules.py --pin
cargo build --release --locked -p bilibi-web
./target/release/bilibi-web status
```

The pin is read at build time, so **pin before you build**. The build also checks the pin against the checkouts it compiles: when Git is available and the Platform or a Domain is at another commit than the pin recorded, the pin is reported stale instead of shown.

**Platform-only setups can be pinned too.** With no Domains or Extensions mounted, `--pin` records the Platform revision, version and resolved `Cargo.lock`, with an empty `[containers]` table. On a checkout at that Platform revision, copy `.local/composition/` into place and run `python3 scripts/discover-modules.py --restore` to restore the lockfile. The same checks apply: a changed Platform revision, an altered pinned lockfile or a mounted container absent from the pin is refused.

What you see in each state:

| State | `status` says | Status bar |
| --- | --- | --- |
| Pinned at a release | `Bilibi 0.1.0, release v0.1.0, commit …` and `finance 0.1.0, commit …` | `v0.1.0` `finance 0.1.0` |
| Pinned between releases | `unreleased, commit …`; a Domain at no tag reads `untagged, commit …` | `v0.1.1` `finance 24938fb69a4d` |
| No pin (a developer build) | `built from commit …` and `Pin: none`; Domains read `unpinned` | `v0.1.1` `finance unpinned` |
| Stale pin (mounted set, `Cargo.toml` or a checkout's commit changed after the pin) | `Pin: stale, …` with what differs; no commits are shown, because they would be wrong | `finance unpinned`, the reason in the tooltip |

Hover a badge in the status bar for the commit and the module list.

## Where to go next

- Mounting, pinning and restoring a composition: [`apps/domains/AGENTS.md`](../apps/domains/AGENTS.md).
- The gates a tested set passes, and the Domain-side workflow: [`docs/domain-ci/README.md`](domain-ci/README.md).
- The architecture and its layers: [`docs/architecture/README.md`](architecture/README.md).
