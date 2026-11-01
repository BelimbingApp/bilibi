# Schedule definitions, review and occurrence claims

Schedule holds installation-global recurrence definitions, the operator's
decision about them, and the durable claim that hands exactly one Queue job to
each intended occurrence. Definitions, fingerprint review, suppression and the
cron/zone arithmetic are Q3; occurrence claiming, the Queue insertion, overlap
leases and run history are Q4; the operator board at `/system/schedule` and
history retention are Q5. Work itself runs on the Queue, never inline.

## Proposed per-tenant reach (issue #138)

**Status: proposed; this section defines the intended contract, not behavior available in the current build.** The full decision is [ADR 17](../../../docs/architecture/decisions/17-per-tenant-scheduled-work.md).

A definition will declare its reach as either `Reach::Installation` or `Reach::EachTenant`. Existing installation-wide behavior remains the default. Reach is material to the definition fingerprint, so changing it makes the operator's review stale and stops queued work until the new fingerprint is reviewed and enabled.

An `EachTenant` occurrence is claimed once by the scheduler in the operator tenant. A Platform-owned fan-out then creates one durable outcome per tenant considered and queues one job in each eligible tenant. The platform operator tenant is explicitly included alongside customer tenants. Enumeration includes active and suspended, undeleted tenants in ID order, in batches of 200; suspended tenants are recorded as `skipped`, and active tenants whose Authz decision explicitly denies the applicable execution capability are `not_granted`. The operator tenant uses platform `EXECUTE`; customer tenants use tenant `TENANT_EXECUTE`. The Schedule vocabulary declares both capabilities, and the `scheduler` system principal's declared capability set includes both `EXECUTE` and `TENANT_EXECUTE`; `EXECUTE` remains platform-only. Authz refuses undeclared system-principal capabilities before checking grants ([`apps/base/authz/src/lib.rs`](../authz/src/lib.rs)). Provisioning grants neither capability by default.

Fan-out is resumable and idempotent by `(occurrence_id, tenant_id)`. In each tenant's audited `MutationBoundary`, Schedule inserts the unique tenant-run row and Queue inserts the child job in the same transaction: scheduled system jobs use `Queue::enqueue_system`, while tenant-user manual jobs use `Queue::enqueue_for`. On a uniqueness conflict, it locks/reads the existing row and reconciles the linked job through that tenant's scoped Queue status lookup: retain a nonterminal job, map `completed` to `succeeded`, map `cancelled`/`discarded` to `failed`, and never enqueue again. A missing job is `failed` with `linked_job_missing`; automatic replacement is refused. Each Schedule worker ID has one fixed Queue capability binding; Queue checks that binding before handler invocation and rechecks the same key and identity inside `Execution::begin`.

The existing `bilibi_schedule::EXECUTE` capability is platform-scoped and cannot authorize `scheduler` in customer tenants; the proposed tenant-audience capability is `bilibi_schedule::TENANT_EXECUTE`. Schedule keeps its existing owner worker ID and handler registration. Queue adds a typed actor-and-audience capability policy for that binding: it selects one exact key from actor kind and Tenancy's sealed operator/customer classification (system or operator user in the operator tenant use `EXECUTE`; system in customer tenants use `TENANT_EXECUTE`; tenant users use `admin.schedule.tenant.execute`). Queue persists that selected key with the job, checks it before handler invocation, and rechecks the exact same key inside `Execution::begin`. Authz makes the decision, including direct-deny precedence and decision logging. Request arguments cannot choose the policy slot or capability, and revoked grants fail closed. This is a Queue contract change; ADR 17 is the canonical policy specification.

Tenant enumeration belongs to Tenancy, not Schedule or Domain SQL. The proposed `Tenants::scheduled_page(scope, after_id, through_id, limit)` contract uses a dedicated once-bound `ScheduleTenantAuthorizer` implemented by Authz. It checks that the sealed caller is the live `scheduler` principal in the active platform-operator tenant and explicitly grants `admin.tenancy.schedule.fan_out` (platform audience) there; it does not create an `OperatorProof`, which system principals cannot hold. Pages contain only tenant ID, active/suspended status and the operator marker, exclude deleted tenants, use an exclusive `after_id` plus a fixed high-water `through_id`, and reject limits outside `1..=200`. The first page captures the high-water mark. Fan-out advances its cursor only after all targets in a page have durable outcomes; compare-and-set cursor advancement and unique outcome keys make concurrent retries converge. `SystemScope` alone grants no enumeration authority.

For each tenant job, Schedule must validate a durable child-run record linked to that exact Queue job. The proposed tenant manual path stores a tenant-local child row with `trigger=manual`, the requester, tenant, fingerprint and exact Queue job ID, committed atomically with the job; reserved schedule metadata carries that child ID for `Schedule::run` to validate. The operator-tenant occurrence linked to the fan-out job is not sufficient. Installation jobs keep their current occurrence-row validation path; see ADR 17 for the proposed metadata and row shape.

The operator fan-out ledger records the target set, suspended/denied outcomes where no tenant write is allowed, and durable global-start markers. The tenant-local child-run row is authoritative for job linkage, execution status, requester, and per-tenant overlap; `base_schedule_runs` remains per-attempt evidence and gains `tenant_id`. Aggregate `queued` covers available, scheduled and retryable Queue states; `running` covers executing; `completed` becomes `succeeded`; terminal `cancelled`/`discarded` and missing linked jobs become `failed` with a bounded code; an overlap refusal is a tenant-local `skipped` row. A retryable failed attempt does not count as aggregate failure. `not_granted` records an active tenant with an explicit denied grant decision in the operator ledger. Counts project one execution outcome per tenant-run and use ledger outcomes for suspended and denied tenants. The existing occurrence overlap index remains global for installation claims; `EachTenant` uses a separate partial unique key `(definition_key, tenant_id)` for unfinished `Overlap::Forbid` child runs.

For `Reach::EachTenant`, forbid-overlap is per `(definition key, tenant)`: a slow tenant is skipped for the next occurrence while other tenants proceed. The global operator review, enable, disable, pause and resume apply to every tenant. Disabling, pausing or materially changing a definition prevents unstarted work from beginning. A tenant created after fan-out joins on the next occurrence; missed occurrences continue to coalesce once, with no backfill.

Operator Run now fans out the definition once. Tenant Run now, proposed behind `admin.schedule.tenant.execute`, is a separate user-authorized action for one tenant and queues work as the requesting person; it does not confer or create the system principal's scheduled-work grant. Schedule generates the manual request ID server-side from the authenticated sealed scope; the browser cannot provide it or override tenant/actor identity. Tenant-scoped access to definitions and run history uses this same Schedule board/home, not a separate tenant page; authorization determines which records and actions each actor can see, including the proposed `admin.schedule.tenant.view` access. This follows [`DESIGN.md`](../../../DESIGN.md) (“One thing one door, one door many keys”). The board reports queued, running, succeeded, failed, skipped and not-granted counts per occurrence and allows authorized tenant filtering.

Before child handler code runs, Schedule opens an operator-tenant transaction with the scheduler principal rehydrated from Tenancy's operator marker, authorizes platform `EXECUTE`, and checks the current review, fingerprint, enabled and pause state. It takes the same `schedule:{key}` advisory transaction lock used by review and state changes, then records the child as `started` in the operator occurrence ledger before committing and releasing the lock. That durable transition is the start decision: if a disable or definition change commits first, stale work cannot start; if the child transition commits first, it may finish. Schedule then opens the tenant-local execution transaction, where Queue rechecks the worker-bound capability and Schedule verifies the child-run link before invoking owner code. The lock order is operator eligibility transaction, release, then tenant transaction; the two tenant transactions are never held together and no child scope is converted into an operator scope.

Tenant-local wall-clock schedules, per-tenant cron or pause settings, and per-company fan-out are out of scope. A worker that needs business-day arithmetic uses the work-calendar contract when available; the scheduler does not add tenant-local time semantics.

The public `Definition` shape change is source-breaking for struct-literal callers: every in-repository caller must select `Installation` or `EachTenant`, and affected official Domains require companion PRs. Do not add a compatibility field, alias or shim. The schedule migration identifier must be the next unused ID: `base-schedule-003-schedule-timing` already exists, so the issue's proposed `base-schedule-003-tenant-runs` cannot be used.

Owners contribute a `Definition` to `base/schedule.definitions` with
`Contribution::new`; the contribution id is the durable key
(`^[a-z0-9][a-z0-9._/-]{0,254}$`) and the provenance module is the owner. A
definition names a five-field cron, an IANA zone, a task name, a Queue `worker`
id, bounded arguments, an overlap policy and the one misfire policy
(`Coalesce`). The worker must be a `base/queue.workers` entry owned by the same
module and must accept the arguments; boot refuses unknown zones, macros
(`@daily`), seconds/year fields, `L`/`W`/`#`/`?`, expressions that can never
match a date (`0 0 31 2 *`), unbounded arguments, the reserved
`__bilibi_schedule__` argument and an owner route that is not an internal path.
A restricted day-of-month and a restricted weekday must both match.

## Scheduled workers: use `worker::validate` and `Schedule::run`

The claim enqueues the definition's arguments plus bounded claim metadata under
`RESERVED_ARGUMENT`. A scheduled worker therefore must:

* validate through `bilibi_schedule::worker::validate(args, mine)`, so the
  owner's validator sees only its own arguments and the metadata is kept. Boot
  refuses a definition whose worker drops that metadata, so a hand-rolled
  validator cannot get as far as a refused claim;
* bind its handler (`base/queue.handlers`) to the Schedule typed capability
  policy: installation-system and operator-user `EXECUTE`, customer-system
  `TENANT_EXECUTE`, and tenant-user manual `admin.schedule.tenant.execute`; Queue selects and stores
  one key from the rehydrated actor kind and Tenancy classification;
* run its `Handler::perform` body through `Schedule::run(execution, args, |args|
  ...)`. Before the owner's code starts, the global eligibility gate must mark
  the child started under the per-key advisory lock; the definition must still
  exist with the same fingerprint, be enabled and unsuppressed, and the durable
  occurrence/child row must be bound to this job; otherwise the
  occurrence is failed and the job cancelled with `schedule_removed`,
  `schedule_changed`, `schedule_unreviewed`, `schedule_disabled`,
  `schedule_suppressed` or `invalid_schedule_occurrence`. Pausing, disabling or
  materially changing a definition therefore cancels work that has not begun.
  Each attempt is a `base_schedule_runs` row; a job without metadata is
  cancelled with `missing_schedule_metadata`.

The owner still makes its business effect idempotent: Queue is at least once,
and the occurrence claim prevents duplicate enqueue, not duplicate effects after
a worker failure.

## Who runs the work

A recurrence has no user behind it. For `Reach::Installation`, the scheduler
enqueues as the named system principal `scheduler` (`PRINCIPAL`) in the
platform operator's tenant (`platform_operator`); without an operator nothing
is claimed. Declaration grants nothing: an operator must grant the principal `EXECUTE` in that tenant
(`Grants::set_system(admin, "scheduler", EXECUTE, Some(true))`, capability
`admin.authz.system-principal.grant`). Queue re-proves the principal, its
declaration and that grant on every attempt (`Queue::enqueue_system`,
`Execution::identity`); an ungranted principal's jobs cancel with
`actor_refused`. Run-now (`Schedule::run_now`) queues a `manual` occurrence as
the operator who asked, after `authorize_operator(EXECUTE)`, so that job runs as
that user and its run row names them.

## Claims, coalescing and overlap

`Scheduler` polls every 15 s in each app process (`bilibi-web run` starts it
beside the Queue executor; `Scheduler::poll(now)` takes the clock so any moment
can be replayed). Each poll first settles unfinished occurrences whose job
Queue already reports terminal (`completed` is `succeeded`, cancelled,
discarded or missing is `failed`, and the lease is released), then for every
definition computes `previous(now)` and claims it when it is newer than the
key's watermark (the latest intended instant ever claimed). Downtime
**coalesces**: only the latest missed occurrence is considered, the instants in
between are never claimed. Both passes of a DST fold are real occurrences; a
gap's missing minute is dropped.

`Schedule::claim` is one audited transaction under `lock(key)`: eligibility,
reconciliation of that key, the `(key, intended_at, trigger)` uniqueness check,
the forbid-overlap check, the Queue job (`base/queue.job`) and the occurrence
row (`base/schedule.occurrence`) commit or roll back together, so two app
processes polling the same instant produce exactly one occurrence and one job;
the loser sees `AlreadyClaimed`. `Overlap::Forbid` sets `overlap_key` on the
unfinished occurrence (a partial unique index: one active lease per key); a
later occurrence is refused with `Overlap` and recorded once as a `skipped` run
until the earlier job succeeds, is cancelled or exhausts its retries. Refusals
write nothing.

`base_schedule_runs` is best-effort operational evidence (running, succeeded,
failed, skipped; trigger and who asked for a manual run); recording failure is
logged and never changes the owner's outcome. `Schedule::runs` reads it
redacted with `admin.system.schedule.view`.

## Review and suppression

Every definition is `Unreviewed` until an operator reviews its **fingerprint**
(`Registered::fingerprint`): SHA-256 over key, name, expression, zone, owner,
task name, worker, canonicalised arguments, overlap and misfire. Changing any of
those in code makes a stored review stale, so a redeploy can never silently run
changed work; the owner route is display-only and not part of it. Review state
is `Unreviewed`, `Enabled` or `Disabled` for the current fingerprint.
Suppression is separate: a row in `base_schedule_suppressions` means paused and
deleting it means resumed. Rows are keyed by text, not foreign keys, so a removed
or renamed definition leaves its row as orphan evidence and a new key can never
inherit a decision.

`Schedule::review`, `suppress` and `resume` take the sealed `Scope`, require the
persistent operator identity plus `admin.system.schedule.manage`
(`authorize_operator`), and write the state and its Audit evidence in one
`MutationTransaction` under a per-key advisory lock (`schedule::lock`). A
repeated `suppress`/`resume` changes nothing and records nothing. `tasks` needs
`admin.system.schedule.view` and returns redacted facts (no arguments).
`admin.system.schedule.execute` gates run-now.

`Schedule::eligibility` is checked on a claim and by the global start gate for
each EachTenant child. The start gate takes `lock`, verifies the current
reviewed fingerprint and enabled/unsuppressed state, and commits a durable
child start marker. A retry with that same marker continues already-started
work after a later disable; a child without a marker must pass the gate before
owner code. Any database error is an `Err`, never `Eligible`.

## Changing a task's time

Operators with Schedule manage permission can select a fixed-time task's schedule, choose a time, and save it in the zone shown beside the picker. For example, a daily task at 03:17 UTC opens at 11:17 in Asia/Kuala_Lumpur; saving 09:30 stores `30 9 * * *` with `Asia/Kuala_Lumpur`.

- The picker uses the account's top-bar clock: company, browser-local, or UTC. The browser reports its IANA zone before local editing becomes available. Changing the top-bar clock while editing leaves the open draft and its displayed zone together.
- The saved zone stays with the setting; daylight saving follows that zone. Gaps skip the missing occurrence and folds can run twice, using the recurrence rules below.
- Only schedules with one fixed hour and minute have a picker. Intervals and multiple daily times keep their cadence. Calendar rules convert with the clock where one cron can represent them; a month-boundary conversion that cannot be represented keeps the saved-zone read view.
- **Reset to code default** asks for confirmation and restores the contribution's time and zone. Saving and resetting preserve the task's enabled/disabled and paused state; they do not enable an unreviewed task.

`Schedule::set_time` and `reset_time` require the sealed operator scope and recheck `MANAGE` inside `MutationBoundary`, under the same per-key lock used by claims. The installation-wide `base_schedule_timings` row holds the cron, IANA zone, contributed fingerprint and revision; Audit records the operator, old value and new value as `schedule.time.changed` or `schedule.time.reset`. The submitted timing version refuses stale edits. These writes use the board's existing declared pending allowance until Schedule has operation contracts.

A new contributed fingerprint ignores both the old override and its review. A timing revision also changes queued-job metadata, so work queued at an earlier time is cancelled before the owner runs. Reset retains a revision tombstone, preventing old default-time jobs from becoming valid again. Dispatch and diagnostics use the effective recurrence; a changed setting starts with future occurrences rather than immediately catching up an occurrence from before the change. `Task.expression` and `Task.timezone` remain the code defaults; `clock_expression`, `clock_timezone`, `timing_version` and `time_overridden` describe the effective setting.

## Time

`cron` knows only civil minutes. `recurrence` is the only place a zone matters,
and every function takes the instant as an argument:

* **Fold** (clocks repeat): both instants are real. `previous(now)` is the latest
  that has happened, so a poll before the repeat gets the first and a poll after
  it gets the second; `next(now)` is the earliest still strictly ahead.
* **Gap** (clocks skip): the occurrence does not exist and is dropped, not
  shifted. `next` resumes from the first wall time after the gap; `previous`
  searches before it.
* `previous` follows the exact rule. `next` is display only and also considers the repeat that is still ahead while the first pass of a fold is running.

The board's Schedule column reads each cron in plain words in the viewer's top-bar zone, converted at the zone's offset now ("Every day at 11:17 PM EDT"). The words name that zone, because a daylight-saving change moves a fixed clock time. A cadence with no clock time ("Every 5 minutes") has no zone to name. When no single clock time reads right in the viewer's zone, the words stay in the saved zone and say so ("Every day at 3:17 AM (UTC; shown in saved time zone)"). Read-only schedules keep the exact saved cron and zone in the tooltip. The words do not change dispatch: the effective cron and saved zone decide when the task runs, and Next run is the real next instant in the viewer's zone.

Replay any moment with `cargo run -p bilibi-schedule --example schedule-walkthrough -- dst [key instant]`
(no database). The other modes use a disposable database
(`BILIBI_DISPOSABLE_SCHEDULE_DEMO=yes`, `DATABASE_URL`, after `bilibi-web migrate`;
they install the walkthrough fixture); they link a private worker, handler and five definitions that
production does not have:

```sh
export BILIBI_DISPOSABLE_SCHEDULE_DEMO=yes
cargo run -p bilibi-schedule --example schedule-walkthrough -- timing    # clock settings, stale work, DST, authorization, audit
cargo run -p bilibi-schedule --example schedule-walkthrough -- review    # Q3: review, suppression, audit
cargo run -p bilibi-schedule --example schedule-walkthrough -- prepare   # operator tenant, enable, grant scheduler
cargo run -p bilibi-schedule --example schedule-walkthrough -- coalesce  # one claim after five missed hours
cargo run -p bilibi-schedule --example schedule-walkthrough -- race 2026-06-03T10:00:00Z   # two processes, one row
cargo run -p bilibi-schedule --example schedule-walkthrough -- overlap   # lease refuses, skipped run, cancel releases
cargo run -p bilibi-schedule --example schedule-walkthrough -- execute 2026-06-04T12:00:10Z # scheduler/user/stale review
cargo run -p bilibi-schedule --example schedule-walkthrough -- worker    # live, real clock; run it in two terminals
cargo run -p bilibi-schedule --example schedule-walkthrough -- inspect
```

`worker` claims the every-minute definitions and executes them; a second
`worker` process (set `SCHEDULE_DEMO_KEEP=1` so it does not reset the first)
shows that each minute is claimed by exactly one of them, and the 75-second
task's next minute is refused for overlap and recorded as skipped while it runs.

## Operator board (`/system/schedule`)

The board is `src/web.rs`, a framed live
page behind `OperatorCapability(admin.system.schedule.view)` with a System →
Schedule menu entry. Its read model is `src/board.rs`: tasks with their latest
run (filtered and sorted in memory over the bounded definition
list), an exact run-history page filtered, ordered and paged in PostgreSQL
(`Schedule::run_page`; the sort is an enum, never a client identifier), and
diagnostics that report the local scheduler loop's heartbeat, Queue and run
recorder reads and due work independently (`Unavailable`/`Unknown`, never a
healthy default). The URL uses these names (`tab`, `task_search`,
`task_status`, `task_sort`, `task_dir`, `run_search`, `run_status`,
`start_date`, `end_date`, `run_sort`, `run_dir`, `page`, `page_size`).

`view` reads, `execute` queues Run now, `manage` enables, disables, pauses,
resumes and changes retention. The page shows controls from the actor's
effective capabilities, but every command first re-asks
`authorize_operator` for its capability and the Schedule API checks it again
inside the transaction; a refusal flashes "You do not have permission for that
action." and redraws the controls without clearing the page. Enable, Pause and
Disable confirm through the shared `ConfirmDialog`, which holds the task it
describes; the confirmation acts on that held task, never on a key the client
sends. History dates bound calendar days in the zone the Started column shows: company, browser-local or UTC according to the saved display mode. The shared `BrowserTimeZone` hook reports the local zone and display-mode changes; the display policy validates it and UTC is the fallback.

The board's tables use independent shared `ListColumns`/`FlexTable` arrangements: show/hide and reorder columns, row-height steps, Compact/Normal and Reset. History alone has pagination (25/50/100 rows); tasks remain unpaged. Facts a cell shows on a second line (overlap and misfire policy, last runtime, run runtime and trigger) are optional columns through `ListColumns::with_record_fields`; the default columns do not change.

## Retention and contributed housekeeping

`schedule.history.keep_days` is a global integer setting (default 90, 0–3650,
zero keeps everything), changed from the board's Settings tab
(`board::set_keep_days`, operator + `manage`, audited as
`schedule.retention.changed`). The daily `base/schedule.occurrence-retention`
task (03:37 UTC, forbid overlap) deletes finished occurrences and finished run
rows older than that; unfinished claims and each key's latest scheduled claim
(the watermark) are never deleted. The same daily task prunes both.

Base Session contributes `base/session-expiry` (every five minutes), which
deletes sessions past their absolute or idle expiry (`Sessions::prune_expired`).
Both tasks follow the scheduled-worker contract above (`worker::validate`,
`Schedule::run`, a handler using the typed Schedule capability policy). Like
every definition they are unreviewed until an operator enables them on the
board, and installation-reach tasks run only after `scheduler` is granted
`EXECUTE` in the operator tenant. Copy
`src/retention.rs` or `apps/base/session/src/expiry.rs` for a new housekeeping
task rather than starting a Tokio interval of your own.
