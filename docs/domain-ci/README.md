# Domain CI

Every Domain or Extension repository runs the Platform's gates with itself mounted; the Platform runs the same gates with every official Domain and Extension in `catalogue.json` mounted separately.

| Where | Workflow | Platform | Domain or Extension |
| --- | --- | --- | --- |
| Domain or Extension repository | `ci.yml` here, copied to `.github/workflows/ci.yml` | pinned in `.github/bilibi-revision` | the change |
| Bilibi | `.github/workflows/domains.yml` (jobs named `domains/finance`, etc.; stable check `Official compatibility`) | the change | `main` HEAD, or a companion branch selected in the Platform PR body |

- Both run `scripts/domain-ci.sh`: discovery, committed CSS, Cargo.lock pinning, fmt, clippy, locked release build, migrate, seed-production, and `GET /`.
- A Bilibi change that turns "Domains on this change" red must fix the official Domain or Extension in the same task. Link the companion PR and select its branch using [the companion-PR convention](../versions.md#platform-changes-and-companion-prs); never mark an entry allowed-to-fail.
- On a Platform release tag (`v0.1.0`), or a run started by hand, "Domains on this change" takes each Domain and Extension at its latest release. Manual runs can select `main` instead. Existing Domain tested-set recording remains unchanged. Versions, tags and the catalogue: [`docs/versions.md`](../versions.md).
