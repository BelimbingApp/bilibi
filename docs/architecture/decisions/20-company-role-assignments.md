# Company and date bounds belong to role assignments

**Document Type:** Architecture decision record
**Status:** Accepted; implemented
**Agents:** codex/gpt-6
**Scope:** Authz role assignment, company-owned operational records and engagement expiry
**Last Updated:** 2026-11-01

A person can work in two companies and hold different permissions in each. Finance's appointed-auditor engagement also needs an enforced end date. A desk proves workplace access; the administrative owner of a custom role does not limit where its capabilities apply.

Store an optional company, inclusive UTC start/end dates and purpose on the existing role assignment. The same role can have one assignment per company and an unscoped assignment. Every decision reads live membership, so expiry needs no job and retained scopes do not preserve expired authority. Direct overrides keep their tenant-wide meaning, with deny precedence.

Company scope is an owner opt-in on a capability. The owner must prove its explicit target through the shared company authorization path before opting in. A scoped assignment cannot contain tenant-only keys or grant-all. This keeps an existing tenant-wide list closed until its owner can enforce the company boundary. Effective sets and discovery use the current company; the owner uses its explicit record target. Neither choice grants a desk.

A role owner may require a named company engagement through an Authz contribution. The shared grant facade checks it in preview and commit; live role membership also excludes incomplete assignments. Finance must not implement another engagement-permission table or expiry worker.

Field restrictions remain conservative: any currently held restricting role withholds its fields across the tenant. Scoped grants cannot relax those restrictions. Existing provider-managed memberships stay separate and unscoped; an engagement role cannot be provisioned without its required manual bounds. Tenant-wide shared-report subscriptions cannot inherit company-scoped membership.

Rejected alternatives were Finance-owned grants, scheduled removal at expiry and filtering only the navigation. Each would leave another data door or a retained authorization path using different rules. UTC calendar dates are explicit rather than inferred from the grantor's browser timezone.

The operational contract and runtime proof are in [Authz](../../../apps/base/authz/README.md#company-and-engagement-assignments).
