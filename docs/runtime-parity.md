# Runtime smoke and parity evidence

Bilibi has no unit tests. Use the machine-readable [Companies fixture manifest](../fixtures/parity/companies-v1.json) for the current runnable slice and this checklist to record real app evidence. The fixture installs `fixtures/walkthrough` into the disposable database. Future fixtures should be versioned when their records or expected behavior change; do not turn these cases into unit tests.

## Isolated setup

1. Create a fresh disposable database for Bilibi and a separate fresh disposable database for the pinned Bilimbi reference. Never point either app at a developer, shared, or production database. Record how each disposable database is isolated and removed.
2. Record the Bilibi commit, Bilimbi commit, fixture version, database identifiers (not credentials), runtime versions, and exact origins. Do not record passwords, cookies, session IDs, or tokens in evidence.
3. For Bilibi, run `python3 scripts/discover-modules.py`, `cargo run -p bilibi-web -- migrate`, `cargo run -p bilibi-walkthrough-fixture`, then launch with the manifest's isolated database URL and a free local origin/port. Serve Bilibi on `127.0.0.1` and Bilimbi on `localhost`: cookies ignore ports, so distinct ports on the same host do not isolate their sessions.
4. Confirm both apps are reachable and log in using only the manifest accounts. If the Bilimbi data differs, create equivalent records in its isolated database and note every mapping; do not silently compare unlike data.

The walkthrough fixture (`fixtures/walkthrough`) creates tenants 1 and 2, five permission roles, seven accounts (three linked to Persons, two of them affiliated with a company), 62 tenant 1 companies and one tenant 2 company. Sign-in is by email alone; re-installing the fixture is idempotent. It is not product data: a real install creates its first operator with `create-owner`. The manifest's password is the fixture credential and must only be used against disposable local databases.

## Smoke and visual comparison

Use `chrome-devtools-axi` for both apps with `CHROME_DEVTOOLS_AXI_SESSION` set to the task ID on every invocation; the default browser session is shared with other workers. Match viewport size, device scale factor, browser zoom, locale, timezone, color scheme, account/tenant, and browser version. Wait for fonts and loading indicators before capture. Run the manifest cases in both apps where the behavior exists; mark unsupported or not-yet-ported behavior as a gap instead of skipping it silently.

For every case, capture the initial state and the listed interactions, including browser Back and reload when applicable. Compare screenshots side by side and with a pixel overlay/diff. Keep masks limited to documented volatile timestamps or cursors. Also check focus, scroll position, draft preservation, URL state, browser console, network responses, and relevant server output. Screenshots alone do not establish authorization, persistence, or audit behavior.

## Run evidence record

Copy this section into a dated evidence file outside the source tree for each run. Keep screenshots, overlays, browser/network captures, and bounded database evidence beside that record.

```text
Fixture: bilibi-companies-v1
Run date/time (UTC):
Bilibi commit / Bilimbi commit:
Browser and versions:
Viewport / scale / zoom / locale / timezone / theme:
Bilibi origin / Bilimbi origin:
Isolated database setup and cleanup:
Accounts and permission states exercised:

Case and interaction step:
  Result (pass / fail / gap / not run):
  Screenshot paths (both apps, source state, equal dimensions):
  Overlay/diff path:
  URL and browser Back/reload result:
  Focus/draft/scroll/frame observations:
  Console/network/server evidence:
  Authorization/persistence/audit evidence:
  Difference or follow-up slice:
```

## Common parity misses

- Validation rules run in Bilimbi's order and report only the first failing rule.
- Refusal and error texts are Bilimbi's exact words ("The file's type could not be recognised."), not an error's `Display` text.
- A refused confirm keeps the dialog open next to the flash.
- A save uses the record the page showed, so a stale record gets the owner's refusal.
- After a recorded change the page reads its data again (Bilimbi `load/1`).

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
