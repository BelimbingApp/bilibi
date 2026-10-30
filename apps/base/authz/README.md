# Authorization owner contract

The field restriction catalog combines owner contributions for the same table. Sources must agree on the table label and audit record types, and field IDs must be unique across the composition. This lets a record owner expose restrictions for an Extension-owned field without Authz depending on that Extension.

Authz owns capability vocabulary, decisions, grants and field withholding.
Party relationship roles are unrelated to authorization roles. Base never queries
Core account/company tables: Tenancy's account verifier supplies live account
eligibility at the authenticated edge and for grant targets.

Register a `VocabularyContribution` through the registry's `VOCABULARY` consumer.
Contribute domains, verbs, capability keys (each with an audience: `CapabilityDefinition::tenant`,
`platform` or `portal`), role definitions/extensions, and named system-principal capability bounds.
`Vocabulary::from_catalog` validates the complete vocabulary. Installed Authz also
validates route, embed, menu and settings capability references during boot.
Duplicate capability/principal keys, conflicting role definitions, malformed keys,
and undeclared domain/verb/grant references refuse installation. Capability entries
carry contributor provenance. Two-part keys (`domain.action`) remain valid; new
owners should use the source's `domain.resource.action` form.

To extend a contributed system role, contribute the same code, canonical name
and grant-all flag, with additional capability keys. Capabilities are unioned
across owners; Authz's built-in definitions retain their canonical metadata
regardless of the extensions' catalog order.
Deploying a contribution does not change stored role grants automatically:

```sh
cargo run -p bilibi-web -- authz-reconcile
```

Reconciliation serializes runs, adopts an existing same-named role in place, and
synchronizes contributed system-role metadata/capabilities. It preserves role IDs,
assignments, custom roles, direct grants/denies and field restrictions. It removes
obsolete grants from contributed roles, and clears redundant grants on grant-all
roles. Run it again after installing contributions or provisioning new tenants.
`seed-production` also supplies the initial versioned reconciliation through the
Database seed ledger. `Grants::grant_first_administrator_in` is the one
bootstrap seam: on a system-attributed `MutationTransaction` of a tenant it
reconciles that tenant's system roles and assigns the grant-all `core_admin`
role as an audited change, for `bilibi-web create-owner` (Core User's
`create_first_administrator`, which proves the tenant holds no account). It
refuses a user- or agent-attributed transaction; every other assignment goes
through `assign_role`, which proves the actor and asks for step-up. Neither path runs at app boot. The schema keeps one role
instance per tenant; a custom role is owned by a company of that tenant
(`roles.company_id`, migration `011-role-company`, K15). Base holds the bare
column and Core Company contributes the scoped foreign key; `create_role`
requires a company and reproves it through Tenancy's `PartyProofs` on the
write's transaction (missing, cross-tenant or archived are field errors), the
lists apply Bilimbi's company-visibility rule with the ids Tenancy's
organisation directory names, and a custom role created before ownership was
bound stays listed as "Unowned". Base never queries Core tables for this.

Tenancy sits below Authz, so Authz declares Tenancy's `admin.tenancy.tenant.*`
keys (`bilibi_tenancy::{LIST, CREATE}`) and binds `OperatorAuthorizer`
into `base/tenancy.authorization` at boot (`base/authz.operator`), the one
`authorize_operator` decision Tenancy's tenant create (and Core Tenant
Administration's list) make. A new module below
Authz that needs a decision extends that seam; it does not import Authz.

Use `authorize_key` (or the compatibility `Capability` enum) for enforcement and
`decide` for a reasoned decision. `authorize_any(client, scope, keys)` is the same
enforcement for a combined page (`Access::AnyOf`): it opens for any one live key and
writes no denial row for the others while one opens it. Unknown keys deny even grant-all holders.
A contributed platform capability requires the persistent `platform_operator`
identity **and** a grant; tenant 1 and grant-all have no implicit operator power.
**Audiences** ([ADR 16](../../../docs/architecture/decisions/16-customer-accounts-and-portal.md)). A customer account holds only portal capabilities and staff never hold one: `decide`, `decide_proof`, `holds_proof` and the effective sets deny a capability outside the actor's audience (`Actor::kind()`) with reason `denied_audience`, before any direct grant, role grant or grant-all. Grant-all never satisfies a portal capability: a customer needs a direct allow or a role that lists the key. `Grants` administer staff only: it refuses a customer account, a portal key as a direct allow or on a custom role, and assigning a role that lists a portal key. Boot refuses a contributed role that mixes portal and staff keys, a grant-all role with portal keys, and a system principal that declares a portal key. `holders` lists tenant capabilities only; its caller keeps only staff accounts. `effective_capabilities_of` reads another account as staff, since only staff administration screens read another account.
Direct deny wins over direct allow, role grants and grant-all. Permission reads
are fresh. Reauthenticate/revalidate a retained Scope at every request/event/job
edge; the sealed scope is identity for one unit of work, not a permanent login.
Decisions record reason and principal kind using the caller's connection; decisions
inside an owner transaction follow that transaction's commit/rollback.

`base/authz.grants` exposes audited custom-role creation, role assignments, custom-role capability
changes, direct overrides, and system grants. It checks the administrator and
principal through the live owner verifier. System roles are immutable through
interactive role-grant writes; reconciliation is their explicit update path.
Capture failure or cancellation prevents commit. No browser-provided actor ID is
accepted as attribution. Callers cannot pass a SystemScope to grant administration.

Named system principals require an installed declaration and a durable grant in the execution tenant, or a standing grant (below). `SystemScope::rehydrate` is a trusted worker/maintenance edge, never a browser constructor. `SystemAuthentication::for_system(client, &SystemScope)` (SP1) seals a declared principal into a Tenancy `Scope` (`Authentication::for_principal`), so owner facades that take `&Scope` serve a worker: `decide`, `decide_proof`, `authorize_key`, `authorize_any`, `effective_capabilities` and `explicitly_granted` on that scope apply the named-principal rule (live tenant, declaration, platform gate, durable `authz_system_grants` row or standing grant, deny wins), log `system` with the principal, and never consult roles or grant-all. `effective_system` lists what a principal holds. `Grants` refuse a principal scope as the administrator. Tenancy's `Authorization::authorize_scope` is bound here (`authorize_proof`) for modules below Authz. Anonymous system identities always deny; declaration alone grants nothing. Removed declarations make old grants inert. Named identities hold no user roles, gain no grant-all bypass, and obey platform gates. Decisions stay tenant-scoped; company ownership of custom roles (K15) and the account binder (K9) use Tenancy owner proofs. An owner adapter that holds only Tenancy's sealed proof connection (`OrganisationDirectory`, Media `SubjectOwner`, Artifacts `Owner`) decides through `decide_proof` / `authorize_proof`: the same rules and decision log as `decide`, on that connection. `holds_proof` reads the same decision without logging it, for display and for reach checks that run on every page or authentication (the company switcher's listing and a chosen current company's reach); an operation still decides with `decide_proof`. Queue execution must rehydrate at execution, rather than retaining a Scope captured when enqueued.

**Standing grants** (ADR 12) are for Platform work no tenant should have to switch on, such as Media's malware scans. The module that declares a principal may contribute `StandingGrant { principal, capabilities }` to `STANDING_GRANTS` under the principal's name; the principal then holds those tenant capabilities in every live tenant with no `authz_system_grants` row (decision reason `system_principal_standing`). A tenant's denying grant (`Grants::set_system(.., Some(false))`) still wins. Boot refuses a grant from another module, for a capability outside the declaration, for a platform capability, or a second grant for one principal. `effective_system` includes standing capabilities.

`holders(client, tenant, key, limit)` lists the accounts of a tenant that hold a tenant capability (direct allow, grant-all or role grant, no direct deny), in id order, for an owner that must tell them about work only they may do (Media's quarantine notices). It logs nothing; prove each account live before writing to it.

`/authz/field-restrictions` (Administration › Authorization › Field Restrictions,
`src/restrictions_web.rs`, Bilimbi `FieldRestrictionsLive`) is the operator's
screen for `base/authz.field_restrictions`: one row per restricted field, a
dialog that asks for the roles, then the tables, then the fields as chips and
commits the fields picked together as one transaction, a smaller dialog per row
to change its roles or remove it, and the shared confirmation before removal.
The route and every live event require `admin.authz.field.manage`; the service
re-checks it inside each write, so a grant revoked while the page is open is
refused and the host clears the page. Rows whose table the installed catalog no
longer offers (a restriction on a table its module stopped contributing) stay
listed by id, can be removed and cannot be re-restricted.

`restrictions_for` and `fields::restricted` read restrictions for any table ID;
any held restricting role masks, even alongside an admin. `fields::mask` removes
restricted fields before JSON serialization, including arrays, audit old/new
objects and headless projections. `fields::refuse_attempts` refuses any submitted
restricted key regardless of its value. Owner-typed read models must also clear
those values before rendering (Companies already does). A named system
principal holds no roles, so it fails closed: every restriction in its tenant
applies to it, and it never sees or writes a role-restricted field. P8 owns the
protected-field catalog contract and restriction administration; Grid supplies
that catalog as data, with no Authz→Grid dependency.

A `CatalogField` may declare alternate restriction identities for the same
record value. Each alias names its target table, field and that table's audit
record types. `withheld_by_record_type` maps a restriction on either identity
to both field names under both owners' audit record types, including a
protected target field that cannot itself receive a new restriction. The
record owner still resolves those aliases for its own reads and writes; Authz
does not depend on Grid or the record owner.

## Step-up on grant changes

Authz is a Base Step-up consumer (`src/step_up.rs`, `apps/base/step_up/README.md`). These operations are declared on by default, and each tenant may change them:

| Operation | `Grants` method |
|---|---|
| `base.authz.principal_role.set` | `assign_role`, give or remove |
| `base.authz.grant_all_role.assign` | `assign_role` giving a full-access role such as `core_admin`, in addition to the above |
| `base.authz.principal_capability.set` | `set_direct` |
| `base.authz.role_capability.set` | `set_role_capability` |
| `base.authz.system_principal.set` | `set_system` |

- Each method calls `StepUp::require` after its capability check and before the write. The target names the user, role or capability.
- For the full-access role, the doc suggests "ask every time".
- `/authz/step-up` (Administration › Authorization › Step-up 2FA, `src/step_up_web.rs`) is the policy page for every discovered operation. It needs `admin.authz.step-up.manage` (contributed to `tenant_owner`).
  - Two tabs, kept in the address as `tab`: **Settings** (the default) and **Actions**.
  - Settings: the tenant's window ("Minutes before asking again") and cap ("Hours before always asking again") as inline facts that save when committed; for the installation owner, the installation maxima ("Longest time any organisation may choose").
  - Actions: one FlexTable row per declared operation, with module, label, default, this tenant's state and the floor lock. Confirmed changes: off, on, ask every time, back to default; for the installation owner, the floor.
  - Every change rechecks the capability in its audited transaction and first passes step-up itself (`base.step_up.policy.update`).
- `examples/p6-authz.rs` changes grants from a console. Run it only on a disposable database whose saved instance role is Dev; `BILIBI_ENV=development` supplies that role during its first install seed.
- Agents reach the same writes: `base/authz_agent` adds reviewed-plan agent operations over the `*_in` variants (`assign_role_in`, `set_direct_in`, `set_role_capability_in`), which run inside the plan's transaction. See `apps/base/authz_agent/README.md`.
- The account page (`core/user`) passes `Error::StepUpRequired` up, so the host sends the browser to confirm and back.

## Running evidence

No unit tests. Use a private migrated database and run:

```sh
BILIBI_AUTHZ_DEMO=disposable cargo run -p bilibi-web --example p6-authz
```

This explicit runtime walkthrough prints role/direct grant decisions, live
revocation, extension of the fixture manager role, unknown-key denial, admin-plus-viewer masking
on Companies and a second table, restricted-null refusal, operator/grant
independence, named/anonymous/cross-tenant system decisions and an audit-failure
rollback. It uses the walkthrough fixture, whose tenant 1 is the operator, and installs an audit
refusal trigger, then removes both; never run against a real database. Decision
and audit rows remain as evidence. It requires the real installed owner services.
Compile with the workspace discovery/fmt/clippy checks; do not add a test runner.

## External identity roles

Keep provider-managed roles separate from administrator assignments.

`Grants::configure_identity_roles_in` authorizes and audits a company-scoped source's complete role bound, using ordinary role-update capability and Step-up checks. `apply_identity_roles_in` can then apply only roles in that stored bound for an account proved to belong to the policy's company. Both use the caller's `MutationTransaction`; the identity owner supplies verified claims or provisioned memberships and calls the latter after proving the account live. Narrowing a bound removes obsolete grants immediately.

Automatic memberships live in `authz_identity_roles`, while manual memberships remain in `principal_roles`. `effective_principal_roles` combines them for decisions, masking, role displays, agent reach and shared-layout eligibility. Use the Authz read APIs or that view for effective memberships. Never write the view or delete manual assignments to reflect a provider group change. Direct denies retain precedence over both kinds of role grant.

`Grants::revoke_identity_source_roles_in` removes only one authenticated system identity source's automatic grants, even when the target account is frozen. Its caller proves the source credential; the system attribution must name that same source. It cannot add grants or touch manual roles.

## Restrictions on a caller's read connection

`fields::restricted_read(client, scope)` reads the same live field restrictions as `fields::restricted`, using `&dyn bilibi_tenancy::ReadClient`. Use it when a reviewed preview or another owner's transaction supplies that connection. Any restricting role wins, including alongside a grant-all role; named principals receive every restriction in their tenant. The function acquires no pooled connection.

## Approval eligibility reads

`has_effective_role` checks named-role membership in the scoped tenant, including provider-managed memberships. A grant-all role never substitutes for membership in another named role. `holder_page` enumerates a bounded capability-holder page after an account ID, with direct denies applied; callers must still prove account eligibility, company reach and owner visibility. `role_page` returns bounded role labels for an owner-authorized policy editor and grants no administration permission.

The `effective` implementation module is private. Import its public types and reads from the `bilibi_authz` crate root; no authorization rule changes with that path correction.
