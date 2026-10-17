# ADR 01: Agent-first operations: one contract, four doors

**Document Type:** Architecture Decision Record
**Status:** Accepted for the direction. The implementation is a follow-up Platform slice.
**Agents:** claude-cloud/sonnet-5.5-default (author), amp/gpt-6-luna-default (review), oracle amp/gpt-6-astra
**Scope:** How a business action is defined once and reached from the GUI, a CLI, an HTTP API and MCP, with the same rules for a human and for an agent acting for a human
**Last Updated:** 2026-10-17

> The `Agents` value is self-reporting: what the agent reports about itself.
>
> Prior art: Bilimbi [ADR 0021](https://github.com/BelimbingApp/bilimbi/blob/main/docs/architecture/decisions/0021-agent-access-and-api-operations.md) (external agents act for a person through granted connections and self-registered API operations). This record keeps that model and widens it from "agent cards" to a contract every door calls.

## Context

**People should not key data by hand. Agents do the routine work.** A person should be able to do all daily work on Bilibi by talking to an agent in a harness, including administration. The harness runs the model. Bilibi runs no model.

Bilibi has four doors. Each serves a different caller.

| Door | Main caller | Also serves |
| --- | --- | --- |
| GUI | Humans | Agents, to a lesser degree |
| CLI | Agents | Scripts and operators, to a lesser degree |
| API (`/api/v1`) | Machines | Agents |
| MCP (`/mcp`) | Agents | |

**Example: the accounting clerk.** Today a clerk keys every incoming and outgoing document and every ledger entry by hand. It is slow and typos creep in. With an agent in a harness, the clerk says what arrived, the agent reads the document, searches for the vendor and drafts the entries. The clerk sees the preview and approves it in the harness, and the agent commits it. Bilibi checks the capability and the tenant's step-up policy, writes, audits and keeps the evidence.

What this needs from the platform:

- An agent finds what it can do without a hand-kept list. The mounted modules differ per installation.
- An agent searches and reads as much as it writes. Most clerical work is lookup: does this vendor exist, which invoice is this, what was posted last month.
- A write is previewed before it is committed, and a human approves it in the harness (the operating rule in section 2). Bilibi does not verify that approval.
- The agent can do whatever the person can do, and never more.
- Every door gives the same answer for the same action. A rule fixed in one door must not stay broken in another.

## Decision

### 1. One operation contract, many callers

**Every action that reads or changes business data is defined once, as an operation contract. The contract does not know who calls it.**

A contract declares:

| Part | Meaning |
| --- | --- |
| Key | Stable and unique. Starts with the owning module ID, `/` written as `.` (`core.company.create`) |
| Description | Plain title and summary, plus a guide where the business needs explaining |
| Inputs and outputs | Typed. Unknown fields are refused |
| Capability | The one capability that opens the action |
| Step-up policy key | Names a step-up policy (section 4). Optional |
| Idempotency | How a repeated call is recognised: a version fence, or a single-use code |
| Preview and commit | For writes: a preview with no business write, then a commit |
| Audit | The change records, taken in the same transaction as the write |
| Channels | Optional: which doors may call it, for technical reasons only (for example an action that needs a browser session). Never a channel restriction on who may do a sensitive action: that is authorization and step-up |

- GUI pages, the CLI, `/api/v1` and `/mcp` are **callers** of contracts. A page's button invokes the contract. For an action a contract covers, it never calls the module facade directly.
- Every door then gets the same authorization, validation, audit, idempotency and step-up with no extra work.
- The platform can check parity. For example, boot or CI can refuse a page write event that does not name a registered contract.
- The owning module registers the contract as a contribution. The host keeps no list and no per-module endpoint (`AGENTS.md` section 4).

### 2. Rules that hold for every caller

- **Agents act as the human.** The person is the actor. The agent is a facet on the scope. Separation of duties still applies: an agent cannot approve what its person may not approve.
- **An agent may perform any action its person may perform, and never more.** That includes authority administration (roles, grants, field restrictions, settings, agent connections) and platform actions, when the person holds them. The agent's reach is the person's live capabilities, read afresh on every call.
- **Step-up protects sensitive actions** (section 4), for agents and humans alike. A capability alone is not the guard on a sensitive action.
- **Impersonation is the one exception to agent/person parity.** An agent does not impersonate. The agent acts as its person, and an agent switching to another identity would break that in the audit trail: the actor would no longer be the person who granted the connection. This is a named exception. Every other action follows the parity rule and is guarded by authorization and step-up, not by channel restrictions.
- **Reads and searches are first-class.** Search, describe, guide and read are contracts too. They are cheap, fresh and capability-filtered.
- **Writes are preview, then a single-use commit.** The preview writes nothing and returns a review and a random code. The commit consumes the code once, rechecks everything, and rolls back all writes and audit rows together if any step fails. **The code proves the reviewed contract, not consent.**
- **Operating rule: a human is present for every agent write and approves it in the harness.** The human enforces this rule. Bilibi does not technically verify the approval: the same agent credential can preview and commit (`apps/base/agent_api/src/plans.rs`). Bilibi's enforced controls are authorization (the capability check) and the tenant's step-up policy (2FA, section 4; not built yet).
- **Accountability.** The person the agent acts for is accountable for each write it commits. The audit trail names that person as the actor.
- **Audit evidence is append-only.** Every change leaves an audit record that cannot be altered. Each module defines its own correction behaviour. Financial and other evidentiary records (ledger entries, posted documents) are corrected by reversal: a new, dated, audited record that reverses or replaces the old one. Ordinary master data may be deleted, with the change audited. Today's code already deletes some master data and keeps the old value in a `Change` (for example department delete, `apps/core/company/src/departments.rs`).
- **Tenant and actor come from the credential**, never from input.

### Agent identity at runtime

- **Bilibi identifies an agent by its connection**: the person it acts for, the registered client, the grant and its expiry. Each request also gets a trace ID, which is the correlation ID of the recorded exchange and of any mutation it commits.
- **The agent also reports itself**: harness, model, version and effort, in the same `harness/model-version-effort` form the ADR headers use (for example `amp/gpt-6-astra-high`).
- Report known values only. Omit an unknown part rather than guess it. Never reject an otherwise authorized request for missing metadata. Ordinary machine callers need not send it.
- That report is labelled **self-reporting**: what the agent reports about itself. It is recorded beside the harness/model-version-effort value for audit and support. It is **never used for authorization**.
- Today the API records only the self-reporting `X-Bilibi-Agent-Id` and `X-Bilibi-Run-Id` labels (`apps/web/README.md`). Recording the harness/model-version-effort form is a follow-up code change.

### 3. Plugins are thin adapters

- A harness plugin teaches only connection, discovery and safe execution. Task planning belongs to the agent.
- There is one plugin per harness: ChatGPT first, others when needed.
- Each plugin is a thin adapter over the shared base of MCP, API and CLI. It holds no business logic and no operation list.

### 4. Step-up is a policy, not a feature of one page

- Any contract may require step-up (2FA) by naming a policy key.
- Policies are discovered from the contracts, so a new operation needs no central change.
- Each tenant sets its policy per key.
- A successful step-up opens a **30-minute window, renewed on use, capped at 6 hours**.
- The same rule applies to a human and to an agent acting for a human.

### 5. Prompt injection is deferred

Protection against hostile content in documents an agent reads is **deliberately deferred to the agentic design**. It stays an open item. The operating rule (a human approves every agent write) is not a Bilibi control, and no control here is claimed to prevent prompt injection. Results carry an untrusted-content notice, grants are revocable and every call is logged, which help detection and cleanup only.

## Status of each door today

Verified against the code on 2026-10-17.

| Door | State | Evidence |
| --- | --- | --- |
| GUI | Built. Pages call module facades and check capabilities themselves. They do not yet call contracts | `apps/web/README.md`, each module's `web.rs` |
| API `/api/v1` | Built. Search, describe, guide, call, plans, commit. OAuth device grant, per-person grants, live proof on every request | `apps/web/src/api.rs`, `apps/web/README.md` |
| MCP `/mcp` | Built. Stateless Streamable HTTP. Six tools: `search`, `describe`, `guide`, `read`, `plan`, `commit` | `apps/web/src/mcp.rs`, `apps/base/agent_api/src/mcp.rs` |
| CLI | **Not built.** `bilibi-web` has only server commands (`migrate`, `seed`, `seed-production`) | `apps/web/src/main.rs` |
| Harness plugins | **Not built.** No plugin or adapter exists in this repository | |

The registry is `base/agent_api` (`apps/base/agent_api/README.md`). It is the partial form of this decision:

- It is **agent-facing only**. Pages do not use it.
- Only three Core modules contribute operations: `core/company` (companies, relationship roles and relationships), `core/person` and `core/affiliation`.
- No Domain or Extension contributes yet. None is mounted in this repository.
- Discovery refuses authority-administration capabilities for agents (role, grant and field administration, impersonation, settings, agent-connection management). `apps/base/agent_api/src/reach.rs` and `apps/core/agent_access/src/lib.rs` hold these refusals. This contradicts the decision above, except for impersonation, which stays refused for agents.
- Writes use the plan service: preview, then a single-use code. Idempotency today is that code plus version fencing in each owner. There is no caller-supplied idempotency key.
- Step-up is not generic. Two password confirmations exist, each hard-wired: the database tables page (`apps/base/system/src/database_tables/gate.rs`) and secret reveal (`apps/base/settings_editor/src/reveal.rs`, which also checks permission, throttles attempts and audits each one before a value is returned). No 2FA exists and no operation can declare a policy.
- There are no per-channel declarations. Every registered operation is reachable by agents.

## Consequences

**Follow-up Platform slice** (this record sets the direction and does not change code):

- The registry becomes channel-agnostic. A contract may declare the channels allowed to call it for technical reasons. There is no general human-only option. Sensitive actions are expressed through authorization and step-up. Impersonation is the one named exception to agent/person parity (section 2).
- GUI write events invoke contracts. Pages keep rendering and live patching.
- A platform check (boot or CI) refuses a page write event that names no registered contract. This replaces any separate parity rule.
- Step-up declarations become a property of the contract. 2FA, the tenant policy and the window rules in section 4 are built on that. The two existing password confirmations (database tables, secret reveal) move onto the same mechanism.
- The CLI client is built over the same contracts, as a caller like `/api/v1`.
- Design work, because lifting the refusals is not enough. **Platform-owner standing for agents:** `prove_owner` rejects agent scopes today (`apps/base/tenancy/src/lib.rs`), so an agent cannot reach platform-owner actions or owner-only pages even when its person is the owner. Define how an agent may hold the owner's standing, and any agent path to owner-only pages, with actor attribution and live revocation preserved. The existing scope fields alone do not provide this.
- `base/agent_api` stops refusing authority-administration capabilities for agents, as part of this slice. Agent reach becomes the person's live capabilities. Step-up policies on those contracts must land first or together, so the refusal is not lifted before its replacement exists.

**Open items:**

| Item | State |
| --- | --- |
| Page actions without a contract | Many pages write through facades. They are migrated module by module. The platform check turns each gap into a visible failure |
| CLI client | Not built. Needs a design for login (device grant) and output for agents |
| Prompt injection | Deferred to the agentic design |
| Cost per document | Unmeasured. An agent that reads, searches and drafts per document spends model tokens on every call. The harness owner bears this. Bilibi can keep it down with small results, paging and clear guides. Measure it on a real document flow before promising anything |
| Harness plugins | ChatGPT first. Not started |

**Risk this decision accepts:**

- Lifting the refusal widens what a stolen or misused agent grant can do. A grant could change roles or grant `core_admin` if its person may.
- Two controls contain this: step-up policy on sensitive contracts, and grants that are per person, time-limited and revocable at any moment. Revocation is proved live on every request.
- Prompt injection stays an open item, deferred to the agentic design (section 5). The wider reach raises the stakes of that gap. No control here mitigates it, and the human-approval rule is not enforced by Bilibi.

**Costs of the decision:**

- Every business action needs a contract. A module that skips it works in the GUI only, and the platform check will say so.
- Contracts constrain page design: a page cannot do something an agent cannot.
