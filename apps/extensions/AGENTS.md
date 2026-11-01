# Project agent memory

This file is the project's committed home for project-intrinsic agent knowledge: build, test, release, architecture, and sharp-edge notes that should travel with the code.

- Correct entries that work proves wrong; add new ones only by deliberate maintainer choice, never as routine task output.

## Mounting Extensions

- An Extension is its own Git repository, `BelimbingProject/bilibi-ext-<id>` (or a company's own repository), mounted as a real nested checkout at `apps/extensions/<name>`, where `<name>` is the `<id>` in snake_case. Mount with `git clone <repository URL or local clone> apps/extensions/<name>`, then `python3 scripts/discover-modules.py` and any Cargo command.
- Its root `bilibi.container.toml` declares `id = "<name>"` and `layer = "extension"`; modules follow the Domain rules in `apps/domains/AGENTS.md` (crate `bilibi-<name>-<module>`, no root `Cargo.toml`, `[workspace]` or `Cargo.lock`, no symlinks, no features of shared crates). A Domain or Extension ID may be mounted under only one of `apps/domains` and `apps/extensions`.
- Never commit `apps/extensions/Cargo.toml`, `apps/extensions/src/lib.rs` or `Cargo.lock` from a mounted checkout; commit only inside the Extension's own repository and make Platform changes with nothing mounted. `python3 scripts/discover-modules.py --check` refuses a staged or committed mounted composition.
