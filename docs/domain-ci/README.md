# Domain CI

Every Domain or Extension repository runs the Platform's gates with itself mounted; the Platform runs the same gates with every official Domain and Extension in `catalogue.json` mounted separately.

| Where | Workflow | Platform | Domain or Extension |
| --- | --- | --- | --- |
| Domain or Extension repository | `ci.yml` here, copied to `.github/workflows/ci.yml` | pinned in `.github/bilibi-revision` | the change |
| Bilibi | `.github/workflows/domains.yml` (jobs named `domains/finance`, etc.; stable check `Official compatibility`) | the change | `main` HEAD, or a companion branch selected in the Platform PR body |

- Both run `scripts/domain-ci.sh`: discovery, committed CSS, Cargo.lock pinning, fmt, clippy, migrate, seed-production, and `GET /`.
- A pull request lints and boots a debug build. A push to `main`, a version tag, and a run you start by hand also do the locked release build and boot that program. A push to `main` runs the debug build too, so pull requests restore a warm cache. Only the release runs are recorded as tested sets.
- A Bilibi pull request whose files cannot affect a mounted Domain or Extension passes **Official compatibility** without mounting. A `Bilibi-Companion:` line still mounts. The paths and the companion exception are in [`docs/versions.md`](../versions.md#tested-sets).
- A Bilibi change that turns "Domains on this change" red must fix the official Domain or Extension in the same task. Link the companion PR and select its branch using [the companion-PR convention](../versions.md#platform-changes-and-companion-prs); never mark an entry allowed-to-fail.
- On a Platform release tag (`v0.1.0`), or a run started by hand, "Domains on this change" takes each Domain and Extension at its latest release. Manual runs can select `main` instead. Existing Domain tested-set recording remains unchanged. Versions, tags and the catalogue: [`docs/versions.md`](../versions.md).
