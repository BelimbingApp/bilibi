# Domain pages and workplaces

Declare whether each page follows the selected desk or covers the tenant.

## Routes

A page for company-owned records sets `company_scope: CompanyScope::Company` on its `Route` contribution. A shared directory, administration or settings page sets `CompanyScope::Tenant`. Import the enum from `bilibi_module_registry::contributions`.

The declaration tells the shell what the page covers. **The owner still filters company-owned reads and writes by `Scope::current_company()` and proves work access with `bilibi_company::authorize_work_target` plus its action capability.** Explicit company IDs from an agent require the same work-access proof. Company-record administration uses `authorize_target`, which retains the tenant-wide management capability.

## Menu

Add `.company_scoped()` to a company page's `MenuItem::page(...)`. Tenant-wide entries use the constructor's default. Web refuses a leaf whose declaration disagrees with its route.

The top bar retains the selected desk on every staff page. Tenant-wide pages add **All workplaces** and explain that switching desks does not filter them. A Domain draws no second company switcher and owns no CSS or JavaScript.

## Upgrading

Add `company_scope` to every route literal, classify each route by the records it actually reads, and update its menu declaration. Import Queue handler, execution and state types directly from `bilibi_queue`; its implementation modules are private. Staff accounts have tenant/person identity and explicit desks, with no account-owned company or affiliation. See the affected Platform module's README for its operation contract.

## Where to go next

- [Mounting guide](AGENTS.md)
- [Route contracts](../base/module_registry/README.md)
- [Company work access](../core/company/README.md#the-current-company)
- [Desk decision](../../docs/architecture/decisions/18-company-work-access-and-page-scope.md)
