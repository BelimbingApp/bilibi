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

**The Platform.** From a green `main`:

```sh
git tag v0.1.1 <commit>         # Cargo.toml at that commit must say 0.1.1
git push origin v0.1.1
```

The tag starts the `Domains on this change` workflow with each Domain at its latest release, and every pair that passes is recorded as a tested set (below). Then open a pull request that bumps `Cargo.toml` to the next number, so `main` never claims a number that was released.

**A Domain.** From its green `main`:

```sh
git -C bilibi-dom-finance tag v0.1.1 <commit>
git -C bilibi-dom-finance push origin v0.1.1
```

Then add the version to `catalogue.json` under that Domain, with the Platform versions it needs, and open a pull request to Bilibi. To record it as tested, run `Domains on this change` by hand from the Actions tab on `main` with `domains_at` left at `latest-release`.

**Fixing a tag.** A tag is a promise other people build from. If a release is wrong, release the next number instead of moving the tag.

## The catalogue

`catalogue.json` at the root of Bilibi is the list of software an adopter can choose from. People edit it on `main`; CI publishes it with the tested sets at:

```
https://raw.githubusercontent.com/BelimbingApp/bilibi/catalogue/catalogue.json
```

Read the published copy, never `main`'s: `main` carries no tested sets. CI rebuilds the published copy from `main`'s file plus every set recorded so far, so a hand edit on `main` reaches the published copy at the next recorded run.

The file has five parts:

| Part | What it holds | Who writes it |
| --- | --- | --- |
| `format` | `1` | nobody; it changes only with this document |
| `platform` | name, repository, and each released version with its date and commit | a person, when a Platform release is tagged |
| `domains` | per Domain ID (the folder it mounts at): name, one-line description, repository, module folders, and each released version with its date, commit and the Platform versions it needs (`">=0.1.0, <0.2.0"`) | a person, when a Domain release is tagged |
| `plugins` | per plug-in ID: name, publisher, repository, the Domain it belongs to (`null` for a Platform plug-in) and its versions, in the same shape | a person; empty until the plug-in runtime ships |
| `tested` | one entry per Platform commit and Domain commit that passed the gates together, with both versions, the time and the CI run | CI, through the `catalogue` branch |

A `platform` requirement is comparators joined by commas: `>=0.1.0, <0.2.0`. The operators are `>=`, `<=`, `>`, `<` and `=`. `python3 scripts/catalogue.py --check` refuses a catalogue that is not well formed, and CI runs it on every change, so a typo never reaches the published copy.

## Tested sets

**A tested set is one Domain, mounted alone on one Platform commit, that passed `scripts/domain-ci.sh`:** discovery, the committed CSS, a pinned `Cargo.lock`, format and lint, a locked release build, a fresh migration, the production seeds and a `GET /`. It is recorded as the two commits, each with the release tag it sits on (or `null` for a commit between releases), the time, and the CI run to read.

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

1. On a pull request or a push to `main`, each Domain is mounted at its `main` HEAD and the gates run. Nothing is recorded: these runs protect `main`, they are not releases.
2. On a release tag `v*`, each Domain is mounted at its latest release tag instead. On a hand-started run, `domains_at` chooses between the two.
3. After the gates pass, `python3 scripts/catalogue.py tested` reads the pin the gates made (`.local/composition/manifest.txt`) and writes the set.
4. The `Record the tested sets` job then runs `python3 scripts/catalogue.py publish`, which merges `catalogue.json` from the run's checkout, the sets already on the `catalogue` branch and the new ones, and pushes the result to that branch. The same pair recorded twice keeps the newer run.

## Versions inside the program

**The program is the truth about what it contains.** `scripts/discover-modules.py --pin` records the Platform commit, its version and release tag, and each mounted Domain's commit and release in `.local/composition/manifest.txt`. The module registry's build script (`apps/base/module_registry/build.rs`) compiles that manifest into the program, and `bilibi_module_registry::composition` hands it to `bilibi-web status`, the status bar and anything else that asks. Nothing is read from a file or the database at run time, so the shown versions cannot drift from the running code.

Pin, then build:

```sh
python3 scripts/discover-modules.py --pin
cargo build --release --locked -p bilibi-web
./target/release/bilibi-web status
```

The pin is read at build time, so **pin before you build**: a program built from an older pin shows the older pin.

What you see in each state:

| State | `status` says | Status bar |
| --- | --- | --- |
| Pinned at a release | `Bilibi 0.1.0, release v0.1.0, commit …` and `finance 0.1.0, commit …` | `v0.1.0` `finance 0.1.0` |
| Pinned between releases | `unreleased, commit …`; a Domain at no tag reads `untagged, commit …` | `v0.1.1` `finance 24938fb69a4d` |
| No pin (a developer build) | `built from commit …` and `Pin: none`; Domains read `unpinned` | `v0.1.1` `finance unpinned` |
| Stale pin (mounted set or `Cargo.toml` changed after the pin) | `Pin: stale, …` with what differs; no commits are shown, because they would be wrong | `finance unpinned`, the reason in the tooltip |

Hover a badge in the status bar for the commit and the module list.

## Where to go next

- Mounting, pinning and restoring a composition: [`apps/domains/AGENTS.md`](../apps/domains/AGENTS.md).
- The gates a tested set passes, and the Domain-side workflow: [`docs/domain-ci/README.md`](domain-ci/README.md).
- The architecture and its layers: [`docs/architecture/README.md`](architecture/README.md).
