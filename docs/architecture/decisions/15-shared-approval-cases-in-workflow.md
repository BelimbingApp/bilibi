# ADR 15: Shared approval cases in Workflow

**Document Type:** Architecture Decision Record  
**Status:** Accepted; implementation in progress  
**Agents:** codex (author), claude-code/sonnet-5.5 (author)  
**Scope:** Approval policy, eligible decision makers, shared waiting list, exceptions and owner execution across mounted modules  
**Last Updated:** 2026-10-29

### Context

Company work repeatedly needs independent approval: manual journals, supplier documents, purchase orders, leave, payroll and personal-data erasure. Workflow already owns durable status changes, process runs, human actions and history, but owners repeat approval routing and independence rules. Finance additionally has a separation setting, exceptions register and independent reviews that must be preserved.

An approval is different from 2FA. 2FA proves the account holder is present; approval records another authorized person's decision. Accepted ADR 01 requires agents to act with their person's authority and separation constraints. Base must not depend on or query business Domains to interpret their records.

### Decision

Add first-class `ApprovalCase` contributions inside `base/workflow`, behind the `Engine` facade. Each owner declares its subject, decision capabilities, typed routing facts, minimum controls and effect adapter. Workflow owns versioned policies, approval requests and rounds, stages and decisions, delegation, reminders/escalation, one eligible-person inbox, and a generic exceptions register with independent reviews.

An approval policy is a bounded sequence of All/Any stages. Each requirement can be satisfied by one eligible account selected by capability, effective authorization role, named person or an owner-proved reporting relationship. Amount and type rules use owner-proved typed facts and exact money with explicit currency. Policies may strengthen but cannot weaken the declared minimum controls. Company policies are versioned operator data with an editor; publication affects new rounds, while live identity, capability and security floors are checked at every decision.

Retain Workflow's immutable process graphs. A contributed fixed wait process represents an approval round; approval stages are bounded business-policy data, not dynamically edited process graphs. Intermediate votes use the human-action identity/idempotency/version gate without completing the aggregate wait. Only a terminal, owner-applied approval state can complete it as approved. A signal, generic work completion or operator waiver is never approval proof. A surrounding process may receive a revision-bound signal after the protected decision succeeds.

Extend the gate with a private typed continuation so approval and owner methods share the current `MutationTransaction`. Owner locks, approval locks, run/work locks and requirement locks have one documented order. Every entry path dispatches through this coordinator. Owner effects, decisions, exceptions, work completion, audit and notification/queue intents commit or roll back together. An owner-effect failure leaves the final decision unaccepted. Callbacks open no inner transaction and perform no external I/O.

Owners retain business validation and execution. Ledger posts and reverses entries; Leave owns reservations and balance; Payroll owns frozen calculations and its final event; Payables owns allocation and match coverage; Privacy owns verified matching, holds, erasure work and completion evidence. The owner rechecks its constraints in the same approval transaction and, for deferred execution, in each executing transaction. Before final application, the coordinator rechecks all saved votes against the live hard security floors. Completed votes otherwise stand as decisions made at their recorded time; a later role removal does not rewrite history. A content change or explicit security invalidation supersedes the round.

Separation includes preparers, current beneficiary accounts where applicable, actors and impersonators on both sides. Agents count as the person they represent; system identities cannot approve. Valid one-hop delegation substitutes routing responsibility but grants no business permission and cannot bypass independence. A permitted Finance same-person approval records the separation-control change and reason. Every business-control override records an exception. Reviews require a person independent of all preparing and deciding identities, even when separation is disabled.

Rejection requires a reason. Recall is permitted only while the owner allows it, and atomically releases owner reservations. Resubmission creates a new linked round for a fresh revision and policy; it carries no old approval. Completed owner effects are corrected only through owner operations. Timed jobs can remind or reassign according to policy, never auto-approve.

Base names accounts through Principal Directory and uses owner adapters or documented Ports for optional reporting relationships. It does not read Core or Domain tables. All pages, counts and machine reads apply owner visibility and live eligibility. Cases and actions are contributed; the host gains no module or endpoint list. Every write remains reachable through the shared operation-contract model.

Move generic Finance separation/exception mechanics to Workflow through an explicit, evidenced cutover, retaining immutable legacy evidence and migrating every old helper caller. Other owners move in coordinated slices without duplicate decision authorities or compatibility shims. New Privacy erasure work uses this facility from its first implementation. Align ADR 05 and issue 132 with ADR 01: an independent person's authorized agent may approve with the required floor 2FA; an automated system may not.

### Alternatives considered

- **Keep approval in each owner and share only buttons:** rejected because routing, delegation, separation, reminders and exceptions would still diverge.
- **Build a separate `base/approvals` module on Workflow:** viable, but adds another facade and cross-module transaction adapters for the same human-action lifecycle. Start inside Workflow; extract only if an independent lifecycle warrants it.
- **Represent every company policy as a new Workflow process graph:** rejected for the first version because current graphs are contributed immutable code, while thresholds, people and escalation are operator data. A fixed wait plus bounded approval policy avoids a new runtime graph compiler.
- **Adopt BPMN or an external workflow service:** rejected because the surveyed cases fit a smaller in-process model and need atomic owner writes with existing database/audit boundaries.
- **Use Step-up or an agent harness confirmation as approval:** rejected because neither proves a second independent authorized person's decision in Bilibi.

### Consequences

Owners implement business meaning once and declare common approval rules. People gain one waiting list and consistent history, reasons, cover and escalation. Finance's exceptions and reviews become reusable controls.

Workflow becomes responsible for a durable approval-policy schema, a transaction continuation, security-sensitive eligibility queries and an operator editor. Case adapters and policy/process versions must remain available for outstanding work. Approval records are personal-data stores and participate in Privacy's retention and erasure contracts. Clean API changes require coordinated official-Domain upgrades and explicit agent upgrade notes.

The first version omits arbitrary scripting, quorum/weighted voting, looping processes, external reply approvals, cross-tenant decisions, multi-hop delegation and calendar-day rules. A later requirement needs its own justified extension; it must not bypass owner authorization or transactional execution.

### Validation and rollout

Prove the continuation and strict identities first with the disposable reference approval case. Manual journals are the first real consumer in the next coordinated slice. Migrate every Finance helper/preview/report caller in a coordinated cutover, then Leave and People Payroll. Keep Finance Payroll consuming documented People events. Demonstrate parallel/Any stages, amount/type rules, stale/replayed decisions, recall races, delegation expiry, reminders, independent exception review, cross-tenant secrecy and audit/owner-effect rollback. Use disposable walkthrough data and browser interaction at exact `APP_ORIGIN`; add no unit-test suite.

### Prior art

Bilimbi Workflow human actions and owner adapters; Bilimbi Finance duties and exception reviews; Bilimbi People Training's supervisor → HR → final approval chain; Bilibi ADR 01 and proposed ADR 05. Their contracts remain authoritative for their subjects; this ADR records the new shared architectural choice.
