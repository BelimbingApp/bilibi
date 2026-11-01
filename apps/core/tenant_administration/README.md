# Tenant administration

Create a customer's tenant and companies together, or search the platform tenant directory.

For example, an operator can create **Acme** with **Acme Trading** as its primary company and **Acme Industrial** and **Acme Auto** as additional companies. The Tenants page and agent doors call the same contract. A failed write creates none of them.

## Operations

Every contract serves every channel through the operation registry.

| Key | Input | Permission |
| --- | --- | --- |
| `core.tenant_administration.tenant.list` | Optional `search`, `page`, `page_size` (at most 100) | Operator standing and `admin.tenancy.tenant.list` |
| `core.tenant_administration.tenant.get` | `tenant_id` | Same list permission |
| `core.tenant_administration.tenant.create` | `name`, `primary_company`; optional `additional_companies`, `parent_id`, `status` | Operator standing and `admin.tenancy.tenant.create`, plus Base Step-up policy |

The create review shows normalized tenant input, parent facts, company names, generated company codes, status and the primary assignment. Status defaults to `active`; `suspended` is also accepted. At most 99 additional companies may be supplied. All names obey their owning module's validation; matching code suggestions receive `_2`, `_3` and so on within 12 characters.

An agent previews, shows the review to its person and commits the single-use plan. If the commit answers `step_up_required`, retry that plan with the person's current 2FA code as `step_up_code`. The step-up key is the create operation key; its declaration is on the installation floor by default. Normal window and tenant policy rules apply.

## Browser

### Parent oversight consent

Administration → Tenancy → **Oversight** (`/tenancy/oversight`) shows the child's recorded parent, table levels, withheld/personal fields, expiry, status and latest reads. Editing requires `admin.tenancy.oversight.manage`; reading requires `.view`. Setup 2FA is offered before granting. Reviewed narrowing and **Revoke all access** remain available without enrollment. **Oversight received** (`/tenancy/oversight-received`) requires `tenancy.oversight.view` and shows only consenting direct children, never siblings or an unconsenting child's name.

The shared browser/agent contracts are `core.tenant_administration.oversight.given`, `.received`, `.catalog` and reviewed `.update`. Update accepts `version`, `active`, optional `expires_at` and a bounded `tables` array of `{table, level, withheld, personal}`; levels are `aggregates` or `rows`. Every commit revalidates live catalog fields and consent version. Grant changes use the child's mutation boundary and audit; the sealed read proof grants no child write or record-page access.

Only fully classified sources enter the consent catalog ([Grid's classification contract](../../base/grid/README.md#parent-oversight)). Core Contacts, accounts, affiliations, addresses and companies are unavailable until their owners finish classification and required privacy/store contracts. An existing grant or forged personal opt-in cannot enable them, and masking/withholding unknown fields does not make a source eligible. The editor and report builder state this limitation instead of presenting unknown personal data as safe to share.

Administration → Tenancy → Tenants (`/tenancy/tenants`) keeps the list, sorting, parent picker and Add Tenant dialog. The dialog also collects the primary company and one additional company per line. The page calls the bounded directory contract through `PageContracts::read` and calls the create contract through `PageContracts::submit`. Its only business write event is `create`; `PageWrites` has no pending allowance. Edit, suspend and provision-existing actions do not exist on this page.

The column picker offers every scalar tenant field the page shows under its dialog label: ID, Name, **Parent tenant**, Sub-tenants and Status. Primary company and Additional companies are entered only when a tenant is created and no tenant view reads them back, so they are not columns. The default columns stay the same.

## Identity and atomic writes

- [Base Tenancy](../../base/tenancy/README.md) owns tenant data and proves the person's live operator standing. Creation preserves today's operator-plus-capability permission; installation ownership is not required.
- Base Tenancy inserts the tenant on the plan transaction and returns a sealed `CreatedTenant` receipt. It proves only this transaction's new tenant and original actor.
- [Core Company](../company/README.md) accepts that receipt to insert companies and the primary assignment. No request supplies a tenant to act as, and no scope is retargeted.
- Each owner rechecks authorization on the mutation transaction and returns every affected row as a `Change`. Tenant, companies, primary assignment, audit and plan consumption commit or roll back together.
- The actor remains the person. The committed action records the agent connection, and the shared trace links all mutations to it. Revoking the grant immediately ends further calls, including a previously reviewed plan.

Creating a tenant does not create accounts, switch the caller into that tenant or confer access to its business data. No general cross-tenant company operation is exposed. The ChatGPT plugin stays a thin adapter and discovers these contracts through the registry.

## Where to go next

See [operation contracts](../../base/agent_api/README.md), [Base Step-up](../../base/step_up/README.md) and [ADR 01](../../../docs/architecture/decisions/01-agent-first-operations.md).
