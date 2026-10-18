# Tenancy identity boundary

Use `Authentication` and `Scope` from `src/lib.rs` for authenticated identity. Raw route IDs and K0 party IDs carry no authority. Scope and Actor have private identity fields, no deserialize implementation and no public constructor; the display fields are never policy input. A proof applies to one freshly guarded unit of work, not a permanent credential.

`base/tenancy.authentication` owns a once-bound `ActorVerifier`. Core User installs the account adapter during registry boot; missing and duplicate bindings fail startup. The `ProofClient` read adapter keeps owner checks on the caller's connection or transaction. Base has no SQL against Core account/company/person tables. `Authentication::authenticate` is an authentication-edge API: supply only identities extracted from validated durable credentials, never request IDs. It establishes identity and grants no capabilities. `Scope::mutation_context` supplies F2 attribution from that identity.

`base/tenancy.parties` owns optional `OrganisationProvider` and `PersonProvider` bindings. K1/K7/K9 install owner adapters using K0's `bilibi-party-ids`. Missing providers refuse all target proof requests. Providers must prove live, eligible targets in the exact scope, including tenant and access checks; IDs and business relationship roles never grant authority. An organisation/person proof retains its scope and typed target ID. The account adapter (`bilibi_user::identity`) proves each stored Person/affiliation/company link through its owner on every authentication, so an unlinked account carries no facet and a frozen one authenticates nowhere. This boundary neither fabricates Person data nor depends on People.

`prove_operator` returns a sealed proof only when the live scope tenant is explicitly recorded in the singleton `platform_operator` table. Numeric tenant IDs and `grant_all` have no operator meaning. Migration `005-tenancy-operator` leaves the marker unset; `bilibi-web platform-provision` and the development `seed` set it through `admin::provision_platform_operator`. Operator proof establishes identity, not a permission grant: use Authz `authorize_operator` for an operation that requires both. The migration does not replay the historical tenancy schema in `001-base`.

`prove_owner` is the installation owner: the account Core User's bootstrap receipt (`user_bootstrap`) names, sealed by the verifier on every authentication (`AccountIdentity::installation_owner`), with the operator proof added and refused under impersonation or an agent grant. Routes gate on it with `Access::Owner` and menu items with `Requirement::Owner`; no role, capability or grant confers it.

Named system principals and delegated job rehydration require the corresponding P6/Q2 owner adapters; an unverified worker or system identity cannot construct an authenticated Scope through this API.

Impersonation (K14): `authenticate_session` takes the operator the durable session row names (`sessions.impersonator_id`) and re-proves that account live in the same tenant beside the actor; `Scope::impersonator()` then carries it, and `mutation_context` and Audit's `Attribution::from_scope` record the actor/impersonator pair. To refresh a scope an edge already sealed, call `Authentication::reprove(client, scope)`, not `authenticate(tenant, user, csrf)`: the latter drops the operator (and the agent facet), so the owner's writes would lose half the audit pair. Self-service writes of the account's own preferences, pins, credentials re-proof and agent connections refuse a scope with an impersonator.

Agent self-report (ADR 01): `SelfReport` is an agent's `harness/model-version-effort`, shape-checked by `SelfReport::parse` and sealed on the agent facet by `Scope::for_agent`. `mutation_context` copies it to `Context::agent_self_report` and Audit's `Attribution::from_scope` records it. It is evidence shown under `SelfReport::LABEL`, never authority.

## Tenant administration and `/tenancy/tenants`

`admin::Tenants` (`base/tenancy.tenants`, `src/admin.rs`) is the tenant
administration service: `list`, `count`, `fetch` and the audited `create`
(mutation `tenancy.tenant.create`, change kind `tenancy.tenant`). Every method
takes an explicit `OperatorProof`; a tenant id (tenant 1 included) or `grant_all` is
never that proof, and `create` re-proves the identity on its own transaction.
Callers receive `TenantIdentity` (Bilimbi `Tenancy.Identity`), never the row;
its `is_platform_operator` is the `platform_operator` marker read with the row.
A refused create is `CreateOutcome::Refused` with the source's field errors
(blank or over-long name, unknown status, missing parent), never an `Error`.
Migration `009-tenancy-hierarchy` adds the source's `parent_id` and timestamps;
the operator marker stays the explicit singleton of `005`.

The capability keys are `admin::{LIST, CREATE}` (`admin.tenancy.tenant.list`,
`admin.tenancy.tenant.create`), both platform capabilities. Authz declares them
on Tenancy's behalf, because the vocabulary type is Authz's and Tenancy sits
below it. The route `/tenancy/tenants` (`src/web.rs`, ported from Bilimbi
`TenantsLive`) is `OperatorCapability(LIST)`, so the host proves identity and
capability on every request and live event; the page decides `CREATE` before
showing the Add Tenant dialog and again before writing. That decision goes
through `Authorization` (`base/tenancy.authorization`), a once-bound
`OperatorAuthorizer` that Authz installs at boot with its `authorize_operator`
(both requirements, independently logged); missing or duplicate bindings fail
startup. A module below Authz that needs a capability decision uses this seam
rather than importing Authz or re-deriving operator standing from an id.

To try it, install the walkthrough fixture on a migrated disposable database
(`cargo run -p bilibi-walkthrough-fixture`; it appoints tenant 1, while
migrations appoint nobody and `bilibi-web seed` provisions a real operator),
then sign in as `admin@example.test` (grant-all, tenant 1) and open
Administration → Tenancy → Tenants. Without the marker the same account is
refused (403, and the menu branch stays hidden); with it, an account that holds
`LIST` but not `CREATE` sees the list without the Add button and is refused the
create with a flash. Deleting the marker row restores the non-operator state.

