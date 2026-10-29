# Work calendars

`core/work_calendar` answers, for every module, "is this business time?" and "when is N business hours from now?", from calendars a tenant's administrators keep. It is Core master data beside Company and Address, so Domains share one calendar ([ADR 13](../../../docs/architecture/decisions/13-work-calendars-in-core.md)). Calendars decide business time, never who works: shifts and rosters stay in Domains.

No Bilimbi counterpart. The four day types are Belimbing's attendance calendar (Normal, Rest, Off, Holiday).

## Using it from a module

Declare a dependency on `core/work_calendar` and resolve `WorkCalendars` from service `core/work_calendar.calendars` (`bilibi_work_calendar::SERVICE`). Every call takes the sealed `Scope`; day and arithmetic reads need nothing more, since the caller authorizes its own operation.

| Call | Answers |
| --- | --- |
| `choices(scope, search)` | Active calendars whose code or name contains `search` (empty: any), by name, at most 50 |
| `check(client, scope, code)` | Whether a record may be assigned `code` now: an active calendar, locked `FOR SHARE` in the caller's transaction. Retired or unknown is `Invalid` |
| `default_for(scope, company)` | The company's default code (`work_calendar.default`, company over tenant), or `None` when unset, retired or missing |
| `day(scope, code, date)`, `days(scope, code, from, to)` | Resolved dates |
| `timetable(scope, code, from, to)` or `Timetable::load(client, scope, code, from, to)` | Business-time arithmetic for a window of at most 731 days |

- Store the calendar **code**, never an id. Codes are stable: a calendar is retired, never deleted.
- `None` from `default_for` is the caller's choice: the facade never substitutes clock time. `Timetable::clock()` is always open, so a caller that falls back to clock time keeps one code path.
- A `Timetable` is the per-request cache: one load serves a list of 50 cases. There is no process-wide cache, so the next request sees a saved change.
- A load is one statement, so it reads one snapshot on any connection: a plan committing meanwhile is seen whole or not at all.

## Day types and resolution

Every date resolves to exactly one type, fixed in code (`DayType`): `working` (with opening periods), `rest` (the weekly rest day), `off` (another non-working day, such as Saturday in a five-day week) or `holiday`. A Domain cannot add one. A specific closure is a named exception of one of the four; a half day is a short `working` exception.

1. The calendar's **exception** covering the date wins.
2. Then a **holiday** from an included holiday set, or a **substitute** for one.
3. Then the **week** of the rule version in force.

A `Day` gives the date, its type, `weekly` (the week's type underneath, such as a holiday on a rest day), its periods, the `Reason` (week, holiday, substitute, exception) and the zone.

- **Rule versions.** A version holds the zone and the full week from its `effective_from` until the next version; a changed weekend or new hours adds a version and earlier dates keep their rules. The first version also governs any earlier date, so setting or replacing it changes those too. Setting a version on an existing date replaces it.
- **Substitutes** (`substitute` per calendar): `none` (default), `rest` (a holiday on a rest day moves) or `rest_and_off` (also on an off day). It moves to the next day the calendar's own week makes working that is not a holiday or another holiday's substitute, at most 14 days on. A gazetted replacement date is entered as an ordinary holiday instead.
- **Substitutes chain.** Holidays are moved in date order, so one substitute can take the day a later holiday needed, however far back that runs. A load follows the chain back to a gap of more than 14 days without holidays, which no substitute crosses, so a date resolves the same in any window.
- **Exceptions**: a date or range (at most 366 days) with its own type and, for `working`, periods. Ranges of one calendar never overlap.

## Periods and zones

- Up to four periods a day, `HH:MM-HH:MM` in the version's zone, half-open (`09:00-18:00` closes at 18:00), sorted and not overlapping. An end at or before the start is the next day (`22:00-02:00`); a period belongs to the day it starts on. A day's late hours may not run into the next day's.
- Wall-clock times become UTC instants by `Period::bounds`' rules: a time in a daylight-saving gap starts at the first valid instant, a time in a fold uses the earlier one. Business time is real elapsed time, so a period across a clock change is shorter or longer.
- Dates are stored zone-free; nothing stores a computed instant.

## Arithmetic (`Timetable`)

All instants are `jiff::Timestamp` (UTC) and durations non-negative `jiff::SignedDuration`.

| Call | Result |
| --- | --- |
| `is_open(t)` | Whether `t` lies in an opening period |
| `next_open(t)` | `t` if open, else the next period's start |
| `add(t, d)` | `d` of business time counted from `t`, or from the next opening if closed. A result exactly at a closing time stays there ("due 18:00 today", not "09:00 tomorrow"); `add(t, 0)` is `t` |
| `elapsed(a, b)` | Business time in `[a, b)`; `a` after `b` is `Backwards` |
| `add_working_days(date, n)` | The `n`th working day after `date`; `n = 0` is `date` or the next working day. Take the time from `day` |

A search looks at most 366 days ahead (`SEARCH_DAYS`) and never past the loaded window, then returns `TimeError::Beyond` rather than looping, as it does for a calendar with no working days. An instant outside the window is `Beyond` too: load the window you need.

## Holiday sets

- Whole-day holidays a tenant keeps, optionally for a country and division (`country_iso`, `admin1_code` such as `MY.14`), checked against `core/geonames`: a set with a country can be saved only after countries are imported.
- A calendar includes several sets (national plus one state). No holiday data ships and nothing seeds: governments announce holidays late, some by moon sighting.
- Installation-wide sets kept by the platform operator are not built yet: a change to one cannot record `changed` in other tenants from the operator's transaction (follow-up issue).

## Administration

Tenant capabilities `admin.work-calendar.view`, `.list` and `.manage`; the `tenant_owner` role holds all three. Writes recheck `.manage` inside their transaction, are audited (`core/work_calendar.calendar`, `.rule`, `.exception`, `.holiday_set`, `.holiday`, `.include`) and carry a version fence on the calendar or set. Every statement carries the tenant predicate.

- **Locks.** A write locks its calendar or set before reading. A holiday write locks its set, and an inclusion its calendar and then the set (shared), so of a concurrent pair the second to commit sees the other and records `changed`.
- **One plan, one version.** Each operation of a reviewed plan names the version read before the plan, and the commit fences each against it, so one plan may write a calendar or set several times. A change committed elsewhere since the review is still refused as stale.
- **Bounded reads.** Lists page (`page`, `page_size` at most 100) with the total. A calendar's rule versions and included sets, and a set's including calendars, come as the first 400 with the total; exceptions and holidays page 400 at a time (`page`).

Agents and the [pages](#pages) use these contracts (ADR 01); writes are reviewed (plan, then commit):

| Operation | Kind | Does |
| --- | --- | --- |
| `core.work_calendar.list` | read (list) | One page of calendars, by code, with the total; each with the zone of the rule version in force today and its included sets (the first 400 and their total); `search` matches code or name |
| `core.work_calendar.holiday_set.list` | read (list) | One page of holiday sets, the same way |
| `core.work_calendar.get` | read | One calendar: versions, sets, a page of exceptions, version |
| `core.work_calendar.holiday_set.get` | read | One set with a page of its holidays (optionally from and to a date) and the calendars including it |
| `core.work_calendar.days` | read | Resolved dates of a range |
| `core.work_calendar.business_time` | read | `is_open`, `next_open`, `add`, `elapsed`, `add_working_days`; `beyond: true` is an answer |
| `core.work_calendar.create` | write | A calendar with its zone (default the tenant's `localization.timezone`), first week and substitute rule |
| `core.work_calendar.update` | write | Rename, substitute rule, retire or reactivate |
| `core.work_calendar.rule.set` | write | A week from a date |
| `core.work_calendar.exception.add` / `.remove` | write | Dated exceptions |
| `core.work_calendar.holiday_set.create` / `.update` | write | Sets, with country and division |
| `core.work_calendar.holiday_set.holidays` | write | Add (or rename) and remove up to 400 holidays: a year at a time. A removal names a date the set holds, once; the reply counts what was removed |
| `core.work_calendar.include` | write | Include a set in a calendar, or leave it out |

Setting `work_calendar.default` (tenant and company scopes, null by default) names a calendar code. A company without its own value, or whose value is cleared, uses the tenant's.

### Pages

Under Administration, behind `admin.work-calendar.list` (menu and lists) and `.view` (records); every write is one of the contracts above, declared in `PAGE_WRITES` and asked again inside it.

| Page | Path | What |
| --- | --- | --- |
| Work Calendars | `/work-calendars` | Code, name, status, zone today, included sets; search; New Calendar opens a dialog (code, name, zone, first date, substitution, week) and lands on the calendar |
| Calendar | `/work-calendars/{code}` | Name and substitution edited in place; Retire (confirmed) or Reactivate; weekly hours by version with Set week from a date; included sets with Include and Leave out; exceptions with Add and Remove (confirmed), 400 a page (`?page=`) |
| Month | `/work-calendars/{code}/month?month=YYYY-MM` | Each date of the month as a day tile (`base/ui` `DayTile`): type, opening periods, reason; previous and next month are links |
| Holiday Sets | `/holiday-sets` | Code, name, country, division; search; New Holiday Set opens a dialog whose divisions follow the country |
| Holiday Set | `/holiday-sets/{code}?year=YYYY` | Name, country and division edited in place; the calendars that include it; one year's holidays with Add, rename in the row and Remove (confirmed) |

- A new calendar's zone starts as the current company's `localization.timezone` (else the tenant's); the default month, year and dates are today there.
- A write against a version that moved on since the page read it says so and asks for a reload; nothing is overwritten.
- Grid table `work_calendars` (keyed by code): code, name, status, substitution, zone today and included sets, behind `.list`.

### Retirement

A retired calendar leaves `choices`, fails `check` for a new assignment, and still computes for records holding it.

## Events

Each write that moves dates records `core/work_calendar.changed` in its own transaction, once per affected calendar, with subject the calendar code and payload `{"calendar": code, "from": date | null, "to": date | null}` (null is open-ended). No stored result elsewhere changes: a module with open items, such as SLA due times, subscribes and recomputes them itself.

- **Ranges.** A holiday or inclusion change runs to 14 days past the chain of holidays (each within 14 days of the one before) that starts at its last date, since substitutes displace one another; a calendar that substitutes nothing changes only the holidays' dates. Setting or replacing the first rule version is `from: null`, since it governs every earlier date.
- **One event per change.** A reviewed plan that changes one calendar twice records two events; recording the very same change again is a no-op.

## Not here

Shifts, rosters and attendance; pay rates by day type and leave-day counting; SLA, voice-menu and routing logic; holiday feeds, file import and iCalendar; per-person calendars; meeting and booking calendars. Installation-wide holiday sets come in a later change ([#210](https://github.com/BelimbingApp/bilibi/issues/210)).

## Walkthrough

On a disposable database, set `DATABASE_URL`, `BILIBI_WORK_CALENDAR_DEMO=disposable`, `BILIBI_SETTINGS_KEY` and `BILIBI_USER_SIGNING_KEY`, then run `cargo run -p bilibi-work-calendar --example work-calendar-walkthrough`. It migrates, installs the walkthrough fixture and, through the reviewed contracts, builds a Monday–Friday calendar with a lunch break, national and state holiday sets, a shutdown, a special working Saturday and a half day; prints the resolved days with their reasons, substitutes under a Friday–Saturday weekend and a new rule version; answers each `business_time` operation across breaks, weekends, holidays, a close boundary, a closed start, a daylight-saving gap and fold and a period past midnight; and shows the company default, retirement, `changed` events, tenant isolation, refused writes and the audit trail. It also shows a substitute chain resolving the same in any window, one plan editing two sets of a calendar, one plan writing one calendar twice and one set twice (refused when an operation names the version an earlier one makes, or when another session changed the calendar after the review), a holiday write and an inclusion committing at once, a business-time read held while a plan commits answering from one snapshot, replacing the first rule version, an exception's hours in its removal evidence, paged lists and holidays, removals of dates a set does not hold, a tenant owner managing calendars, and the migration ID.
