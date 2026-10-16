# F5 browser parity sweep

Manual interaction sweep against the disposable Bilibi database on 2026-10-16.
The browser used the orb's Chrome; the app and browser used the same local
`APP_ORIGIN`. GeoNames and settings specimens were confined to disposable
databases. No product parity defects were observed, so there is no product fix
commit from this sweep.

| Interaction | Result | Fix commit |
| --- | --- | --- |
| GeoNames import and inline correction | Imported bounded sample payloads (countries 6/4 skipped, Admin1 10/1, cities 10/2, MY postcodes 10/5); inline place-name correction saved. | None — passed |
| Settings secret reveal, clear, restore | Fixture secret revealed after password confirmation, returned to masked state, then cleared; Restore confirmed and cleared two overrides. | None — passed |
| Terminate another live session | Terminating the other browser's live session redirected it to the expired-session sign-in page; the current session remained active. | None — passed |
| Workspace split, resize, share | Opened Contacts and Settings tiles, resized the split to 55%, saved a layout and published a company-shared workspace. | None — passed |
| Schedule daylight-saving cases | Walkthrough showed both 01:30 fall-back instants and skipped the spring-forward 02:30 occurrence; cron validation cases also passed. | None — passed |
| OAuth device approval | Browser reviewed and approved a device request; token exchange succeeded. | None — passed |
| `/api/v1` and `/mcp` with bearer tokens | Device token returned 200 for `/api/v1/search`; an MCP-resource token returned 200 for authenticated `tools/call` (`search`). | None — passed |

Verification: `python3 scripts/discover-modules.py --check`,
`cargo fmt --all -- --check`, and
`cargo clippy --workspace --all-targets --locked -- -D warnings` passed.
