# Decision records

**Document Type:** Index of architecture decision records
**Status:** Active
**Scope:** Decisions that shape Bilibi's architecture
**Last Updated:** 2026-10-21

Each record states one decision, its context and its consequences. The format follows [Bilimbi's records](https://github.com/BelimbingApp/bilimbi/tree/main/docs/architecture/decisions). The architecture overview is [`../README.md`](../README.md).

## Statuses

- **Proposed**: written down, not yet agreed by the owner.
- **Accepted**: the owner agreed the direction. The record says what is built and what is not.
- **Superseded by ADR NNNN**: a later record replaced it.
- **Withdrawn**: what it describes no longer exists. The line says why.

The owner decides a status.

## Header rule

Every record carries these header lines: Document Type, Status, Agents, Scope, Last Updated.

`**Agents:**` lists who wrote and who reviewed the record. Each agent is written as `harness/model-version-effort`, in lower case. The value is self-reporting: what the agent reports about itself.

- One agent: `claude-cloud/sonnet-5.5-default`
- More than one: add the role in parentheses, comma separated: `claude-cloud/sonnet-5.5-default (author), amp/gpt-6-astra-low (review)`
- Other examples: `amp/gpt-6-astra-high`, `codex/sol-high`
- A reviewer is appended after the review.
- If the agent does not know a part, it asks the user who is using the harness (the person who chose the model there, normally the account the agent acts for) and writes what that user says. It never asks the installation owner or a Bilibi administrator. It never guesses. Omit a part nobody can give.

## Numbering

- Records are numbered with two digits, sequentially, in the order the decisions are made: `01`, `02`, `03`.
- A file is named `NN-short-title.md`.
- Bilibi does not copy ADRs from other repositories. Bilimbi and Belimbing records are cited as prior art by link only.

## Records

### Agents and operations

| Record | Status |
| --- | --- |
| [01 Agent-first operations: one contract, four doors](./01-agent-first-operations.md) | Accepted |

### Software delivery

| Record | Status |
| --- | --- |
| [02 Build in development and keep everything in step](./02-build-in-development-keep-everything-in-step.md) | Accepted |
| [03 Application workspaces: one front door](./03-application-workspaces-one-front-door.md) | Accepted; implemented |

### Identity

| Record | Status |
| --- | --- |
| [04 Provider-neutral single sign-on](./04-provider-neutral-single-sign-on.md) | Accepted |
