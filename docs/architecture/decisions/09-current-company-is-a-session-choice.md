# ADR 09: The current company is a session choice, never a grant

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/fable-5.1-default (author)
**Scope:** Which company a page operates on when a person may act in several (`Scope::current_company`, the shell's company switcher)
**Last Updated:** 2026-10-28

> Source: [issue 203](https://github.com/BelimbingApp/bilibi/issues/203). Finance's [bilibi-dom-finance#66](https://github.com/BelimbingApp/bilibi-dom-finance/issues/66) and [#59](https://github.com/BelimbingApp/bilibi-dom-finance/issues/59) wait on it.

> [ADR 18](./18-company-work-access-and-page-scope.md) replaces this record's company-work reach and affiliation-only background company rules. Its implementation follows separately.

## Context

**An accountant keeps the books of several companies in one tenant.** Until now a page that operates "on the current company" read the account's affiliation (`Scope::actor().organisation()`), so a person who may reach every company through the tenant-wide grant still saw only their employer's book, and the company a page showed was named nowhere but inside the account menu. Agent operations already name their company explicitly (`company_id`) and prove it with `bilibi_company::authorize_target`.

Three places could hold the choice: the address of every page, the account's saved preferences, or the browser session. Three things could decide reach: the choice itself, a company hierarchy, or the existing reach rule (own company, or any company with `admin.company.tenant-wide.manage`).

## Decision

**The current company is a per-session choice, shown in the top bar, that grants nothing.**

- `Scope::current_company()` is the company a request works in: the session's chosen company while the account still reaches it, else the affiliation's company. `Scope::actor().organisation()` stays the affiliation and stays the input to reach. A page that operates on "the current company" reads `current_company()`; an owner that decides reach or binds a record to the person's employer keeps reading the affiliation.
- The choice lives on the session row (`sessions.company_id`), written only by Core User's `choose_company` after Core Company proves the reach, and proved again by Core Company on every authentication through the account verifier. A choice the account no longer reaches (grant revoked, company archived) falls back to the affiliation and is forgotten.
- Impersonation shows exactly what the viewed account sees. Entering a view starts from that account's own default and parks the operator's choice in `sessions.held_company_id`; a choice made during the view applies to it alone; leaving restores the operator's choice. A viewed account that lacks reach to the parked company never clears it: forgetting applies only to the choice of the identity that holds the row.
- Reach is the existing rule. A hierarchy does not grant reach to a subsidiary; a chosen company does not widen any capability. Every read and write still proves its target company with its own capability.
- A live page is bound to the company it was rendered under (the shell stamps it, the socket opens with it), as it is bound to the account and the CSRF secret. A switch in another tab, or a reconnect after one, makes its next event refuse before acting and reload the whole page under the new company, so no write started on one company lands on another and no live region re-renders under a top bar that names the old company.
- Agents carry no session choice. Operations name their company explicitly; `core.company.reachable` lists the companies an agent may name.

We rejected a choice in every page address because each page would carry and validate it, and copied links would silently carry a company. We rejected a saved account preference because a choice is about the work in front of the person now, not a setting to keep across devices. We rejected making the switch change reach because a UI selection must never grant authority.

## Consequences

- Finance and other Domains adopt it by replacing `scope.actor().organisation()` with `scope.current_company()` on pages and reports that operate on the current company, and keeping `authorize_target` on every read and write.
- Cross-tenant access stays out of scope: a session belongs to one tenant, and the switcher only offers companies of that tenant.
