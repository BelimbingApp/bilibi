# ADR 18: Company work access, page scope and background work

**Document Type:** Architecture Decision Record
**Status:** Accepted; implementation pending
**Agents:** codex/gpt-6 (author)
**Scope:** Assignment to a company's work, the company switcher, page scope and the company captured by background jobs
**Last Updated:** 2026-10-31

The owner agreed this design on 2026-10-31. Implementation follows in separate commits.

## Context

An accountant should be able to work in assigned client companies without seeing every customer or supplier as a workspace. Today the switcher offers the account's affiliation company, or every live company when the account holds `admin.company.tenant-wide.manage`. Creating a supplier record can therefore make it appear as a company to work in.

The top bar also shows the chosen company on pages that do not use it. A company directory or administration page can look filtered to that company even though it covers the tenant.

Background work has a third mismatch. Queue captures the account's affiliation company, and Artifacts normally requires that company. Bulk Data therefore refuses imports, exports and bulk updates when the session has chosen another company.

ADR 09 established the session choice; ADR 10 established background operation contracts. This decision would replace their company-reach and affiliation-only job rules. The account's identity, existing capabilities, audit attribution and tenant boundary remain authoritative.

## Decision

### Give people desks in workplaces

**Contacts you trade with, workplaces you work in.** A company becomes a workplace when people have desks there; the switcher lists your desks only; the top bar always means “I am working in X”.

A company's existing record page has a People with a desk here section listing staff who may work in it. A manager with the existing company-manage authority can add or remove a person, including assigning themselves a desk. Every change is audited. The person's or user's existing page presents the same relationship as Desks. These entry points and agent actions call one operation contract.

A person's own affiliation company is always available to them. A desk adds other companies within the same tenant. A customer or supplier with no assigned staff and no affiliated account remains a company record and does not appear in anyone's switcher.

Work assignment does not create company-specific roles. Existing capabilities continue to decide which actions the person may perform. Company-record administration retains its current reach rules; permission to edit a record does not itself assign the editor to work there.

### Offer only assigned companies

The switcher offers the person's own company and companies where they are assigned a desk. `admin.company.tenant-wide.manage` no longer expands that list.

Tenancy accepts a chosen company only while the person may work there and the company remains live and unarchived. The proof runs on every authenticated request. Removing a desk assignment makes an existing session fall back to its own affiliation company on its next request; Session forgets the invalid choice. Existing protection against live events acting under a different company remains in force.

Cross-tenant companies and customer portal accounts remain outside the staff switcher. Impersonation continues to use the viewed person's access and keeps the operator's parked choice separate.

### Declare what a page covers

Every route and menu contribution declares whether it covers one chosen company or the tenant. Base and Core administration and settings pages normally cover the tenant. Domains declare company scope wherever the records belong to the chosen company, such as a Finance book.

The top bar shows the company label or switcher on company-scoped pages. Tenant-wide pages keep the chosen company label and mark the page as applying to all workplaces, with a tooltip explaining that the page covers all companies the person may access. The label does not grant access or bypass the page owner's authorization.

The route declaration determines the shell's presentation; changing a label alone does not filter data. Page owners must use the chosen company in their queries and continue checking their operation capabilities. Official Domains adopt the contract through companion changes. DESIGN.md and the existing Domain authoring guides describe the declaration.

### Keep queued work in its chosen company

Queue captures the sealed request's chosen company separately from the account's affiliation identity. A later switch in the browser does not move an existing job.

Every attempt re-proves the account, its captured identity and its access to the captured work company. Losing work access must refuse that attempt rather than silently run it in the person's own company. Transactional revalidation retains the same company proof before effects commit.

Artifacts accepts a company the person may work in, subject to the owning module's existing authorization. Reads and downloads re-prove access as well as the owner's rules. Retention and system-principal work keep their existing authority model.

Bulk Data files runs and artifacts under the selected company, and its jobs use that company. The affiliation-only switch-back refusal can then be removed. Exports and bulk updates continue through their existing operation contracts and field restrictions.

## Alternatives

- Keeping tenant-wide management as work access would continue to put unassigned customer and supplier records in the switcher.
- Giving every company its own roles would expand this change beyond assignment; existing capabilities already govern actions.
- Showing the switcher on every page would continue to imply that it filters tenant-wide pages.
- Falling back to the affiliation company inside a queued job could execute reviewed work under a different company.
- Backfilling assignments from management grants would preserve the unwanted access. Development databases are disposable, and the project has no production users during this change.

## Consequences

Assignment changes are audited through MutationBoundary. Schema changes use new migrations; no applied migration is edited and no compatibility or access backfill is introduced.

Implementation must preserve a distinct proof for company-record administration and company work. A desk assignment must make ordinary company-scoped work possible with its existing action capability, without accidentally granting administration of that company's record.

Runtime proof covers an administrator without customer or supplier entries, a client appearing after assignment, an accountant's assigned clients, removal on the next request, tenant-wide labels, and light, dark and phone layouts. Under Company 002, proof includes switching, exporting, downloading and performing a bulk update.
