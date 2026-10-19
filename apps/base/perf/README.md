# Perf history

`base/perf` records redacted request, live-event, job and process samples into
`base_perf_samples` without ever delaying the work it measures. Reading the
history, its Performance page and its dashboard health widget are owned here,
alongside recording, storage and retention.

The service is `base/perf.recorder` (`Recorder`). Re-exported as
`bilibi_base::perf`.

## Operator page and dashboard health

Open `/system/performance` as a platform operator with
`admin.system.perf.view`. Operator standing and the capability are independently
required on HTTP, socket mount, URL changes, events and background refreshes.
The page never accepts a tenant parameter to widen access. The discovered menu
entry and `base-perf-health` dashboard widget use the same platform capability.

The page composes Base UI's `ListState`, filter toolbar, `ListColumns`, `FlexTable`, pagination and `Tab::patch` tabs (`src/web/`). Slowdowns is the default and leaves `tab` out of the address; All samples is `tab=samples`. Kind, outcome, exact page address or job name, page size (25/50/100/300), page and tab survive reload and browser Back. Like Bilimbi, the list is all retained history, with no time-range filter. Slowdowns shows the existing regression comparison (the last hour against the hour before, at least five samples in both windows, at the configured p95 threshold). The page does not compute a separate slowest-pages list. Column changes and zoom reuse the loaded data; the next host refresh reloads it. Timestamps follow the shell's Company/UTC/Local display policy.

One status line above the tabs names recording, samples kept, samples waiting to be saved, and samples dropped. It uses a warning only when the recorder or the history store is unavailable, or when samples are waiting or were dropped. An empty sample read says nothing matches; a failed read says history is unavailable and withholds a fabricated zero count or pager. A failed regression read has its own unavailable state. Recorder health, waiting and dropped counts describe this process. Turning recording off leaves retained history readable and says so on that line.

`dashboard.performance` is an independent dashboard embed, refreshed through
`PanelCache` every 60 seconds. It checks operator access before its cache on
every update. A refused panel clears that cache, skips diagnostics and renders
an em dash without a link; the dashboard also removes entries whose offering
capability is lost. Customize mode withholds the navigation link.

## Observing work: use `observe`, not your own timer

```rust
let response = recorder
    .observe(Kind::Request, Some("/companies/{id}"), handler, |response| Finish {
        outcome: Outcome::Ok,
        response_bytes: response.body().size_hint().exact(),
    })
    .await;
```

`observe` times the future, attributes the database round trips made inside it
(`bilibi_database::timing`) and hands one `Sample` to `submit`. Identities are
route templates, event names or worker ids; a concrete path, query string or
anything failing `valid_identity` is run unobserved. Dropping the future
records a `cancelled` sample. Adapters (`apps/web/src/perf.rs`, the queue
observer in `jobs.rs`) only say which identity and how it ended.

## Why telemetry cannot slow business work

- `submit` never awaits, never touches the database or settings and never
  fails. It returns `Accepted`, `Ignored` (off, below the minimum duration,
  outside the sample rate) or `Dropped` (queue full).
- The policy (`perf.enabled`, minimum duration, sample rate) is atomics
  refreshed in the background every 10 s, so a settings change is noticed by
  every process without a hot-path read.
- One writer task (`Recorder::run`, started by the web app) batches rows on its
  own connections with a 5 s bound. A failed or timed-out batch is counted in
  `Stats::dropped`; it is never retried or buffered without limit.
- `health_status` reports `Degraded` when samples were lost or the queue is
  full, and `Unavailable` when the writer is not running or the store cannot be
  read; the request path never consults it.

## Process facts

Each minute the writer stores a `runtime` sample: resident memory, CPU time,
threads, open file descriptors, Tokio queue and task counts, and database pool
use. These replace
Bilimbi's BEAM facts (schedulers, run queue, atoms, ports).

## Retention

`perf.history.keep_days` (30) and `perf.history.max_rows` (200 000) bound the
table. The daily `base/perf-retention` Schedule definition prunes both, and the
writer re-enforces the row cap every 100 accepted rows. Retention runs even if
recording is switched off.

## Running proof

`examples/perf-walkthrough.rs` drives observation, redaction, settings policy,
queue saturation, an unreachable store, regressions and retention against a
disposable database. It refuses to run without `BILIBI_DISPOSABLE_PERF_DEMO=yes`
because it truncates `base_perf_samples`:

```sh
BILIBI_DISPOSABLE_PERF_DEMO=yes DATABASE_URL=postgres://... \
  cargo run -p bilibi-perf --example perf-walkthrough
```
