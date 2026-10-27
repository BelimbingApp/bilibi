# ADR 09: Work calendars are Core master data with four fixed day types

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** Where business calendars live, which day types exist, and what a calendar decides (`core/work_calendar`)
**Last Updated:** 2026-10-27

> Source: [issue 135](https://github.com/BelimbingApp/bilibi/issues/135). Bilimbi has no calendar module. Belimbing keeps work calendars in People Settings and resolves dates to Normal, Rest, Off or Holiday (`AttendanceCalendarResolver`), and expects "a future cross-cutting Calendar service … once a second module needs it".

## Context

**Several Domains need business time.** A CRM computes SLA due times and open/closed voice-menu paths, Finance payroll reads rest days and public holidays, Factory schedules on working days, and a People Domain's rosters read day types. Domains may depend on Core but never on each other, so a calendar kept in one Domain could not be shared. Module IDs are part of migration IDs, so the module's place had to be chosen before code existed.

## Decision

**1. Work calendars are Core master data**, in `core/work_calendar` beside `core/company` and `core/address` (not `core/calendar`, which suggests meetings). Domains declare a dependency on it; no port is needed.

**2. Four day types, fixed in code:** `working` (with opening periods), `rest`, `off` and `holiday`. Payroll, attendance and leave rules key on exactly these, as in Belimbing, so a Domain cannot register another that the others would not understand. A specific closure is a named exception of one of the four, and a half day is a short working day: there is no fractional type.

**3. Calendars decide business time, Domains decide who works.** A calendar answers whether an instant is business time and does business-time arithmetic. Shifts, rosters, attendance and per-person calendars stay in Domains.

**4. Resolution has one order:** the calendar's exception, then a holiday or substitute from an included set, then the week of the rule version in force. Weeks are effective-dated, so a change never rewrites earlier dates, and a change rewrites no stored result elsewhere: it records `core/work_calendar.changed` and consumers recompute.

**5. Arithmetic is bounded and explicit.** A `Timetable` loads a window of at most 731 days and searches at most 366 days ahead, returning `Beyond` rather than looping. No calendar is the caller's choice: the facade never substitutes clock time, and `Timetable::clock()` is the explicit fallback.

## Rejected alternatives

- **Calendars in a People Domain** (Belimbing's place). Finance, Factory and a CRM could not read them without depending on another Domain.
- **Operator-defined day types** (a pick-list). A Domain's pay or leave rule could not recognise a type another tenant invented (ADR 07: values logic branches on stay in code).
- **A fractional day type for half days.** A short working exception says the same with real hours, and arithmetic needs the hours anyway.
- **A process-wide cache of resolved days.** A saved change would need invalidation across processes; the per-request `Timetable` sees the next change on the next request.
- **Bundled holiday data.** Governments announce holidays late, some by moon sighting; no bundled feed is authoritative, and seeded data would look like it is.

## Consequences

- Installation-wide holiday sets (kept by the platform operator, read-only to tenants) are deferred: a change to one cannot record `changed` in other tenants from the operator's transaction, so they need a cross-tenant fan-out designed on its own.
- Pages, the month view and a day-type tile in `base/ui` follow in a separate change; the contracts they will call exist now.
