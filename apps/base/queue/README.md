# Durable Queue

Queue stores every job in PostgreSQL. Owners contribute a `Worker` through the
`base/queue.workers` typed catalog, using `Contribution::new` and source provenance.
Boot rejects invalid/duplicate worker IDs and attempts outside 1–100. The worker's
validator must reject unknown keys and enforce its own business types; it can
normalize values. Queue applies the transport bounds both before and after that
validator: object root, depth 8, 100 members per container, 128-byte keys,
16,384-byte strings, and 65,536 bytes encoded JSON. Only the default queue is
supported. Callers cannot choose attempts, transport metadata, or actor identity.

Use `Queue::enqueue_for` with the sealed `Scope` and the owner's existing
`MutationTransaction`. The audit context must match the tenant/user in that
scope. Queue inserts through `mutate`, so insertion or audit failure poisons the
transaction. Owner writes, job insertion, and audit evidence commit or roll back
together. A returned `JobRef` is provisional until the outer commit. Queue audit
rows exclude arguments; jobs should contain references, never secrets.

Actor user and optional Person/Organisation/Affiliation IDs are captured in
separate columns, together with worker owner and trace ID. Names, email, CSRF,
sessions and passwords are never copied. These are historical references, not
proofs of current authority. There are no foreign keys to Core identities: a job
survives deletion so execution can record an explicit refusal.

Owners also contribute a `HandlerBinding` through `execution::HANDLERS`, using
exactly the same worker ID and module provenance. Its `service` must belong to
that module and resolve to `HandlerService(Arc<dyn Handler>)`; its `capability`
must be declared in Authz. `Executor::from_services` runs after all services are
installed, so Queue never depends upward on owner modules. Missing, foreign or
unknown bindings refuse executor startup. The Web host starts the executor with
10 slots, one-second polling, 30-second leases and a 15-minute attempt timeout.
Other private hosts can configure bounded `execution::Config` and use `run`
with a Tokio watch shutdown receiver.

PostgreSQL `FOR UPDATE SKIP LOCKED` claims serialize across app processes.
Claims increment the durable attempt before invoking owner code. An expired
lease is recovered as retryable, or discarded at the attempt limit. Retry delay
is `min(3600, 2^attempt)` seconds for both owner failure and expired leases.
Heartbeats renew only a still-live matching token; completion, retry and cancel
are likewise fenced. A stale executor cannot overwrite a newer claim or a
cancellation. Shutdown drops in-flight futures and leaves their leases for
recovery. Handler/validator panics, timeouts and invalid results retry within
the captured attempt budget; infrastructure errors preserve durable recovery.
Only bounded safe failure codes are stored, never error payloads or arguments.

Each attempt reconstructs a current `Scope` through Tenancy Authentication and
requires the same captured Person/Organisation/Affiliation facets. Inactive,
deleted, changed or denied actors cancel with `actor_refused`; transient DB
failures retry. Arguments are validated again. The binding's capability is
checked before invoking the handler. Use `Execution::begin` for owner database
writes: it opens the audited boundary, preserves the enqueue trace ID, locks the
job's live matching lease and rechecks actor eligibility and capability in that
transaction. Owners must also reauthorize each target-specific command. Keep
transactions short and do external IO outside them. Cancellation waits for any
already locked business transaction; it cannot undo a committed effect.
At-least-once delivery requires an owner idempotency key, including for external
IO and when a business commit succeeds before queue completion is recorded.

`Queue::enqueue_system_within` is the same system job enqueued from inside
another actor's transaction, for a Platform delivery edge whose work is not
that actor's command (Base Events delivers a recorded event as
`domain-events`). The job and its audit row commit with the owner's writes and
are attributed to the transaction's actor; it executes only as the principal,
under the same per-attempt declaration and grant checks below.

`Queue::enqueue_system` enqueues as a named system principal (`SystemScope`,
for example Schedule's `scheduler`) instead of a user. The audit context must
already name that principal in the same tenant, and the principal must be
declared in the Authz vocabulary. A system job carries `actor_kind='system'`,
`system_principal` and no party facets. Each attempt rehydrates the principal
(`SystemScope::rehydrate`: live tenant plus current declaration) and checks the
binding's capability through `authorize_system`, which also needs the durable
tenant grant (`Grants::set_system`); a principal without that grant cancels with
`actor_refused`. `Execution::identity` says which kind of actor is running.
`Execution::scope` is `Some` for both kinds (SP1): for a system job it is the
principal's sealed scope, built on every attempt from the rehydrated principal
through `SystemAuthentication::for_system`, so the handler calls owner facades
(Settings, Company targets, Domain facades) with it and each decides by the
principal's name and grants. A handler written when a system job had no scope
may still refuse `Identity::System`. `Execution::begin` re-proves the same
identity inside the owner's transaction. `Queue::enqueue_for` with a
principal's scope enqueues a system job as that principal.

`store::lookup` and bounded `store::states` return redacted state/attempt/failure
facts including terminal states. Trusted owner/scheduler adapters supply a
proven tenant; UI/API adapters must authorize their own reads. `cancel_for` and
`retry_for` join an owner-authorized audited transaction and require matching
scope/context. Cancel is idempotent for terminal jobs. Explicit retry resets the
attempt budget of cancelled/discarded/retryable/scheduled jobs; completed and
executing jobs refuse replay. User-delegated and named system-principal jobs are supported; anonymous work
and uniqueness policy remain separate extensions.
Impersonation is refused until Tenancy supplies a sealed impersonator facet;
caller-supplied audit attribution cannot create execution authority.

## Running proof (no unit tests)

Use an explicitly disposable PostgreSQL database, never the normal development
database. Create it with your local PostgreSQL tools and set `DATABASE_URL` to
that database. Migrate it, then run the owner walkthrough, which installs the
walkthrough fixture accounts itself:

```sh
cargo run -p bilibi-web -- migrate
BILIBI_DISPOSABLE_QUEUE_DEMO=yes cargo run -p bilibi-queue --example queue-walkthrough
BILIBI_DISPOSABLE_QUEUE_DEMO=yes cargo run -p bilibi-queue --example queue-walkthrough -- inspect
```

The walkthrough prints external visibility before commit, atomic owner/job/audit
rollback and commit, argument refusals, and mismatched scope/context refusal.
`inspect` runs in another process and prints persisted redacted job summaries.
The two contributed workers are linked only into the example; production has no
specimen worker. An empty production worker catalog is valid.

The private executor walkthrough below also proves SP1: `enqueue-system secret`
enqueues the specimen job as the declared principal `queue-specimen`; the
worker cancels it with `actor_refused` until `grant` gives the principal its
two capabilities (`Grants::set_system`, step-up off for this fixture binary),
then runs it as `System · queue-specimen`, reads the encrypted company setting
`queue.specimen.secret` through `Execution::scope()` (printing only that it
decrypted and its length), and records its effect; `revoke-secret` keeps the
job running but makes Settings refuse the secret (`secret_refused`).
`inspect` prints the audit rows of the effects with their principal.


## Private executor walkthrough

The private executable links Queue and the Platform it runs in; the fixture's
accounts sign in through Core User's verifier (the executable binds none of
its own), and `prepare` migrates the database and installs the fixture. On a
fresh disposable database, with `BILIBI_SETTINGS_KEY` set:

```sh
export BILIBI_DISPOSABLE_QUEUE_DEMO=yes
cargo run -p bilibi-queue --example execution-walkthrough -- prepare
cargo run -p bilibi-queue --example execution-walkthrough -- enqueue restart
cargo run -p bilibi-queue --example execution-walkthrough -- worker
```

In another terminal, `inspect` prints durable facts. Kill only the PID printed
by that private worker while the restart job is executing, then launch `worker`
again. With the specimen's three-second lease, the job expires, backs off and
finishes on attempt two with one effect in `queue_execution_effects`. Ctrl-C
stops the private executable gracefully. It never drives Herdr or shared app
processes. The contributed worker/handler is absent from production.

Other enqueue modes: `retry`, `discard`, `cancel`, `panic`, `invalid`,
`heartbeat`, `timeout`, `late-write`, `fenced-write`, `complete`, `secret`
(reads the company secret as the actor; `enqueue-system secret` as the
principal; `BILIBI_SETTINGS_KEY` must be set for `prepare` and `worker`).
`grant`, `revoke-secret` and `revoke` change the principal's grants as the
fixture administrator. `cancel JOB_ID` and
`retry JOB_ID` use the production audited owner commands. `QUEUE_DEMO_RUNTIME`
sets the private attempt timeout (seconds). The heartbeat mode takes eight
seconds, exceeding the three-second lease without a second attempt. The effect
uses job ID as an owner idempotency key. To demonstrate current actor refusal,
set the enqueuing account's `users.status` to `suspended` after enqueue and
before starting the private worker. To demonstrate reauthorization before a
delayed write, change the specimen's `principal_capabilities` row for
`companies.update` to false while `late-write` is executing. Both refuse the
owner effect. All such manual database changes belong exclusively in the
disposable specimen DB.
