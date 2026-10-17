# DateTime display and period contract

`base/datetime` is a policy boundary, not date arithmetic: storage stays UTC and
business modules keep their own deadline semantics. It owns two Settings
definitions and the request-scoped display context presentation renders with.

- `localization.timezone` — tenant/company scoped IANA identifier, validated by
  `valid_timezone` against the bundled `jiff` tzdb (links such as
  `Asia/Kuala_Lumpur` included, `timezones()` lists them). Core Company owns the
  editor and writes through Settings under its own capability; Base never
  queries Core. Editor metadata is attached by P3.
- `ui.timezone.mode` — the actor's own `company | local | utc`, default `company`.
- `DisplayPolicy::display(client, actor, company, locale)` returns a
  `DisplayContext { mode, timezone, locale }`. The locale is the one the caller
  already resolved; pass `bilibi_locale::Resolved::locale`. `company` is a
  request: Settings reproves it with the Company provider on the same
  connection, so an ineligible company refuses. A stored zone that no longer
  converts falls back to the default instead of breaking pages.
  `DisplayContext::anonymous()` is the pre-sign-in `local` context.
- `DisplayContext::render` returns `Rendered` carrying both server-decidable
  texts (`company`, `utc`), the mode's own `text` and an RFC 3339 `iso` value.
  The shared timestamp template must emit both (`data-text-company`,
  `data-text-utc`) so the browser follows a mode change by copying a server
  string; `Local` is the only mode the browser formats. Dates are zone-free
  (`render_date`). Format is `dd/mm/yyyy, HH:MM` with a zone abbreviation, as in
  Bilimbi. Do not format an instant by hand in a template.
- `Period` parses `period/from/to`, with inclusive days and the Last 30 days
  default. `bounds(zone)` converts to a half-open UTC range under the zone's
  real rules (23/25-hour days; a midnight gap starts at the first valid
  instant, a fold at the earlier). `Clock::resolve` picks the zone for "today"
  from the mode; the browser zone is accepted only in `Local`, validated, and
  otherwise UTC.
- `PeriodFilter` (`filter.rs`, `templates/period_filter.html`) is the compact
  period control a list page renders beside its toolbar, the counterpart of
  Bilimbi's `<.period_filter>`: a preset select that applies at once and a
  compact modal whose custom dates stay a draft until Apply range succeeds.
  The page keeps the applied `Period` in its URL and owns the component's
  transient state: build it with `PeriodFilter::new(id, period, &clock)`,
  call `set` after re-reading the URL or clock, hand every live event to
  `event(kind, payload, &clock)` and act on `filter::Outcome::Applied`
  (replace the URL period, reset the page) or `Update` (re-render). Use it
  instead of bare date inputs or a hand-written range. Its event names are
  fixed (`period_select`, `period_apply`, `period_cancel`), one filter per
  page; `/system/api-exchanges` is the first host.
- The browser halves live in `base/ui` `assets/datetime.js`, served inside
  `/assets/hooks.js`: `DateTime` keeps every `<time data-hook="DateTime">`
  (the `instant` macro of `flex_table.html`, or the same attributes) on the
  shell's live display mode, copying the server's company/UTC text and
  formatting only `local`; `BrowserTimeZone`, mounted on the region holding a
  page's date filters, reports the `browser_timezone` event (`timezone`) on
  mount and on every mode change, so local-mode calendar days follow the
  browser zone without a reload. A page validates it through `Clock::resolve`.
  A page whose hook can survive a server remount supplies a fresh `data-timezone-request` on each mounted state and clears it after receiving a report; the hook reports each new request once. Until that report arrives, local-mode bounds are provisional and must not clamp the bookmarked page against a UTC count. Schedule History uses this handshake for Back and reconnects.

Run `BILIBI_DISPOSABLE_P4_DEMO=yes cargo run -p bilibi-web --example
p4-locale-datetime` against a migrated disposable database (it appoints
tenant 1 operator and resets only the keys it uses).
