# ADR 17: One global schedule, fanned out under per-tenant consent

**Document Type:** Architecture Decision Record
**Status:** Accepted
**Agents:** codex/gpt-6, claude-code/sonnet-5.5-high
**Scope:** How `base/schedule` dispatches one recurring definition to tenant-local Queue jobs
**Last Updated:** 2026-11-01

> Source: [issue 138](https://github.com/BelimbingProject/bilibi/issues/138). The contract is [`apps/base/schedule/README.md`](../../../apps/base/schedule/README.md); Queue's is [`apps/base/queue/README.md`](../../../apps/base/queue/README.md).

## Context

`base/schedule` stores one installation-global definition and claims each occurrence in the platform operator's tenant. A Domain cannot run recurring work per tenant itself: listing tenants needs an `OperatorProof`, which a scheduled job does not have, and direct SQL on Tenancy's tables breaks ownership. Report schedules, retention sweeps and Domain housekeeping (SLA scans, reminders, document expiry) each need the same recurrence to run once in every tenant, as that tenant, so one slow or failing tenant never holds up another.

## Decision

1. A definition declares `Reach::Installation` (the default behaviour) or `Reach::EachTenant`. Reach is part of the fingerprint. Review, enable, disable, pause and resume stay global: the code decides what runs and when; each tenant decides only whether it may run there.
2. The scheduler claims an `EachTenant` occurrence once, in the operator tenant. Its job is the Platform worker `base/schedule-fan-out`, which queues the owner's worker once per tenant. The owner never enumerates tenants and never writes across tenants.
3. **Enumeration stays in Tenancy.** `Tenants::principal_page(scope, after_id, through_id, limit)` serves a worker running as a named system principal in the operator tenant: it checks the operator marker and decides the platform capability `admin.tenancy.tenant.list` on the principal's own sealed scope (declaration, durable grant, logged), because a system principal cannot hold an `OperatorProof`. Pages hold at most 200 tenants in id order, exclude deleted ones, and walk a fixed high-water mark.
4. **Consent is a per-tenant grant, not provisioning.** In a customer tenant the `scheduler` principal needs `admin.schedule.tenant-work.execute` (`TENANT_WORK`), granted by that tenant's administrator through the shared Automation access page or agent operation. The existing `EXECUTE` stays platform-only. The operator tenant is a target too and uses `EXECUTE`. A denying grant wins; an ungranted active tenant is recorded `not_granted` and nothing is queued.
5. **Queue decides exactly one capability per attempt.** A worker may contribute a `CapabilityPolicy` (`base/queue.capability-policies`, entry id = worker id) naming the key for the principal and for a person in any tenant other than the operator's. Queue picks the key from the job's actor kind and the live operator classification, never from arguments, never tries an alternative, and rechecks the same key inside `Execution::begin`. Without a policy the binding's `capability` decides everywhere, so no existing worker changes. `bilibi_schedule::TENANT_POLICY` is the policy every each-tenant worker contributes; boot refuses a definition without it.
6. **Per-tenant rows, written with the job.** `base_schedule_tenant_runs` has one row per `(occurrence, tenant)` (unique). A queued row and its job are written in one audited transaction in that tenant, as that tenant's `scheduler` principal. Rows without a job (`skipped` for a suspended tenant or overlap, `not_granted`) record why. The first fan-out attempt stores the tenant-id high-water mark (`fan_out_through`), so a retry walks the same cohort and a tenant created later joins the next occurrence. A retried or resumed fan-out inserts only missing rows and never queues a second job for a tenant; Queue stays at least once, so owners keep effects idempotent.
7. **Overlap is per tenant.** Under `Overlap::Forbid` a partial unique key on `(definition, tenant)` over unfinished runs is the lease; a tenant whose previous run is still going is `skipped` while the others proceed. The occurrence itself holds no lease.
8. **Start check.** Before a tenant's first attempt runs owner code, `Schedule::run` takes the per-key lock and checks the current fingerprint, enabled and unpaused state; if a change committed first, the job is cancelled (`schedule_disabled`, …). The run's `started_at` is the durable start marker: a retry of a started run continues after a later disable, while Queue still rechecks tenant, declaration and grant on every attempt (`actor_refused` after revocation or suspension). Settling reads Queue's state and maps terminal jobs to a bounded code; a missing job is `linked_job_missing` and is never replaced.
9. Missed occurrences still coalesce once at the claim; every tenant then gets that one occurrence, so owner work is level-triggered ("find what is due now"). Tenant-local wall-clock firing, per-tenant cron or pause, and per-company fan-out are out of scope.
10. Operator **Run now** fans out one manual occurrence, queued as the `scheduler` principal and audited as the operator. The one Schedule board reports per-tenant outcomes to the operator (counts per latest fan-out, a tenant filter on History), and a tenant administrator sees only their own tenant's definitions and runs on the same page, with a Run now that runs as that person; `Schedule::run_now_for_tenant` queues work as that person, works without the tenant's scheduler grant, and is refused in the operator tenant.

## Consequences

- One operator review governs when and what runs; each tenant separately decides whether the named principal may act there.
- Fan-out resumes after a crash without duplicates, and each tenant's work, audit attribution, history and failure stay tenant-local.
- Schedule and Domains gain no general tenant-listing authority and perform no cross-tenant SQL.
- `Definition` gained a required `reach` field: every in-repository caller was updated in the same change (no default, no alias).
- The operator needs two grants for the scheduler in the operator tenant (`EXECUTE` and `admin.tenancy.tenant.list`) before an each-tenant definition fans out.

## Alternatives rejected

- **One schedule entry per tenant:** multiplies cron definitions, reviews and fingerprints by the tenant count for no gain.
- **Owner module loops over tenants:** gives Domains cross-tenant enumeration and bypasses Queue's tenant scope and retry lifecycle.
- **One operator-tenant job writes into every tenant:** one audit context and one failure boundary for all tenants.
- **Standing grant for all tenants:** conflicts with per-tenant consent and silently authorizes work at provisioning.
- **A second operator-tenant ledger plus a tenant-local row, and a persisted capability key per job:** the first draft of this record. The single per-tenant row, the live operator classification and Queue's existing per-attempt rechecks give the same guarantees with fewer moving parts.
