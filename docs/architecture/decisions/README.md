# Decision records

**Document Type:** Index of architecture decision records
**Status:** Active
**Scope:** Decisions that shape Bilibi's architecture
**Last Updated:** 2026-10-17

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
- Write only what the agent knows. Omit an unknown part rather than guess it.

## Records

| Record | Status |
| --- | --- |
| [0001 Agent-first operations: one contract, four doors](./0001-agent-first-operations.md) | Accepted |
