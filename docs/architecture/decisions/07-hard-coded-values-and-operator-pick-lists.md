# ADR 07: Hard-coded values and operator pick-lists

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5-high (author)
**Scope:** Which value lists stay in code and which an operator may edit (`base/picklists`)
**Last Updated:** 2026-10-22

> Source: [issue 129](https://github.com/BelimbingApp/bilibi/issues/129).

## Context

**Modules need dropdowns whose values adopters change without a release.** Case categories, industries, loss reasons and lead sources differ per tenant. Until now each catalog was bespoke (`core/company` legal entity types, relationship roles). Some values, though, drive behaviour: a status that pauses a timer, a won or lost stage, a provider's delivery result. If an operator could rename or retire those, module code would break.

## Decision

**A value a module's logic branches on stays in code; a value the module only stores, shows, filters and reports on is an operator pick-list.**

| Kind | Where | Who changes it |
| --- | --- | --- |
| Logic value | A Rust enum, or a `base/workflow` status | A developer, in a release |
| Curated value | A pick-list declared to `base/picklists` | Each tenant's administrators (the platform operator for an installation-wide list) |
| Curated value that code must recognise | A pick-list value declared `locked` | The operator may relabel, translate and reorder it, but not retire it |

### Behaviour and defaults

- **Module code names only locked codes.** Naming any other code of a pick-list is a defect: the operator may retire it.
- **Records store the code.** Never a label or a row id. A code never changes.
- **Retire, not delete.** Records keep retired codes, and they still show, filter and report.
- **Starting values are reconciled by code and never overwrite the operator's edits.**

## Consequences

- A new module offers a curated dropdown with one declaration and no table, seed or admin page.
- Reviewers ask one question of each new value list: does any code branch on it? If yes, it stays in code.
- Moving `core/company`'s catalogs onto pick-lists is possible later and is not part of this decision.
