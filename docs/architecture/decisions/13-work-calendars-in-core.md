# ADR 13: Work calendars are Core master data with four fixed day types

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** Where business calendars live, which day types exist, and what a calendar decides (`core/work_calendar`)
**Last Updated:** 2026-10-29

> Source: [issue 135](https://github.com/BelimbingApp/bilibi/issues/135); decision 6 from [issue 210](https://github.com/BelimbingApp/bilibi/issues/210). Bilimbi has no calendar module. Belimbing keeps work calendars in People Settings and resolves dates to Normal, Rest, Off or Holiday (`AttendanceCalendarResolver`), and expects "a future cross-cutting Calendar service … once a second module needs it".

## Context

**Several Domains need business time.** A CRM computes SLA due times and open/closed voice-menu paths, Finance payroll reads rest days and public holidays, Factory schedules on working days, and a People Domain's rosters read day types. Domains may depend on Core but never on each other, so a calendar kept in one Domain could not be shared. Module IDs are part of migration IDs, so the module's place had to be chosen before code existed.

## Decision

**1. Work calendars are Core master data**, in `core/work_calendar` beside `core/company` and `core/address` (not `core/calendar`, which suggests meetings). Domains declare a dependency on it; no port is needed.

**2. Four day types, fixed in code:** `working` (with opening periods), `rest`, `off` and `holiday`. Payroll, attendance and leave rules key on exactly these, as in Belimbing, so a Domain cannot register another that the others would not understand. A specific closure is a named exception of one of the four, and a half day is a short working day: there is no fractional type.

**3. Calendars decide business time, Domains decide who works.** A calendar answers whether an instant is business time and does business-time arithmetic. Shifts, rosters, attendance and per-person calendars stay in Domains.

**4. Resolution has one order:** the calendar's exception, then a holiday or substitute from an included set, then the week of the rule version in force. Weeks are effective-dated, so a change never rewrites earlier dates, except that the first version also governs every date before it. Substitutes are resolved from where no earlier holiday's chain can reach, so a date resolves the same in any window. A change rewrites no stored result elsewhere: it records `core/work_calendar.changed` and consumers recompute.

**5. Arithmetic is bounded and explicit.** A `Timetable` loads a window of at most 731 days and searches at most 366 days ahead, returning `Beyond` rather than looping. No calendar is the caller's choice: the facade never substitutes clock time, and `Timetable::clock()` is the explicit fallback.

**6. Installation-wide holiday sets are the operator's rows, told to other tenants by a queued fan-out.** The platform operator keeps a country's or state's holidays once, as sets of its own tenant marked `installation`. Every tenant reads and includes them, and only the operator's tenant writes them, with the platform capability `admin.work-calendar.holiday-sets.manage`. A tenant that wants to differ copies one into a set of its own. An event records only into its transaction's tenant, so the operator's holiday write queues one job in its own transaction (`enqueue_system_within`), as Events queues its deliveries. The job runs as the declared principal `work-calendar-publisher` with a standing grant (ADR 12), and records `changed` in each other tenant including the set, one transaction per tenant, keyed by the operator's trace. A tenant that denies the principal is told nothing, and its calendars still read the new holidays. This is the one job that writes outside the tenant Queue seals it to (`apps/AGENTS.md`: unattended work runs under `Execution::scope()`): reaching other tenants is its whole purpose, so it rehydrates the same declared principal in each, rechecks the grant there, and fences each transaction on its own lease.

## Rejected alternatives

- **Calendars in a People Domain** (Belimbing's place). Finance, Factory and a CRM could not read them without depending on another Domain.
- **Operator-defined day types** (a pick-list). A Domain's pay or leave rule could not recognise a type another tenant invented (ADR 07: values logic branches on stay in code).
- **A fractional day type for half days.** A short working exception says the same with real hours, and arithmetic needs the hours anyway.
- **A process-wide cache of resolved days.** A saved change would need invalidation across processes; the per-request `Timetable` sees the next change on the next request.
- **No event for installation-wide changes** (consumers recompute on a schedule). Every consumer would carry a sweep, and an SLA due time would stay wrong until it ran.
- **Recording each tenant's event in the operator's transaction.** The write would hold every tenant's calendars at once, and Events records only into its transaction's tenant by design.
- **An installation table outside tenancy for the sets.** Audit, versions and the reviewed writes already work per tenant; the operator's tenant keeps them like any other set.
- **Bundled holiday data.** Governments announce holidays late, some by moon sighting; no bundled feed is authoritative, and seeded data would look like it is.

## Consequences

- Installation-wide holiday sets cross tenants on purpose, each statement commented in the code: any tenant reads (and, to include or copy one, share-locks) the operator's installation-wide rows; the operator's writes check a new installation-wide code against every tenant's, count the other tenants' calendars a change tells, and the job finds the tenants to tell. Every other statement keeps the tenant predicate (the module README lists them).
- A tenant hears of an installation-wide change shortly after it commits, not in the same transaction. A tenant that denies `work-calendar-publisher`, or is suspended, is not told; the operator's tenant denying it, where the job runs, tells no tenant.
- The administration pages and the month view ([#209](https://github.com/BelimbingApp/bilibi/issues/209)) call these contracts and draw each day with the day tile in `base/ui`, so a Domain's roster shows the four types the same way.
