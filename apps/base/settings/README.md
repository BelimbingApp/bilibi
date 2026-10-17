# Settings storage contract (P2)

Contribute `Setting` payloads to `base/settings.definitions` using the registry's
`Contribution` and the declaring module's provenance. The contribution ID is the
setting key or wildcard pattern. Exact keys win, then the longest matching
pattern (lexical order breaks equal lengths). Definitions and runtime claims
share one ownership namespace, so duplicate ownership fails at discovery.

`Definition` owns JSON type, allowed scopes, default, nullable/encrypted flags,
numeric bounds, custom validator, and optional P3 editor metadata. Defaults are
validated at discovery, and every write validates again. `RuntimeClaim` permits
operational JSON at any scope, has no default, rejects null writes, and is never
a secret: use an encrypted definition for secrets. Undeclared keys, wildcard
requests, overlong keys, wrong types and forbidden scopes refuse before writing.
Writes are bounded to 1 MiB of JSON.

Resolve using `Settings::get` or `resolve_many` from `base/settings.store`.
Resolution is user → selected company → tenant → global → declared default,
filtering to the definition's allowed scopes. A stored JSON null is a real
nullable override; missing runtime state resolves to null. `Resolved.source`
is absent for a default; `overridden` means a row at the exact requested target.
Bulk reads use one uncached SQL snapshot and only the requested scope identities.

`Settings::get_global` is the one ambient read: a non-secret, global-scope value
for consumers with no session (signed links, workers). Encrypted definitions,
runtime claims and keys without a global scope refuse; a secret needs an
explicit, authorized `get`.

`Target` is a request, not permission. The actor is a sealed Tenancy `Scope`.
User means the actor's own account; tenant means their tenant. Each company read,
write, and delete calls `base/tenancy.parties` on the same DB connection. A missing
provider refuses. Core owns this binding; Settings never queries Company tables.
Global writes/delete additionally reprove the explicit platform operator.
The calling owner/HTTP adapter must freshly guard its session and authorize
route/field capabilities on **every** save, delete and reveal. The storage API
is not a browser endpoint and does not grant arbitrary settings-edit authority.
P3 owns capability-filtered forms and password/throttle/audit controls for reveal.

Write through `put`/`delete` with a shared `MutationTransaction` whose actor and
tenant match the supplied scope. Delete removes only the selected row and is
idempotent. The shared mutation capture and action capture must succeed before
commit. Secret old/new values are redacted before calling capture. Per-identity
transaction locks serialize first inserts as well as updates. Multiple owner
writes compose in one transaction and roll back together.

Set `BILIBI_SETTINGS_KEY` to 64 hex characters encoding 32 random bytes before
boot. Missing/invalid keys are tolerated until an encrypted value is read or
written; those operations then refuse. Keep the key stable and outside the DB.
AES-256-GCM envelopes have random 96-bit nonces and bind the setting key, tenant
and scope through authenticated associated data. Wrong keys, copied ciphertext,
malformed envelopes and encryption-policy mismatches refuse; there is no
plaintext fallback and no Laravel codec. Deleting ciphertext needs no key.
`Resolved` and `Encryption` deliberately have no Debug implementation. Callers
must never log values or put stored secrets into generic editor HTML.

An owner that keeps a secret in its own table (Core User's authenticator
keys) seals it with `seal_owned(owner, tenant, record, value)` and opens it
with `open_owned`: the same key and envelope, with associated data in a
separate `bilibi-owned-secret-v1` domain naming owner, tenant and record, so
the ciphertext opens neither under another record nor as a setting.

There is deliberately no value cache: transaction reads see their own writes,
rollbacks publish nothing, and other processes read committed changes directly.
`Settings::subscribe` emits identity-only notifications after commit; lagged
subscribers must reload. Cross-process consumers can LISTEN on
`bilibi_settings_changed`; PostgreSQL releases those identity-only notifications
at commit. These notifications are refresh hints, not a durable work queue.

## Manual walkthrough

On a disposable database, run the host's `migrate` command first.
Explicitly appoint fixture tenant 1 with
`INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1)`; the
production migration deliberately appoints nobody. Set `DATABASE_URL`,
`BILIBI_DISPOSABLE_SETTINGS_DEMO=yes`, and a specimen `BILIBI_SETTINGS_KEY`, then
run `cargo run -p bilibi-settings --locked --example settings-walkthrough`.
The command prints inheritance, override/delete fallback, own-transaction reads,
rollback/commit notifications, ciphertext metadata, invalid-key/type refusals,
company-provider revocation, tenant isolation, and nonoperator refusal. It uses
only `walkthrough.*` keys and never prints the stored secret. It binds its own
fixture-only Company proof provider; there is no production Company binding.
Run again with `-- inspect` to read persisted state from a separate process.
This is an opt-in service demonstration, with no test harness or assertions.
The example's contributions are linked only into the example executable.
