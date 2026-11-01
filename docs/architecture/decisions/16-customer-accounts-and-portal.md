# ADR 16: Customer accounts are a kind of account, kept out of the staff application by three barriers

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How a tenant's customers sign in to a self-service portal (`users.kind`, `PortalScope`, the `/portal` route space, capability audiences)
**Last Updated:** 2026-10-29

> Source: [issue 137](https://github.com/BelimbingApp/bilibi/issues/137).

## Context

**A tenant wants its customers to sign in and see only what belongs to them.** Until now every account was a staff account. A customer could only get an ordinary account with no roles, and that account still opened every `SignedIn` page and embed (dashboard, workspace, notifications, settings, OAuth consent, AI connections) and could connect an agent. A CRM Domain (cases, help articles) and a Finance invoice portal both need a shared answer; without one each Domain would invent its own login.

There were three choices to make. Customer accounts could live in a second account store or in the existing `users` table. A customer's email could be unique across the installation, as staff email is, or unique only within its tenant. The customer could be kept off staff pages by a single check or by several independent ones.

## Decision

**A customer account is a row in `users` with `kind = 'customer'`. It signs in to its tenant's portal at `/portal/...` and is kept out of the staff application by three independent barriers: the first two each stop it on every staff path, and the third on every path that asks for a capability.**

- **One account store with a kind.** `users.kind` is `staff` (every existing row) or `customer`. A customer always names a Person and names an affiliation, and so a company, only for a business customer; a private individual has neither. Core User stays the only writer of `users` and credentials, and invitations and registration live in `core/customer_access`, which calls Core User's account commands.
- **Customer email is unique per tenant; the portal address names the tenant.** Staff email stays unique across the installation, because staff sign in by email alone. Customer email is unique among one tenant's customers. The portal sign-in therefore names its tenant through a site address (`/portal/sign-in/{site}`), which also brands the anonymous pages and lets one person be a customer of two tenants without either learning of the other. Signed-in portal pages need no address: the session names the tenant.
- **Three barriers.**
  1. *Cookie path.* The portal session lives in its own cookie, `bilibi_portal`, with `Path=/portal`. The browser never sends it to a staff path, so there a customer is anonymous.
  2. *Host kind check.* The host reads only `bilibi_portal` under `/portal` and only `bilibi_session` elsewhere. It refuses a token whose account kind does not match its cookie, and `/login` refuses a customer's credentials as it refuses a wrong password.
  3. *Authz audience.* Every capability has an audience, tenant (staff), platform or portal. `decide` denies a staff capability to a customer and a portal capability to staff, with reason `denied_audience`, whatever the roles, direct grants or grant-all say; grant-all never satisfies a portal capability. Boot refuses a role that mixes audiences, and a portal role is never grant-all. A `SignedIn` page asks for no capability, so this barrier alone does not stop a customer there; the first two do.
- **Domains receive a sealed `PortalScope`.** The host builds it from a portal session only. `person()` is always present; `organisation()` and `affiliation()` are `None` for a private individual. "My records" filters take their ids from it, never from the request. Seeing a whole company's records is a portal capability, never a parameter.
- **The portal is its own route space.** Portal routes, downloads and embeds use `Access::Portal(key)` and live under `/portal/`; no other access lives there, and the host reserves its credential paths. It has its own menu root and layout, with no staff search, pins, tiles or bell.
- **No agents for customers.** Core Agent Access refuses to issue or prove a grant for a customer account, and customers never reach `/api/v1` or `/mcp`.

We rejected a separate customer account store because every credential rule (hashing and its timing guard, reset tokens, verification, the second factor, session revocation, standing, credential audit) and every key on `users.id` (sessions, step-up, notifications, audit) would be copied, and every later fix made twice. We rejected installation-wide customer email because one tenant would learn that an address is another tenant's customer, and because a consumer may deal with several tenants. We rejected a single barrier because any one of them can be broken by a mistake elsewhere (a cookie scoped too wide, a route declared with the wrong access, a role granted by hand). Each barrier looks redundant on its own; do not remove one as duplication.

## Consequences

- Every staff read of accounts filters `kind = 'staff'`: sign-in, password reset, user lists, the `users` Grid table, the Principal Directory, company and person account panels, landing pages and impersonation. A new staff read must do the same.
- A Domain that adds portal pages declares portal capabilities, contributes `Access::Portal` routes and takes `&PortalScope` in its portal facade. Code must handle `organisation() == None`.
- A customer stays signed in only while its Person is live and, for a business customer, while its affiliation is current by its dates and its company is not archived; standing is proved on every request.
- Single sign-on for customers, staff viewing the portal as a customer, and agent access for customers are out of scope.

## Delivery

The decision is built in parts under issue 137; until all have landed, only what a part names is in force.

| Part | Brings |
| --- | --- |
| 1 | `users.kind`, the staff-only reads, `portal_login`, `PortalScope`, customer standing, the audience of a pending sign-in, and the fail-closed staff edge: Session resolves staff sessions only, and Grants, `prove_operator` and `prove_owner` refuse a customer. |
| 2 | Capability audiences in Authz (`denied_audience`, the boot refusals of mixed-audience and grant-all portal roles) and Core Agent Access refusing customers. |
| 3 | The `/portal` route space, the portal cookie and the host's kind check per cookie. |
| 4 | The portal layout and menu root; the portal live socket in its own pull request. |
| 5 | Core Customer Access: sites, invitations, the staff screen and the portal credential pages (sign-in, forgotten password, reset, invitation), the portal home and account, and `PortalScope`'s narrow entry points. |
| 6 | Staff control of the portal: turning a site on and off (off by default; while off its pages answer 404), the customer accounts list with revoking and restoring access, resending an invitation, and signing a customer in on accepting one. |
| 7 | Self-registration: the site's request setting, the register and confirmation pages, and staff approval (which sends an invitation) or rejection. |
| 8 | Audit labels for customers (delivered), the portal-access panel on a contact's record (delivered), then the portal logo, a portal second sign-in step and mail delivery reports. |
