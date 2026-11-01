# 21 One sign-in owns credentials over separate tenant memberships

**Document Type:** Architecture decision record
**Status:** Accepted; implementation in progress
**Agents:** codex/gpt-5
**Scope:** Staff identity across tenant boundaries
**Last Updated:** 2026-11-01

## Decision

**One platform sign-in owns credentials and may have one separate staff account in each tenant that invites the person.**

- `user_identities` is installation-owned, not tenant data. It holds the login address, password, verification, second-factor enrollment, home account and last tenant used.
- A tenant-owned `users` row is one membership and remains the actor for that tenant. Its roles, restrictions, Person link, desks, notifications and audit attribution never cross the tenant boundary.
- The home account's tenant administers the sign-in credentials. Another tenant may administer or remove only its own membership and must not learn the home tenant or any other membership.
- One browser session enters one membership at a time. Entering another membership replaces the session token, clears step-up and rotates CSRF; an impersonated session cannot enter another tenant.
- Customer accounts remain tenant-bound identities and never become cross-tenant memberships.

The credential foundation is implemented: all accounts point at a sign-in identity, and passwords, verification, reset tokens, sign-in throttles, pending sign-ins and authenticator enrollment belong to it. Invitation, membership administration, tenant entry, stale-tab handling, cross-tenant session revocation and operator identity audit search remain to be built before this record is implemented.

## Why

Tenant authority already assumes that the actor account belongs to the tenant in every scoped foreign key, authorization decision, directory lookup and audit row. Reusing one account across tenants would either break those invariants or make a tenant depend on a foreign account it cannot read.

Separate accounts keep today’s tenant boundary intact while the shared sign-in removes duplicate passwords and second factors for accountants, outsourced teams and people who work in separate country tenants.

## Rejected alternatives

- **Cross-tenant grants on one home account.** The foreign account would become an actor in a tenant that does not own it, breaking scoped references, directory naming and audit attribution.
- **One credential per tenant.** This preserves the current duplication and does not meet one-sign-in behaviour.
- **One tenant per browser tab.** This would put tenant identity into every route, live socket, download and background operation, and a record address from one tenant could be replayed in another.
- **Company desks as tenant membership.** A desk selects among one tenant's own companies under [ADR 19](./19-company-work-access-and-page-scope.md); it never crosses the tenant wall.

## Consequences

- Credential code resolves the identity from the account and never stores a password, verification timestamp or authenticator against the tenant account.
- An identity-wide credential change eventually revokes every membership's sessions; removing one non-home membership does not affect the sign-in or another tenant.
- Every tenant-facing page and error stays membership-local. Installation-wide identity correlation is reserved for the platform operator.
- Fresh databases use migration `core-user-028-sign-in-identities`. No account conversion, backfill or compatibility columns exist while development databases are disposable.

