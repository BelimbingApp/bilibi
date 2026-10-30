# Decision records

**Document Type:** Index of architecture decision records
**Status:** Active
**Scope:** Decisions that shape Bilibi's architecture
**Last Updated:** 2026-10-29

Each record states one decision, its context and its consequences. The format follows [Bilimbi's records](https://github.com/BelimbingApp/bilimbi/tree/main/docs/architecture/decisions). The architecture overview is [`../README.md`](../README.md).

## When to write a record

Write one only when a change departs from Bilibi's existing conventions, or picks between real options whose effect lasts beyond the change, so that a later contributor would otherwise ask "why is it like this?" or undo it. Do not write one when the implementation is straightforward and follows the norm: the code, the module's contract and the PR description are enough.

## Statuses

- **Proposed**: written down, not yet agreed by the owner.
- **Accepted**: the owner agreed the direction. The record says what is built and what is not.
- **Accepted; implemented**: the decision is built. Keep the record: code shows what, not why or which options were rejected. Shrink it to the decision, its reasons and rejected alternatives; drop how-it-works detail that duplicates the code.
- **Superseded by ADR NNNN**: a later record replaced it.
- **Withdrawn**: what it describes no longer exists. The line says why.

The owner decides a status. Once the decision is built, use **Accepted; implemented**. When a record is no longer true, mark it **Withdrawn**, or **Superseded by ADR NNNN** if a later record replaced it.

Delete a record only when it never warranted one under the [when-to-write rule](#when-to-write-a-record).

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
| [10 Queued jobs call operation contracts, and modules opt in to CSV import, export and bulk updates](./10-queued-jobs-call-contracts.md) | Proposed |

### Software delivery

| Record | Status |
| --- | --- |
| [02 Build in development and keep everything in step](./02-build-in-development-keep-everything-in-step.md) | Accepted |
| [03 Application workspaces: one front door](./03-application-workspaces-one-front-door.md) | Accepted; implemented |

### Identity

| Record | Status |
| --- | --- |
| [04 Provider-neutral single sign-on](./04-provider-neutral-single-sign-on.md) | Accepted; implemented |
| [09 The current company is a session choice, never a grant](./09-current-company-is-a-session-choice.md) | Proposed |
| [16 Customer accounts are a kind of account, kept out of the staff application by three barriers](./16-customer-accounts-and-portal.md) | Proposed |

### Personal data

| Record | Status |
| --- | --- |
| [05 Personal-data registry, data-subject requests and audit erasure](./05-personal-data-registry.md) | Proposed |

### Search

| Record | Status |
| --- | --- |
| [06 Record search at query time, in the owner's source](./06-query-time-record-search.md) | Proposed |

### Data

| Record | Status |
| --- | --- |
| [07 Hard-coded values and operator pick-lists](./07-hard-coded-values-and-operator-pick-lists.md) | Proposed |
| [08 Currencies as Core master data, and conversion always names its rate](./08-currencies-as-core-master-data.md) | Accepted; implemented |
| [11 Record references across modules](./11-record-references.md) | Proposed |

### Business time

| Record | Status |
| --- | --- |
| [13 Work calendars are Core master data with four fixed day types](./13-work-calendars-in-core.md) | Proposed |

### Files

| Record | Status |
| --- | --- |
| [12 Malware scanning fails closed, through one Extension seam](./12-malware-scanning-fails-closed.md) | Proposed |
| [14 Resumable uploads stage on a Media disk, bound to the file's SHA-256](./14-resumable-uploads-stage-on-a-media-disk.md) | Proposed |

### Approval

| Record | Status |
| --- | --- |
| [15 Shared approval cases in Workflow](./15-shared-approval-cases-in-workflow.md) | Accepted; implementation in progress |
