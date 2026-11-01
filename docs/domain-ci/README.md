# Domain CI

Every Domain or Extension repository runs the Platform's gates with itself mounted; the Platform runs the same gates with every official Domain and Extension in `catalogue.json` mounted separately.

| Where | Workflow | Platform | Domain or Extension |
| --- | --- | --- | --- |
| Domain or Extension repository | `ci.yml` here, copied to `.github/workflows/ci.yml` | pinned in `.github/bilibi-revision` | the change |
| Bilibi | `.github/workflows/domains.yml` (jobs named `domains/finance`, etc.; stable check `Official compatibility`) | the change | `main` HEAD, or a companion branch selected in the Platform PR body |

- Both run `scripts/domain-ci.sh`: discovery, CSS compilation and coverage, Cargo.lock pinning, fmt, clippy, migrate, seed-production, and `GET /`.
- A pull request lints and boots a debug build. A push to `main`, a version tag, and a run you start by hand also do the locked release build and boot that program. A push to `main` runs the debug build too, so pull requests restore a warm cache. Only the release runs are recorded as tested sets.
- A Bilibi pull request is mounted as the change merged into the live base branch (`scripts/merge-live-base.sh`), so a Platform change and its Domain companion that merged after the pull request was last pushed do not fail it. A conflict with the live base fails the job and asks the author to update the branch.
- A Bilibi pull request whose files cannot affect a mounted Domain or Extension passes **Official compatibility** without mounting. A `Bilibi-Companion:` line still mounts. The paths and the companion exception are in [`docs/versions.md`](../versions.md#tested-sets).
- A Bilibi change that turns "Domains on this change" red must fix the official Domain or Extension in the same task. Link the companion PR and select its branch using [the companion-PR convention](../versions.md#platform-changes-and-companion-prs); never mark an entry allowed-to-fail.
- On a Platform release tag (`v0.1.0`), or a run started by hand, "Domains on this change" takes each Domain and Extension at its latest release. Manual runs can select `main` instead. Existing Domain tested-set recording remains unchanged. The Release workflow runs that same check before it creates the tag. Versions, tags and the catalogue: [`docs/versions.md`](../versions.md).
- A Domain or Extension tag `vX.Y.Z` runs these gates. Bilibi's daily Catalogue workflow then lists the release when the `Mounted in Bilibi` job passed on the tagged commit; the repository needs no secret and opens no pull request. Write `.github/bilibi-platform` (one line, for example `>=0.1.0, <0.2.0`) only when the Platform requirement changes. A repository that is not in the catalogue is never listed. Keep the job named `Mounted in Bilibi`: the Catalogue workflow looks for that check.
