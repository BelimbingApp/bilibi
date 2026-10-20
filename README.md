# 2FA setup before saving

The shared form shows setup before a protected submission, and preserves ordinary saves when only part of a form needs 2FA.

## Runtime

- Target origin and APP_ORIGIN: `http://127.0.0.1:4384`.
- Separate release-source origin and APP_ORIGIN: `http://127.0.0.1:4386`.
- Database names: `bilibi_2fa` and `bilibi_2fa_source`, on a task-owned PostgreSQL 18 process at `127.0.0.1:5484`.
- Both databases received only migrations and `fixtures/walkthrough`; no shared developer database was used.
- Browser: Headless Chrome 154, `en-US`, Asia/Kuala_Lumpur, scale 1. Desktop 1440 × 1000; phone 390 × 844 with mobile and touch emulation. Light and dark schemes both exercised.
- Browser session: `CHROME_DEVTOOLS_AXI_SESSION=bilibi-2fa-setup-first` on every invocation.
- Source reference: the task's original `instance-settings.png`. The 2FA component has no Bilimbi counterpart.

## Behaviour

| Case | Result |
| --- | --- |
| Instance settings without 2FA | Role selector disabled, setup link beside it, code input absent. Save remained available. Changing the instance name saved without a setup redirect. Reload and a bounded database read confirmed persistence. |
| Setup and return | Focused the setup link and pressed Enter. The URL kept `return=/system/instance`. Completed password-confirmed enrollment with a real authenticator code; the recovery-code acknowledgement returned to Instance settings. No setup key, QR code, recovery code or filled credential was captured. |
| Instance settings with 2FA | Role selector enabled, numeric code input and Save present, setup link absent. A role change without a code was refused. A fresh code saved Staging; a database read confirmed `instance.role=staging`. The code field was blank after saving. |
| Audit | One `step_up.refused` with `operation=base.system.instance.role.change` and `reason=code_required`, followed by `step_up.window_opened`. Enrollment and verification events also present. Bounded evidence is in `persistence-evidence.txt`; no secret fields were selected. |
| Confirm access | Without enrollment, setup link present and code/Verify absent. With enrollment, code/Verify present and setup absent. Recovery toggle still produced the separate input with a 32-character limit. |
| Operator settings | The protected pairing-key field was disabled beside a setup link before enrollment. Unrelated Save remained available. After enrollment, the protected field was enabled and its setup link absent. |
| Software pairing | Before enrollment, the key field was disabled and Save absent. After enrollment, the field and Save were available. The no-2FA pairing screenshots were recaptured on the separate unenrolled source copy at its exact origin, with the disclosure reopened after every viewport change. |
| Software install review | Reviewed an uninstalled release made from the actual task binary. Before enrollment, setup link and Cancel were present; code and confirmation absent. After enrollment, code and confirmation present; setup absent. |
| Software card | Rendered the unchanged production card in a task-only MCP host at `http://127.0.0.1:4385`. The host forwarded reads and plans to the real target API through a real, browser-approved native agent connection. Actual previews changed `two_factor_enabled` from false to true after enrollment. The card hid code/confirmation before setup and showed them afterwards. Clicking setup emitted `ui/open-link` with the absolute target URL and Software return address. |
| Shared specimens | Both `/assets/ui-components.html` and `/system/design-library/components` rendered the new setup and enrolled specimens. |
| Visual and console checks | All 48 captures were made in the browser, after fonts loaded. No horizontal overflow in either viewport. No browser JavaScript errors on the final Software page. Reviewed phone screenshots for Instance settings, the Software dialog and the card. |

## Screenshots

`screenshots/` contains 48 PNG files. Each prefix has `desktop-light`, `desktop-dark`, `phone-light` and `phone-dark` variants.

| Without enrollment | With enrollment |
| --- | --- |
| `instance-without-2fa` | `instance-with-2fa` |
| `confirm-without-2fa` | `confirm-with-2fa` |
| `settings-without-2fa` | `settings-with-2fa` |
| `pairing-without-2fa` | `pairing-with-2fa` |
| `install-without-2fa` | `install-with-2fa` |
| `card-without-2fa` | `card-with-2fa` |

Use the Instance settings before/after desktop and phone images in the PR, with Software/Confirm access examples as useful. Proof and scratch files remain outside the worktree.

## Limits

- No software install, undo or go-ahead was committed. Undo and go-ahead use the same shared review renderer; their installer effects were not exercised.
- The card used a local MCP host rather than the ChatGPT application. The proof host captured open-link requests and refused commits.
- The existing card renders its structured changes as `[object Object]`; that display is outside this 2FA change and appears in the screenshots.

## Checks

- `python3 scripts/discover-modules.py --check`: passed.
- `cargo fmt --all -- --check`: passed.
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed after all source edits were frozen.
- `./scripts/build-assets.sh --check`: passed.
- `cargo build --locked -p bilibi-web -p bilibi-walkthrough-fixture`: passed; the built binary was used for all browser checks.
- No unit tests or test crates added or run.

## Commit and cleanup

- Validated commit: `76a2a4a706d62268d024c0cf48117ad2df9fd165` on `fm/bilibi-2fa-setup-first`.
- Worktree is clean. No push or PR was created before the firstmate handoff.
- Stopped both app copies, the task card host, browser session and PostgreSQL. Process shutdown used the owned PIDs; no process-name or shared infrastructure stop was used.
- Removed both disposable databases, temporary binary copies, authenticator encryption/signing keys and OAuth credentials. Screenshots, bounded persistence evidence and reproduction scripts remain outside the worktree.
