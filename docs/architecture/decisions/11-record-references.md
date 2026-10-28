# ADR 11: Record references across modules

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5-high (author)
**Scope:** How a module points at another module's record (`base/records`)
**Last Updated:** 2026-10-27

> Source: [issue 136](https://github.com/BelimbingApp/bilibi/issues/136).

## Context

**Modules need to point at records other modules own, across Domains, without depending on them.** An activity about a case, a document about an invoice. Domains may not depend on each other, and a port per pair of modules would re-implement names, access and paths for every pair. The same record was also named four ways: `person` (audit), `persons` (Grid table), `core.person` (Media subject), `core/person` (workspace kind).

## Decision

**One Base registry, `base/records`, names, links and checks a reference `(record_kind, record_id)` for any module, keyed by the owner's audit kind.**

| Rule | What it means |
| --- | --- |
| One key per kind | The kind key is the audit kind the owner already writes in `Change.kind`, and the kind's Grid table claims it in `record_types`. No new name for a record. |
| Merges through redirects | An owner records `from → to` in its own merge transaction; every reader follows it. Holders' rows are never rewritten, and an event is not needed. |
| A Base module | Above Grid and UI: Grid is static SQL with no owner code or page, and UI cannot name records because Grid depends on it. |

The module's README is the contract: [`apps/base/records/README.md`](../../../apps/base/records/README.md).

## Consequences

- A module offers its records for reference with one access check and one declaration; a holder needs only two text columns.
- Names, links and checks follow the reader's permissions in one place, and an unmounted owner leaves holders working.
- Moving Media subjects and Finance attachment sources onto record kinds is possible later and is not part of this decision.
