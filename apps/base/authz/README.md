# Authorization owner contract

Authz owns capability vocabulary, decisions, grants and field withholding.
Party relationship roles are unrelated to authorization roles. Base never queries
Core account/company tables: Tenancy's account verifier supplies live account
eligibility at the authenticated edge and for grant targets.

Register a `VocabularyContribution` through the registry's `VOCABULARY` consumer.
Contribute domains, verbs, capability keys (with an explicit `platform` flag),
role definitions/extensions, and named system-principal capability bounds.
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
Database seed ledger. Neither path runs at app boot. The schema keeps one role
instance per tenant; a custom role is owned by a company of that tenant
(`roles.company_id`, migration `011-role-company`, K15). Base holds the bare
column and Core Company contributes the scoped foreign key; `create_role`
requires a company and reproves it through Tenancy's `PartyProofs` on the
write's transaction (missing, cross-tenant or archived are field errors), the
lists apply Bilimbi's company-visibility rule with the ids Tenancy's
organisation directory names, and a custom role created before ownership was
bound stays listed as "Unowned". Base never queries Core tables for this.

Tenancy sits below Authz, so Authz declares Tenancy's `admin.tenancy.tenant.*`
keys (`bilibi_tenancy::admin::{LIST, CREATE}`) and binds `OperatorAuthorizer`
into `base/tenancy.authorization` at boot (`base/authz.operator`), the one
`authorize_operator` decision Tenancy's own page makes. A new module below
Authz that needs a decision extends that seam; it does not import Authz.

Use `authorize_key` (or the compatibility `Capability` enum) for enforcement and
`decide` for a reasoned decision. Unknown keys deny even grant-all holders.
A contributed platform capability requires the persistent `platform_operator`
identity **and** a grant; tenant 1 and grant-all have no implicit operator power.
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

Named system principals require an installed declaration and a durable grant in
the execution tenant. `SystemScope::rehydrate` is a trusted worker/maintenance
edge, never a browser constructor. Anonymous system identities always deny;
declaration alone grants nothing. Removed declarations make old grants inert.
Named identities hold no user roles, gain no grant-all bypass, and obey platform
gates. Decisions stay tenant-scoped; company ownership of custom roles (K15) and
the account binder (K9) use Tenancy owner proofs. An owner adapter that holds
only Tenancy's sealed proof connection (`OrganisationDirectory`, Media
`SubjectOwner`, Artifacts `Owner`) decides through `decide_proof` /
`authorize_proof`: the same rules and decision log as `decide`, on that
connection. Queue execution must rehydrate at execution, rather than retaining
a Scope captured when enqueued.

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
those values before rendering (Companies already does). Named system identities
hold no roles and therefore have no role-based field restrictions. P8 owns the
protected-field catalog contract and restriction administration; Grid supplies
that catalog as data, with no Authz→Grid dependency.

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
- `/authz/step-up` (Administration › Authorization › Two-factor Requirements, `src/step_up_web.rs`) is the policy page for every discovered operation. It needs `admin.authz.step-up.manage` (contributed to `tenant_owner`).
  - One FlexTable row per declared operation, with module, label, default, this tenant's state and the floor lock.
  - Confirmed changes: off, on, ask every time, back to default.
  - The tenant's window and cap.
  - For the installation owner: the floor and the installation maxima.
  - Every change rechecks the capability in its audited transaction and first passes step-up itself (`base.step_up.policy.update`).
- `examples/p6-authz.rs` changes grants from a console, so run it with `BILIBI_ENV=development`.

## Running evidence

No unit tests. Use a private migrated database and run:

```sh
BILIBI_ENV=development BILIBI_AUTHZ_DEMO=disposable cargo run -p bilibi-web --example p6-authz
```

This explicit runtime walkthrough prints role/direct grant decisions, live
revocation, extension of the fixture manager role, unknown-key denial, admin-plus-viewer masking
on Companies and a second table, restricted-null refusal, operator/grant
independence, named/anonymous/cross-tenant system decisions and an audit-failure
rollback. It uses the walkthrough fixture, whose tenant 1 is the operator, and installs an audit
refusal trigger, then removes both; never run against a real database. Decision
and audit rows remain as evidence. It requires the real installed owner services.
Compile with the workspace discovery/fmt/clippy checks; do not add a test runner.
