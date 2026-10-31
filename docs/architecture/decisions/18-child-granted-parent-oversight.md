# ADR 18: Child-granted parent oversight is a read proof, not membership

**Document Type:** Architecture Decision Record
**Status:** Proposed; implementation in progress
**Agents:** amp/gpt-6.1-sol
**Scope:** Consent and isolated cross-tenant Grid reads
**Last Updated:** 2026-10-31

## Decision

A recorded parent has no authority over a child. Each child grants its recorded direct parent a versioned, expiring selection of Grid tables, read levels and fields. Granting or widening requires the installation-floor Step-up operation; narrowing and revocation do not require a factor.

Tenancy seals an `OversightScope` for a live, linked staff person in the parent, never a child `Scope`. Owner facades, record links and writes continue to require ordinary tenant membership. Operator standing, `grant_all`, system principals and hierarchy cannot substitute for consent.

Grid owns one bounded fan-out over its existing query engines. Every child read intersects live parent capabilities, restrictions and masks with that child's consent. Personal fields require explicit consent; sources or link edges needing a child user binding are excluded. Read-only transactions retain ordinary Grid cost admission and time bounds. Child failure is explicit and contributes nothing to totals; averages combine sums and counts, not averages.

Each successful child answer requires a durable action in that child, attributed to the parent actor's tenant, and a fresh consent/identity check. A failed child audit suppresses that answer. One parent action records fan-out outcomes. Audit captures field names and counts, never filter operands or returned values. The bounded in-memory cache is per reader, query, grant version and live catalog; hits still recheck authorization and write audit.

## Alternatives

- Retargeting a parent `Scope` would authorize owner writes and invent child membership, so it is forbidden.
- Per-module cross-tenant facades would duplicate isolation and admission policy. Modules instead contribute ordinary tenant-scoped Grid sources.
- A reporting warehouse would retain child data after revocation and add synchronization and erasure obligations. Oversight reads the child's source and persists no results in the parent.

## Contracts

The authoritative APIs are in [Tenancy](../../../apps/base/tenancy/README.md), [Grid](../../../apps/base/grid/README.md) and [Tenant Administration](../../../apps/core/tenant_administration/README.md). Reports and exports must use the same sealed read path; they cannot infer child identity from numeric IDs.
