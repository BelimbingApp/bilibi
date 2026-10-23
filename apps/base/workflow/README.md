# Workflow status kernel

Workflow owns status configuration, edges, subject bindings, append-only
history, and immutable versioned process runs with their lifecycle and
worker leases. Its facade accepts a sealed
`bilibi_tenancy::Scope`, a subject kind (public key or alias) and the owner's
record ID. It never queries an owner's table: the owner proves identity, tenant
and current status through its `SubjectAdapter`. Committed transitions are
sent through the durable outbox described below.

## Contributions

An owner declares `base/workflow` as a module dependency and submits immutable
`bilibi_workflow::Contribution` values to the `CONTRIBUTIONS` consumer through
`bilibi_module_registry::Contribution::new` with source provenance:

- `Subject { key, aliases, adapter }`: one per owned record type.
- `Guard { key, aliases, adapter }` and `Action { key, aliases, adapter }`:
  owner hooks, referenced by key or alias from an edge.
- `Flow(Flow { code, label, subject, active, statuses, edges })`: default
  statuses and edges. Each edge may name a capability, guard and action.
- `HumanAction(HumanAction { key, label, subject, capability, executor,
  handler })`: an action a signed-in person performs on an owned subject (see
  Human actions).

Boot rejects a contributor that does not depend on `base/workflow`, invalid or
duplicate keys (lowercase dotted segments, at most 255 bytes), duplicate aliases,
a flow whose subject belongs to another module, two flows on one subject, edges
whose statuses are missing, whose `settings`/`metadata` are not JSON objects,
whose capability is not in the Authz vocabulary, or whose guard/action is not
owned by the same module. A human action must have a valid key unique per
subject, a label, a vocabulary capability and a subject its own module owns.
Contributions perform no I/O.

## Versioned processes

`Process { key, version, subject, adapter, steps }` contributes a durable run
graph for one registered subject type. Versions are positive integers, step
keys are unique, dependencies must name a step in the same graph, and cycles
are rejected at registry build time. Each process key keeps one owner and
subject across its installed versions.

The registry compiles every contribution into an immutable definition and a
SHA-256 fingerprint over its key, version and ordered step contracts. Step
field order is fixed, object keys in input and metadata are sorted recursively,
and array order is preserved. Floating-point values are rejected so a
serializer change cannot silently alter the fingerprint. Change the version
when changing any step contract, and retain old versions while their runs need
them.

The first authorized start inserts the key/version fingerprint into
`base_workflow_process_versions`; a database trigger refuses updates and
deletes. `Engine::start_run` and `start_run_in` ask both the subject adapter
and process adapter to authorize `StartRun` inside the audited transaction.
They namespace the caller's idempotency key by tenant and definition, create
the run, materialize its complete graph and append its initial events
atomically. Repeating a start returns the same run and work-item IDs when the
subject, version, fingerprint, input, priority and correlation key match; a
changed request conflicts.

`Engine::get_run` and `run_events` re-authenticate the scope, prove the live
subject, ask both adapters to authorize `ReadRun`, then check the saved
fingerprint and every persisted step and dependency before returning anything.
Reads take no row locks and graph reads are bounded by the installed definition
size. A changed contribution under a saved version conflicts; a malformed
saved graph is refused.

## Run lifecycle

`Engine::update_run(scope, run_id, command)` applies one `RunCommand`;
`update_run_in(&mut MutationTransaction, ...)` commits it with the owner's
other business writes, and a refusal poisons that whole transaction. Each
command re-authenticates the scope, locks the subject, then the run, then its
work items, and asks both adapters to authorize its own operation
(`ReconcileRun`, `CompleteWork`, `SignalRun`, `PauseRun`, `ResumeRun`,
`SupersedeRun`, `WaiveWork`, `BlockWork`). `ReadRun` never grants any of them.

- `Complete { step_key, completion }` finishes an `available`, due item of a
  running run. `completion.expected_version` and `executor_key` must match the
  item, so a stale or misrouted completion conflicts. The outcome, output and
  `result_ref` are saved once; a completed item never changes again.
- A dependency accepts the outcomes it lists (default `completed`). An `All`
  step needs every dependency accepted; an `Any` step needs one. When no
  acceptable outcome can still arrive, the step is blocked with an event, and
  that block propagates downstream.
- `Signal { name, payload, idempotency_key }` marks every unfinished item that
  requires `name`. Repeating the same key and payload is a no-op; the same key
  with another signal or payload conflicts. Signals need a running run.
- A released step starts its timer at the later of the run's `available_at`
  and now, plus `delay_seconds`, and becomes `available` once due. `Reconcile`
  expires dead leases (see below) and releases due timers; the periodic
  every-tenant sweep is W6, so until then a claim or `Reconcile` does it.
- `Pause { reason }` and `Resume` toggle a running run; repeating either is a
  no-op. A paused run refuses completion and signals, and resuming re-settles
  the graph from its saved facts.
- `Supersede { reason }` blocks every unfinished item with cause
  `process_superseded`, keeps terminal work as recorded, and finishes the run.
  A live worker lease refuses it; a finished run returns unchanged.
- `Waive { step_key, reason, outcome }` and `Block { step_key, reason }` are
  the owner or operator path: they finish one unfinished item (pending,
  available or leased, even under a live lease) with the reason as its error,
  then settle the graph. Terminal work returns unchanged.

Every write appends to the run's event log with a contiguous per-run
`sequence`, and the run finishes as `failed`, `blocked` or `completed` (in
that precedence) once every item is terminal.

## Worker leases

`Engine::claim_work(scope, worker, ClaimOptions { lease_seconds, definition_key,
executor_keys, run_ids })` leases one due item to `worker` and returns
`Claimed { run, work_item, claim }`, or `None`. Candidates are chosen without
row locks from running runs saved under an installed key/version/fingerprint,
ordered by run priority, work priority, availability and ID; the chosen run is
then locked in the kernel order and both adapters authorize `ClaimWork`. The
locked run is settled first, so due pending work and expired leases are
candidates too. A candidate refused under its lock is skipped and the claim
gives up after eight, so retained unsupported runs never starve supported work.
An empty `executor_keys` or `run_ids` claims nothing. Each claim increments
`attempts` and appends `work.claimed` with the worker and attempt.

`Claim { run_id, work_item_id, lease_token }` is the worker's durable
credential: keep it with the in-flight work so a restarted worker can still
finish it. Run and work facts, audit changes and events never carry the token,
and `Claim`'s `Debug` hides it. `Engine::update_work(scope, &claim, command)`
and `update_work_in(&mut MutationTransaction, ...)` apply one `WorkCommand`;
use the `_in` form to commit the owner's business writes with the completion.

- `Heartbeat { lease_seconds }` (`HeartbeatWork`) extends a live lease to now
  plus `lease_seconds`.
- `Complete { outcome, output, result_ref }` (`CompleteWork`) finishes the item
  as `completed`; it is allowed on a paused run, which then stays paused.
- `Fail { error, retryable, category, retry_at }` (`FailWork`) returns
  retryable work with attempts left to `pending` at `retry_at` (UTC whole
  seconds, the caller's backoff; `None` is now) with `work.retry_scheduled`;
  otherwise the item fails with output `{"failure": {retryable, category}}`.
- `Block { reason, output, result_ref }` (`BlockClaimedWork`) finishes it as
  `blocked`.

Each command needs the item to be `leased` under the same token, compared in
constant time, with an unexpired lease; anything else conflicts. Complete,
Fail and Block on an item that is already terminal return it unchanged, so a
worker may repeat its last call; Heartbeat always needs the live lease.

Expiry is fenced on the saved rows, so it survives any restart. Whenever a
running run is settled (any lifecycle command or claim), a lease whose expiry
has passed loses its owner and token and its `version` increments: the final
attempt fails the item (`work.lease_expired_failed`), otherwise it returns to
the queue at once (`work.lease_expired_requeued`). The old worker's token and
any `Complete` read before expiry then conflict. A paused run keeps its
leases until resumed. A live lease is never touched.

`Engine::initialize` is the explicit insert-only seed for flows, statuses and
edges, run by a trusted admin or seed adapter holding
`admin.workflow.definition.manage`. Existing operator configuration always
wins, including inactive flags; boot never calls it and a changed contribution
never overwrites rows.

A production owner seeds its own flow instead of waiting for an operator to run
`initialize`: contribute a versioned production `Seed` whose action is
`SeedAction::Service { service: bilibi_workflow::FLOW_SEEDS, operation: "<flow
code>" }`, from a module that depends on `base/workflow`. The host's explicit
`seed` command inserts that one flow with the same insert-only rule, once per
seed version; give a new flow its own version. An unknown flow code refuses
the seed. Seed inserts are not audit rows, like every production seed. The
`owner` example's `seed` command shows it on a migrated disposable database.

## Human actions

Use `Engine::available_actions` and `execute_action`, never a hand-rolled
button handler that writes the owner row and completes work itself: the gate
is what makes a double-click, a retry or a stale page safe.

`available_actions(scope, kind, id)` returns the subject version and every
contributed action whose capability the signed-in actor holds. An action with
an `executor` appears once per due, `available` work item of that executor key
in a running run of the subject, carrying the run, item and work version.
Availability is not authorization: owner eligibility is proved only when the
action executes.

The page echoes an `ActionRequest { action_key, idempotency_key,
expected_subject_version, payload, process_run_id, work_item_id,
expected_work_version }` (`ActionRequest::new(...).for_entry(&entry)`); it
never carries the actor, tenant or capability. The payload is a JSON object or
array without floats, and the request is at most 65,536 encoded bytes. A
work-bound action needs the whole binding; any other action refuses one.

`execute_action(scope, kind, id, request)`, or `execute_action_in(&mut
MutationTransaction, ...)` inside the owner's transaction, re-authenticates
the scope, locks the subject under the owner's `ExecuteAction` policy and
checks the capability. It then locks the tenant/idempotency-key row:

- A saved key replays the first result (`replayed: true`) without running
  anything and before any version check, because the first execution
  legitimately changed the subject. The SHA-256 intent digest over subject,
  actor, action, work binding and canonical payload must match, compared in
  constant time; another subject, actor, action or payload under the same key
  conflicts.
- A new key compares `expected_subject_version`, proves the bound run belongs
  to this subject and locks it with its graph, and checks the item is due,
  `available`, of the action's executor and at `expected_work_version`. It
  then inserts the request row, runs `HumanActionHandler::handle`, completes
  the bound item with the handler's `HumanOutcome` (the `work.completed` event
  names the request and actor) and saves the result.

Everything commits in one audited transaction, so a committed request always
carries its result and any refusal, handler error or cancellation rolls back
the owner write, request row and work completion together. Concurrent
duplicates serialize on the subject lock: one executes and the other replays.
The same key raced on two subjects of a tenant serializes on the key's unique
constraint, and the loser conflicts. The handler must take the actor from the
scope, never from the payload, and must perform database effects only.

## Owner contract

All callbacks receive the kernel's `tokio_postgres::Transaction`. They must not
open or commit another transaction and must perform database effects only.

- `SubjectAdapter::load(tx, scope, id, Load::Read | Load::Lock)` scopes the
  query by tenant and, for `Lock`, locks and rereads the row with `FOR UPDATE`.
  It returns a `Subject` holding only proven id, tenant, status, version and
  plain JSON facts.
- `SubjectAdapter::authorize(tx, scope, subject, operation)` enforces owner
  policy for `Read`, `Transition`, `RecordInitial`, `RecordComment`, `Adopt`
  and `ExecuteAction`, even on edges without a capability.
- `SubjectAdapter::persist(tx, scope, subject, to, request)` writes the proven
  status from the locked status, increments the version and returns every
  change for audit capture. The kernel rereads the row and refuses a persist
  that left a different status or an unchanged version.
- `Guard::check` runs before persist; `Action::execute` runs after it with the
  new status. Any error or cancellation rolls back status, hook effects,
  binding, history and the audit facts together.

Use `Engine::transition(scope, kind, id, to, request)` for a standalone
transition, or `transition_in(&mut MutationTransaction, ...)` when the owner's
other business writes must commit with the transition. The engine runs inside
the owner's audited `mutate`, so a guard denial after an earlier owner write
poisons the whole transaction and the outer commit fails. The mutation context
must carry the same tenant and user as the scope; the scope is re-authenticated
inside the transaction. Lock order is owner row, then flow and status
definitions (`FOR SHARE`), then the subject binding (`FOR UPDATE`).

`Request` accepts `expected_version`, `comment` (≤ 16,000 bytes),
`comment_tag` (≤ 255 bytes), JSON arrays `assignees`/`attachments`, and JSON
objects `metadata`/`input`, at most 65,536 encoded bytes in total. `metadata`
reserves `_workflow` for sealed provenance. A mismatched `expected_version`
conflicts before any hook runs.

`available(scope, kind, id)` lists active edges between active statuses whose
capability the actor holds; guards run only during execution, so availability is
not authorization. `record(scope, kind, id, Operation::RecordInitial, request)`
refuses a subject that already has history; `RecordComment` appends a
same-status fact and requires a non-empty comment. `adopt(scope, kind, id)` locks
the subject under the owner's `Adopt` policy and binds it to its flow, inactive
flows included, without rewriting history. Rebinding a subject to another tenant
or owner conflicts.

History rows are append-only (a trigger refuses UPDATE and DELETE). A transition
fact's `tat` is the whole seconds since the subject's latest earlier fact that is
not a `record_comment`; it is null on a first transition and on recorded
facts. `history(scope, kind, id, after, limit)` returns a keyset page ordered by
id with `limit` in 1..=500 and `next_cursor` when more rows exist; each fact's
`transitioned_at` is RFC 3339 UTC, so a page renders it through its display
context without depending on a session time zone.

`pending_work(scope, &PendingQuery)` is the bounded worklist (Bilimbi
`Workflow.pending_work/2`): at most `limit` (1..=500) due `available` items of
running runs in the actor's tenant, narrowed by `subject(kind, id)`, `run_id`,
`definition_key` or `executor_keys` (an empty set lists nothing), ordered by
priority, availability and id with a keyset `next_cursor`. Candidates are chosen
without locks and each run is then read through its owner proof, so owner policy
may hide some of a page while the cursor still advances; it is a worklist, not an
owner workbench count, and execution repeats every check.

## Running proof (no unit tests)

Use an explicitly disposable PostgreSQL database, never the normal development
database. Migrate it, then run the private owner example, which installs the
walkthrough fixture accounts itself.
It links a reference subject, guard, action and flow that production never
contributes (`examples/reference/mod.rs`, shared with the Web fixture
executable `apps/web/examples/w7-workflow-reference.rs`).

```sh
export DATABASE_URL='postgres://.../disposable'
export BILIBI_DISPOSABLE_WORKFLOW_DEMO=yes
cargo run -p bilibi-web -- migrate
cargo run -p bilibi-workflow --example owner -- init
cargo run -p bilibi-workflow --example owner -- available 4 1
cargo run -p bilibi-workflow --example owner -- approve 4 1
cargo run -p bilibi-workflow --example owner -- history 4 1
```

To demonstrate versioned starts and reads in the same fresh disposable
database, run the process commands in order:

```sh
cargo run -p bilibi-workflow --example owner -- init
cargo run -p bilibi-workflow --example owner -- start-v1 4 1
cargo run -p bilibi-workflow --example owner -- start-v1 4 1
cargo run -p bilibi-workflow --example owner -- drift 4 1
cargo run -p bilibi-workflow --example owner -- run-drift 4 1
cargo run -p bilibi-workflow --example owner -- start 4 1
cargo run -p bilibi-workflow --example owner -- start-changed 4 1
cargo run -p bilibi-workflow --example owner -- run 4 3
cargo run -p bilibi-workflow --example owner -- events 4 3
```

The first repeated v1 start returns the same run and work IDs. `drift` and
`run-drift` refuse the changed graph under saved v1, and `start-changed`
refuses a changed input under the v2 idempotency key. A fresh database assigns
the v1 and v2 run IDs `1` and `3` in this sequence because the replay consumes
an unused database sequence value; use IDs printed by `start` if other runs
already exist.

To demonstrate the run lifecycle, start the six-step `reference.lifecycle`
graph and drive it (on a fresh database the run ID is `1`; use the ID `start`
prints otherwise):

```sh
cargo run -p bilibi-workflow --example owner -- start-lifecycle 4 1
cargo run -p bilibi-workflow --example owner -- complete 4 1 first accepted
cargo run -p bilibi-workflow --example owner -- pause 4 1 "Operator review"
cargo run -p bilibi-workflow --example owner -- complete 4 1 second
cargo run -p bilibi-workflow --example owner -- resume 4 1
cargo run -p bilibi-workflow --example owner -- complete 4 1 second
cargo run -p bilibi-workflow --example owner -- complete 4 1 any
cargo run -p bilibi-workflow --example owner -- complete 4 1 all
cargo run -p bilibi-workflow --example owner -- signal 4 1 review.approved
cargo run -p bilibi-workflow --example owner -- complete 4 1 signal
cargo run -p bilibi-workflow --example owner -- reconcile 4 1
cargo run -p bilibi-workflow --example owner -- events 4 1
```

Completing `first` releases `any` while `all` waits for `second`. The paused
run refuses `second`, and after `resume` the completed `first` keeps its
outcome and output. `signal` releases the approval step; repeating it is a
no-op and a different payload under the same key (`signal 4 1 review.approved
'{"approved":false}' approval:1`) conflicts. Completing `signal` starts the
two-second timer on `delayed`; `reconcile` releases it once due, after which
`complete 4 1 delayed` finishes the run. `complete-stale` and
`complete-executor-mismatch` conflict. On a second record, `complete 4 <run>
first rejected` blocks `all`, `signal` and `delayed`; `complete-outer-fail
4 <run> second` completes inside an owner transaction that rolls back, leaving
`second` available; `supersede 4 <run>` blocks the remaining work.

To demonstrate worker leases and their recovery after a crash, start the
four-step `reference.worker` graph (`fetch` allows two attempts; `render`,
then `publish`, depend on it; `notify` is independent). `claim` prints the
claim JSON and exits, which is the crash: its process ends holding a two-second
lease. Pass that JSON back in the record-ID position of a worker command.

```sh
cargo run -p bilibi-workflow --example owner -- start-worker 4 1
cargo run -p bilibi-workflow --example owner -- claim-race 4 1
sleep 3
cargo run -p bilibi-workflow --example owner -- complete-claimed 4 '<fetch claim>'
cargo run -p bilibi-workflow --example owner -- reconcile 4 1
cargo run -p bilibi-workflow --example owner -- claim 4 1
sleep 3
cargo run -p bilibi-workflow --example owner -- claim 4 1
cargo run -p bilibi-workflow --example owner -- events 4 1
```

`claim-race` claims concurrently as two workers, which lease different items.
After the leases expire the old claim conflicts; `reconcile` requeues `fetch`
(version 2) and fails `notify` on its final attempt. The second `claim` leases
`fetch` attempt 2; once that expires too, the next worker's claim fails it,
blocks `render` and `publish`, finishes the run `failed` and finds nothing
claimable.

On a second record, `claim 4 <run> 30 worker-a reference.fetch` (lease
seconds, worker, executor filter) and then `fail 4 '<claim>'` requeue `fetch`
for its second attempt (`fail 4 '<claim>' final` fails it instead).
`heartbeat 4 '<claim>' 120` extends a lease and `complete-claimed` finishes it;
repeating `complete-claimed` returns the same item while `heartbeat` conflicts.
`waive 4 <run> render "Rendered by hand"` releases `publish`, and
`block-claimed 4 '<claim>' "reason"` blocks a leased item. With `notify` leased,
`supersede` conflicts, `block 4 <run> notify "reason"` finishes it anyway, and
the leaseholder's `complete-claimed` then returns the blocked item.

To demonstrate human actions in a fresh disposable database after `init`:

```sh
cargo run -p bilibi-workflow --example owner -- actions 4 1
cargo run -p bilibi-workflow --example owner -- act 4 1
cargo run -p bilibi-workflow --example owner -- act 4 1
cargo run -p bilibi-workflow --example owner -- act 4 1 reference.acknowledge ack:1 '{"note":"x"}'
cargo run -p bilibi-workflow --example owner -- act-stale 4 1 reference.acknowledge ack:2
cargo run -p bilibi-workflow --example owner -- act-outer-fail 4 1 reference.acknowledge ack:3
cargo run -p bilibi-workflow --example owner -- act-race 4 1 reference.acknowledge race:1 '{"handler_wait":true}'
cargo run -p bilibi-workflow --example owner -- act-race-cross 4 1 2 cross:1
cargo run -p bilibi-workflow --example owner -- start-human 4 3
cargo run -p bilibi-workflow --example owner -- actions 4 3
cargo run -p bilibi-workflow --example owner -- act-work-stale 4 3 reference.review.submit review:stale
cargo run -p bilibi-workflow --example owner -- act-race 4 3 reference.review.submit review:1 '{"handler_wait":true}'
cargo run -p bilibi-workflow --example owner -- events 4 <run>
cargo run -p bilibi-workflow --example owner -- actions 2 1
```

The `act` commands build their request from `actions`, as a page would, then
take an action key, idempotency key and optional payload. The second `act`
replays request 1 although the subject version moved; a changed payload under
`ack:1` conflicts, as do a stale subject version and a stale work version.
`act-outer-fail` executes inside an owner transaction that rolls back, leaving
the record version and request table unchanged (`show 4 1`). `act-race`
submits the same request on two connections while the handler sleeps: one
executes and the other replays the same request ID, and `action_count` rises
once. `act-race-cross` races one key on records 1 and 2; record 2 conflicts.
On record 3, `reference.review.submit` is offered for the `review` item of the
`reference.human` run; the raced submission completes it once, releases
`publish` and the action is no longer offered. The viewer (user 2) is offered
nothing. `{"handler_fail":true}` as the payload makes the handler refuse.

Arguments are `command [user-id] [record-id]`; user 4 is the fixture admin.
For `run`, `run-drift`, `events` and the lifecycle commands, the record-ID
position takes a process run ID. Lifecycle commands then take a step key,
signal name or reason; `complete` also takes an optional outcome.
`init` first marks tenant 1 as the platform operator in that disposable
database, because `Engine::initialize` requires the platform capability
`admin.workflow.definition.manage`; production marks the operator through `bilibi-web platform-provision` or `seed`.
Other commands: `show`, `initial`, `comment`, `adopt`, `guard-deny`,
`action-fail`, `bad-persist` (persist wrote the wrong status), `stale`
(expected version mismatch), `outer-fail` (owner facade write, then a failing
transition in the same transaction; the outer commit is refused and the facade
write rolls back) and `cancel` (the transition future is dropped mid-action and
the outer commit is refused). Repeat `show` and `history` after each refusal to
confirm nothing was written. Running `init` again changes nothing.


## Reference page and owner integration

`/workflow/reference/{id}` (`src/web.rs`, `templates/reference.html`) is the
generic action, pending-work and history adapter ported from Bilimbi's
`ReferenceLive`, and the pattern an owner screen follows:

- Read through the facade only: `available_actions` (the subject version and
  the offered entries), `history` and `pending_work` narrowed to the subject.
  The page never queries the owner table, Workflow tables or the request log.
- Render each offered entry as a form that echoes the `ActionRequest` fields:
  the action key, the work item for a work-bound entry and an idempotency key
  the server issued for that rendering (`reference:<uuid>`). The actor, tenant,
  capability and versions are never client values; the mounted state supplies
  `expected_subject_version` and `for_entry` the work binding it was shown.
- Submit through `execute_action` and reload from the facade on success. Map
  the kernel's refusals to the page's copy (`Failure` in `src/web.rs`): a
  `Conflict` is "This record changed. Refresh the page and try again.", a
  `Forbidden` is "Your role does not allow this action.", an entry no longer in
  the rendered list is "That action is no longer available. Refresh the page.",
  a `NotFound` subject is "This record is not available.", and an
  unregistered subject kind is Bilimbi's generic "Workflow could not complete
  the request. Refresh the page and review the record."
  Only `Unauthorized` propagates, so the host clears the page.
- Two submissions of one rendering (a double-click, a resend after a reconnect)
  carry the same key: the second replays the first result and the owner effect
  happens once. A submission against versions another session moved conflicts.
- The two lists are `ListColumns` over built-in columns through the shared
  `FlexTable`; the `When` column keeps its typed timestamp.

The route requires `admin.reference.record.approve`, which this module declares
and grants to no system role; the fixture admin holds it through grant-all. The
`reference.record` subject is contributed only by the fixture executables, so
a production install shows the generic "Workflow could not complete the request. Refresh the page and review the record." there; do not turn
the page into a generic workflow administration product.

To run the page against the disposable reference owner, in a fresh disposable
database after `init` (and optionally `start-human 4 1` for a work-bound
entry), serve the fixture executable and sign in as the fixture admin:

```sh
export BILIBI_DISPOSABLE_WORKFLOW_DEMO=yes
cargo run -p bilibi-workflow --example owner -- init
cargo run -p bilibi-workflow --example owner -- start-human 4 1
APP_ORIGIN='http://127.0.0.1:4317' BIND_ADDR='127.0.0.1:4317' \
cargo run -p bilibi-web --example w7-workflow-reference
```

`/workflow/reference/1` offers Acknowledge and, for the `review` item, Submit
review; `/workflow/reference/0` and a missing id answer the address/record copy.
A viewer (user 2, no route capability) gets 403. Executing in one tab and then
submitting the same entry in a second tab answers "This record changed"; a
direct denial of `companies.update` for the admin answers "Your role does not
allow this action." while the page stays usable; two rapid submissions of one
rendering raise `action_count` once (`show 4 1`) and one request row.
`cargo run -p bilibi-web -- run` serves the same route without the owner.

## Committed transition delivery

Contribute `Contribution::Listener(outbox::Listener { key, subjects, adapter })`
using stable listener and canonical subject keys. Boot rejects duplicate keys,
unknown subjects and contributors that do not depend on Workflow. Listeners
run in key order after the transition commits, under the binding tenant's live
anonymous `SystemScope`. The historical actor is data, never authority.

Use `outbox::TransitionListener::handle` for database effects in the supplied
transaction and return all `Change` values for audit. The kernel commits a
listener receipt with those effects and audit evidence; a subsequent listener
failure cannot repeat an earlier successful effect. A refusal, panic, cancellation
or audit failure rolls back that listener's effects. A restarted worker skips
committed receipts. Perform external IO through an owner's durable outbox,
using `TransitionEvent::event_key` for remote idempotency.

A transition atomically writes owner effects, history, one
`base_workflow_transition_outbox` row keyed by history ID and an immediate
`base/workflow-transition-delivery` queue job. Initial records and comments
produce no transition event. Event facts contain the stored edge and immutable
history, including status, version, attribution, comments and time. Delivery
resolves tenant, subject, flow and owner through history's proven binding;
queue arguments carry only a history ID. No actor is re-created from history.

Each delivery claims a five-minute database lease with a fresh token. Successful
listener effects are fenced by that token and a row lock; an expired worker
cannot overwrite a newer attempt. Failures retain a bounded error code and
retry after `2^min(attempt,10)` seconds (maximum 1,024 seconds). Pending rows
have no terminal retry limit: maintenance recovers them even if their immediate
queue job exhausts retries or its original user is no longer eligible.
The immediate job needs `workflow.transition.deliver`, which no product role
carries: grant it to the roles that deliver (`core_admin`'s grant-all permits
it; the walkthrough fixture's manager holds it).

`base/workflow-maintenance` appears on `/system/schedule` with a once-per-minute
UTC recurrence, coalesced downtime and forbidden overlap. It stays unreviewed
and disabled until an operator enables it. The handler uses Schedule's
execution metadata/review gate; it does not accept unscheduled arguments.
An operator may also call `Engine::maintain_workflows(scope, batch_size,
keep_days)` using `admin.system.schedule.execute`.

Maintenance repairs running process leases and timers using the saved graph and
fingerprint. Owners implement `SubjectAdapter::maintenance_load` to tenant-scope,
lock and prove the owner row under a system identity; the default defers that
owner's runs. It must only load/lock, never apply business policy or effects.
The worker preserves owner → run → work lock order and executes no process,
guard, action or human handler. Missing owners/definitions, changed fingerprints,
inactive tenants and invalid graphs are skipped without changing their runs.
Keyset pages visit every running row, so unavailable early rows cannot starve
later ones. Completed facts remain intact and paused runs remain paused.

After reconciliation, maintenance delivers up to 100 due events and removes
up to 100 delivered rows older than `workflow.outbox.keep_days` (global integer,
1–3,650, default 30). Receipts are removed only with their delivered event.
Pending/leased events, status history, process events and human-action replay
records are retained. Retention never reconstructs or replays old events.

The disposable `owner` example adds these commands (the final ID is a history
ID for delivery commands):

```sh
cargo run -p bilibi-workflow --example owner -- init
cargo run -p bilibi-workflow --example owner -- approve 4 1
cargo run -p bilibi-workflow --example owner -- outbox
cargo run -p bilibi-workflow --example owner -- delivery 4 1
cargo run -p bilibi-workflow --example owner -- outbox
cargo run -p bilibi-workflow --example owner -- listeners-ready
# After the printed available time, retry on two connections:
cargo run -p bilibi-workflow --example owner -- delivery-race 4 1
cargo run -p bilibi-workflow --example owner -- outbox
cargo run -p bilibi-workflow --example owner -- maintenance
```

Initially the second listener inserts an effect and refuses: its insert rolls
back, while the first effect and receipt remain. After `listeners-ready`, the
second effect commits and each listener's count is one. Repeating delivery
skips the delivered event. `execute-queue` runs the real queue executor for four
seconds; `listeners-fail` restores refusal, and `listeners-wait` delays the second
listener for 30 seconds so the operator can stop that example process and
observe lease recovery on restart. `maintenance` also repairs expired work
created by `start-worker`/`claim-race` and releases due timers without executing
the work. These are real database/app commands, with no unit-test runner.
