# ADR 02: Build in development and keep everything in step

**Document Type:** Architecture Decision Record
**Status:** Accepted. Local builds, discovery, release installation, the stored server role, the catalogue-wide compatibility check, the release gate, automatic catalogue entries and upgrade notes for agents and the Software page status are built.
**Agents:** codex/gpt-6 (author)
**Scope:** Where Bilibi is built, how servers receive releases, and how the Platform, official Domains and official Extensions stay compatible
**Last Updated:** 2026-10-20

> The `Agents` value is self-reporting: what the agent reports about itself. Only known parts are included.
>
> These are the owner's decisions of 2026-10-19 and 2026-10-20. This delivery model is Bilibi's own; it has no Bilimbi counterpart.

## Context

**A coding agent is present in development, and Production receives a finished release.** The operator has a local Bilibi repository and works with an agent there. Domains and Extensions compile into the same Rust program as the Platform. Adding one requires building and testing a new composition before it reaches a server.

For example, a Platform change may update a library that Finance also uses. The agent updates Finance in its own repository, opens a companion PR, and checks both changes together. The operator then installs the tested release. Production never edits source or resolves that dependency itself.

Much industry practice supports independently evolving plugins through version ranges, long-lived compatibility promises and matrices of older releases. **Those ranges and matrices matter less in Bilibi because AI keeps the code in step in development.** We choose to fix official software together rather than preserve every older combination. This is a working method, not evidence that an agent's changes are correct: checks and running the resulting composition still matter.

## Decision

**Build locally, release together, and install only tested releases on servers.**

### Development builds

- Development uses the operator's local Bilibi repository and coding agent. The public application starting point also builds the pinned composition in GitHub Actions and publishes release assets.
- Servers never build, including servers labelled Dev or Staging. A Dev role does not enable a compiler or a build job.
- Production never changes code in place. It receives a release built and tested elsewhere, for its operating system, processor and system libraries.
- Adding or updating a Domain or Extension means preparing a new composition in development, keeping the other mounted software, then building and checking it before installation.

### Server tools

**MCP and the Software page discover software, install releases and report status.** They show official Domains and Extensions as Installed, Not installed or Update available, and give instructions for the local checkout.

- Installation takes a tested release, with a review and a fresh 2FA code. It backs up the database before changing it, switches programs and checks the result.
- A failed switch or health check triggers automatic switch-back. When the database changed, returning also needs its backup restored.
- Undo returns to the previous release and restores the backup when needed. Its review must explain which later data will be lost.
- The agreed catalogue states are **Installed / Not installed / Update available**. The complete three-state presentation is part of the work still to be built below.

These tools do not install source modules into the running program. They install the whole prepared release. The first release uses the terminal bootstrap path because there is no running Software page yet.

### Official software

**Official means listed in Bilibi's catalogue.** A public Extension can be official, and a private Domain can be non-official. Domain and Extension describe what the software does; neither name determines visibility or catalogue membership.

- Every Platform change must be checked against every official Domain and Extension.
- If it breaks one, the agent fixes that repository in the same piece of work. The Platform PR and companion PRs link to each other, pass their checks together and merge together.
- For a repository owned by someone else, the agent opens the companion PR and tells its owner. It does not bypass that owner's review or merge authority. The release waits until the required fixes are included.
- Bilibi releases only when the official software fits together. The catalogue updates itself from that release work and its tested records.
- Every release carries upgrade notes written for AI, explaining the changes an owner's agent needs to make to private Domains and Extensions. Owners update that software in development with their own agents; Platform work does not change their private repositories.

### Server role

**Dev, Staging and Production are values of one stored setting, `instance.role`.** Installation sets its initial value. Only a Core Administrator (`core_admin`) in the platform-operator organisation may change it, with a fresh 2FA code and an audit record. Changing the role changes the instance's identity and applicable warnings and 2FA rules; it moves no code or data and enables no server build.

### Current implementation

This table describes the checkout on 2026-10-20. Accepted direction does not mean every part is already implemented.

| Part | State and evidence |
| --- | --- |
| Local builds; no server builder | **Built.** Server-side builds were removed in https://github.com/BelimbingApp/bilibi/pull/60. Discovery and local instructions live in [`base/software`](../../../apps/base/software/README.md) and its [`agent.rs`](../../../apps/base/software/src/agent.rs); server commands live in [`install-server.sh`](../../../scripts/install-server.sh). |
| Tested release installation, fresh 2FA, backup, automatic switch-back and Undo | **Built.** Contracts and access rules are in [`base/software`](../../../apps/base/software/README.md); execution and restore rules are in the [`installer`](../../../apps/installer/README.md) and [`flow.rs`](../../../apps/installer/src/flow.rs). Fresh-server proof was added in https://github.com/BelimbingApp/bilibi/pull/64; the repeatable evidence is in [`server-proof`](../../server-proof/README.md). |
| One editable, audited server role | **Built** in https://github.com/BelimbingApp/bilibi/pull/72. See [`base/system`](../../../apps/base/system/README.md), [`instance.rs`](../../../apps/base/system/src/instance.rs) and [`instance_editor.rs`](../../../apps/base/system/src/instance_editor.rs). |
| Versions, pins and tested pairs | **Built.** The exact commits are compiled into the program. [`versions.md`](../../versions.md) defines the records; [`domains.yml`](../../../.github/workflows/domains.yml) checks three named Domains individually and publishes tested pairs through [`catalogue.py`](../../../scripts/catalogue.py). Separate pairs do not prove a combined composition. |
| Every official Domain and Extension checked on every Platform change | **Built.** [`.github/workflows/domains.yml`](../../../.github/workflows/domains.yml) mounts every `catalogue.json` entry. A pull request that cannot affect a mount passes **Official compatibility** without mounting. See [`docs/versions.md`](../../versions.md) and [`docs/domain-ci/README.md`](../../domain-ci/README.md). |
| Coordinated releases and automatic catalogue updates | **Built.** The [Release](../../../.github/workflows/release.yml) workflow tags `vX.Y.Z` only after Official compatibility passes for that commit. The daily [Catalogue](../../../.github/workflows/catalogue.yml) workflow lists a Domain or Extension tag whose CI from [`docs/domain-ci/ci.yml`](../../domain-ci/ci.yml) passed, with no token. Each Platform release carries [`docs/upgrade-notes/vX.Y.Z.md`](../../upgrade-notes/README.md). |
| Installed / Not installed / Update available | **Built.** Official modules show one status; modules from an operator's own catalogue keep the compatibility wording. See [`base/software`](../../../apps/base/software/README.md#page-and-chatgpt-card) and [`discovery.rs`](../../../apps/base/software/src/discovery.rs). |

## Consequences

**Compatibility work moves into development; release evidence and recovery remain essential.**

- A breaking Platform change includes the official software repairs. That can mean several repositories and owners must finish before a release proceeds.
- Version numbers still identify releases. Exact commits, pinned dependencies and tested records show what was checked; an accepted version range alone proves no test ran.
- Existing version requirements remain useful discovery information. This decision does not remove them or the tested-pair format described in [`versions.md`](../../versions.md).
- Agents must run the checks and use the resulting composition. Neither AI's presence nor individually green Domain pairs proves every combined installation works.
- Backups, health checks, automatic switch-back and Undo remain required. AI cannot make a failed deployment harmless or recover data without a usable backup.
- Database changes need particular care. Migrations are forward-only; switching only the program may leave it facing newer tables. Undo may restore the pre-change database and lose later writes. Follow the [`installer's restore rules`](../../../apps/installer/README.md#undo) and the [`server switch warning`](../../server-install.md#putting-a-release-into-service).
- Private software owners need usable AI upgrade notes and time to check their compositions before adopting a release.

## Open items

**The build host and release transport may evolve without moving builds onto servers.**

| Item | State |
| --- | --- |
| Catalogue-wide CI cross-check | Built. Every official Domain and Extension is checked, and a breaking change still needs a companion pull request. |
| Coordinated release train | Release gating, automatic catalogue entries and upgrade notes for agents are built. The Software page's Update available state is still open. |
| GitHub Actions builds and GitHub release assets | Built through the public [application starting point](https://github.com/BelimbingApp/bilibi-app). Servers install an exact asset or upload; [Software](../../../apps/base/software/README.md) owns the identity, Staging report and install contract. |
