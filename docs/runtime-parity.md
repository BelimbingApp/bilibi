# Runtime smoke and evidence

Bilibi has no unit tests. Use this checklist to record real app evidence. The walkthrough fixture (`fixtures/walkthrough`) installs into the disposable database. Future fixtures should be versioned when their records or expected behavior change; do not turn these cases into unit tests.

## Isolated setup

1. Create a fresh disposable database for Bilibi. Never point the app at a developer, shared, or production database. Record how the disposable database is isolated and removed.
2. Record the Bilibi commit, fixture version, database identifiers (not credentials), runtime versions, and exact origins. Do not record passwords, cookies, session IDs, or tokens in evidence.
3. Run `python3 scripts/discover-modules.py`, `cargo run -p bilibi-web -- migrate`, `cargo run -p bilibi-walkthrough-fixture`, then launch with the isolated database URL and a free local origin/port.
4. Confirm the app is reachable and log in using only the walkthrough fixture accounts.

The walkthrough fixture (`fixtures/walkthrough`) creates tenants 1 and 2, five permission roles, seven accounts (three linked to Persons; two of those Persons have business affiliations and explicit desks), 62 tenant 1 companies and one tenant 2 company. Sign-in is by email alone; re-installing the fixture is idempotent. It is not product data: a real install creates its first operator with `create-owner`. The fixture's password is a fixture credential and must only be used against disposable local databases.

## Smoke checks

Use `chrome-devtools-axi` with `CHROME_DEVTOOLS_AXI_SESSION` set to the task ID on every invocation; the default browser session is shared with other workers. Record the viewport size, device scale factor, browser zoom, locale, timezone, color scheme, account/tenant, and browser version. Wait for fonts and loading indicators before capture. Run the cases where the behavior exists; mark behavior that is not built yet as a gap instead of skipping it silently.

For every case, capture the initial state and the listed interactions, including browser Back and reload when applicable. Keep masks limited to documented volatile timestamps or cursors. Also check focus, scroll position, draft preservation, URL state, browser console, network responses, and relevant server output. Screenshots alone do not establish authorization, persistence, or audit behavior.

## Run evidence record

Copy this section into a dated evidence file outside the source tree for each run. Keep screenshots and bounded database evidence beside that record.

```text
Fixture: walkthrough
Run date/time (UTC):
Bilibi commit:
Browser and versions:
Viewport / scale / zoom / locale / timezone / theme:
Bilibi origin:
Isolated database setup and cleanup:
Accounts and permission states exercised:

Case and interaction step:
  Result (pass / fail / gap / not run):
  Screenshot paths:
  URL and browser Back/reload result:
  Focus/draft/scroll/frame observations:
  Console/network/server evidence:
  Authorization/persistence/audit evidence:
  Follow-up slice:
```

## Common misses

- Validation rules run in a fixed order and report only the first failing rule.
- Refusal and error texts are the exact words shown to the user (for example "The file's type could not be recognised."), not an error's `Display` text.
- A refused confirm keeps the dialog open next to the flash.
- A save uses the record the page showed, so a stale record gets the owner's refusal.
- After a recorded change the page reads its data again.

## Required checks for each implementation slice

Run the repository checks without invoking any unit-test runner:

```sh
python3 scripts/discover-modules.py --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Then run the affected route or interaction in the app and attach the relevant evidence record. `cargo check`/Clippy and screenshots complement each other; neither replaces the live behavior check. Do not run `cargo test` or add Rust, browser-hook, or Python unit tests.

For two-session or stateful behavior, record both actors and sequence: conflicting saves, permission/session revocation, archived writes, cross-tenant reads, audit rollback, job retry/restart, duplicate schedule claim, workflow replay, and token replay. Capture only bounded, redacted database evidence that demonstrates the claimed result.

Page routing, access, framing and download cases are in the [module registry's runtime checks](../apps/base/module_registry/README.md#runtime-checks).
