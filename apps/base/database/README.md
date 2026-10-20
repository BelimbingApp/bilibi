# Database write and seed contracts

Business owners obtain `MutationBoundary` from the `base/database.mutations`
registry service. Start one transaction with trusted `Context`, and pass its
mutable `MutationTransaction` to every nested owner. Use `mutate` for writes and
return all affected records as `Change` values, including bulk writes. Owner SQL,
record capture, action capture and nested notifications commit together. A write,
capture or cancelled future poisons the transaction, so catching its error cannot
permit commit. Dropping the transaction rolls back. `sql()` exposes locking and
query operations; it is an explicit escape hatch, not automatic SQL interception.
Every business write using it must still call `record`. Sessions and migration or
seed ledgers are lifecycle writes, rather than business mutations.

During a live page event `begin` also asks the page-write check
([src/page_write.rs](src/page_write.rs)): it refuses a business transaction
the page's declaration does not cover (the contract, the allowance and the
host's part are in `apps/base/agent_api/README.md`). A person's own view state
(list columns, arrangement, theme, pins) opens `begin_view_state` instead,
which the check leaves alone; never use it for business data.

The authoritative API is [src/transaction.rs](src/transaction.rs). Audit binds its
`Capture` implementation once through `MutationBoundary::bind_capture` during
service installation; a readiness check refuses boot, and `begin` refuses a
transaction, while nothing is bound. Capture methods receive the same PostgreSQL
transaction and must propagate failure; `Capture::mutations` receives a whole
bulk write so the implementation can batch. Rows, redaction, attribution and
attempted/refused action semantics are owned by `base/audit`
(see [apps/base/audit/README.md](../audit/README.md)). The historical
`audit_mutations` and `bilibi_action_commits` legacy ledgers are frozen; Audit
carried their rows over once in `006-audit-evidence`. `begin_with` also supports
explicit deployment adapters.

`Context` carries tenant, actor `{kind,id}`, optional impersonator, action and one
trace ID shared by all nested changes. `agent_connection` records the durable agent grant without replacing the person as actor. Database validates its shape; it does not
prove authorization. F3's session/worker adapters supply trusted attribution.
Use the Audit actor kinds (`user`, `agent`, `guest`, `console`, `scheduler`,
`queue`, `system`, `unknown`); a `system` principal carries its name as the id and
`0` when anonymous. Audit refuses any other kind, which refuses the commit.

Register infallible, synchronous notification/cache callbacks with `after_commit`.
They run in registration order after PostgreSQL accepts commit and never on
rollback, drop, capture failure or commit failure. NotificationBoundary registers
its own bell callback; business owners do not publish manually. A callback must
not panic or perform durable work: process failure can lose fan-out, and durable
work belongs in a transactional Queue/outbox. Share the outer boundary rather
than opening or committing an inner transaction.

# Schema ownership

`001-base`, `002-user` and `003-company` and their SQL files are frozen historical
migrations of the first slices. Do not split, rename, move their registration, or replay their
DDL on databases where their versions were applied. `004-database-boundary` is
an additive bridge and Database ledger migration; `006-audit-evidence` is Audit's. Future tenancy, session,
authorization and audit DDL is contributed by those owning modules through
`Module::migrations`; Database only supplies orchestration. Use globally unique
module-prefixed versions, such as `base/settings/001-values`; a Domain or
Extension module must name them `<name>-<module>-NNN-<slug>` (`<name>` is the Domain or Extension) with
underscores as hyphens (`supply-chain-purchasing-001-suppliers`), which the
registry enforces. `migrate(db, &modules)` runs migrations in dependency order
under a transaction advisory lock. A failed batch rolls back both DDL and
migration ledger rows.

The `bilibi_migrations` ledger records each applied version's owner module and
the SHA-256 of its SQL. A version whose SQL changed, or that another module now
declares, is refused: never edit an applied migration, publish a new version.
Versions recorded by a module that is no longer mounted stay valid and are
reported as kept, so unmounting and remounting a Domain neither replays nor
loses its schema. Rows applied before the ledger kept provenance adopt the
owner and hash of the module that declares them on the next run.

# Production seeds

`cargo run -p bilibi-web -- seed-production` runs only the production seeds;
`seed`, the development seed, runs them before it bootstraps the platform
operator. Application startup performs neither.

`Development` seeds are Bilimbi's module `dev_seed` scripts: idempotent sample
data that only `bilibi-web seed` runs, through `seeds::run_development` on the
transaction that bootstraps the operator, so a repeat leaves what the operator
changed. They keep no ledger and must find their own subject (Company's sample
departments look up the operator's primary company).

Contribute a typed `Seed` to the registry's `SEEDS` catalog with Production mode,
an immutable version, and either trusted SQL or a service operation. The runner
in [src/seeds.rs](src/seeds.rs) executes module dependencies first, then contribution
IDs within each module. Publish separate dependent modules when order is needed
between owners; within a module choose ordered IDs. Development entries are excluded.
For a service seed, register `SeedService(Arc<dyn SeedExecutor>)` at the declared
service ID. The executor receives the runner's transaction and must use it for
all seed data. No extra checkout, transaction control or external IO is allowed.
Treat SQL/service operations as trusted deployment code, never client input.

The runner serializes with other seeds and migrations and commits the selected
batch's data together with `bilibi_production_seeds`. Failure leaves neither data
nor successful ledger entries from that batch; interruption can safely retry.
Repeated versions skip execution, preserving operator edits. Reusing an applied
version with a changed module, contribution ID, SQL or service/operation is refused.
Service implementations themselves must remain stable under an applied version;
publish a new version/operation when behavior changes. The definition ledger
stores SQL text, so put secrets behind service parameters rather than SQL literals.

# Runtime evidence

F2 was demonstrated with external runtime fixture executables and a disposable
PostgreSQL database, without unit tests. Evidence lives in Firstmate's task data
folder `data/bilibi-f2/`: legacy upgrade without replay, repeat development seed preserving
an edit, shared Company/notification trace and bell fan-out, explicit rollback,
drop, swallowed capture failure, action-capture failure, cancelled owner/capture,
deferred commit failure, system/impersonator attribution, failed seed rollback and
retry, repeat seed skipping, definition drift refusal, SQL/service dependency
ordering, concurrent seed serialization and exclusion of non-production contributions.
