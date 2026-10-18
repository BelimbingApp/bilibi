# Domain CI

Every Domain or Extension repository runs the Platform's gates with itself mounted; the Platform runs the same gates with each Domain mounted.

| Where | Workflow | Platform | Domain |
| --- | --- | --- | --- |
| Domain repository | `ci.yml` here, copied to `.github/workflows/ci.yml` | pinned in `.github/bilibi-revision` | the change |
| Bilibi | `.github/workflows/domains.yml` (checks named "Domains on this change / finance" and so on) | the change | default-branch HEAD |

- Both run `scripts/domain-ci.sh`: discovery, committed CSS, Cargo.lock pinning, fmt, clippy, locked release build, migrate, seed-production, and `GET /`.
- A Bilibi change that turns "Domains on this change" red must fix the Domain in the same effort: open the Domain fix alongside the Platform change, never mark the entry allowed-to-fail.
