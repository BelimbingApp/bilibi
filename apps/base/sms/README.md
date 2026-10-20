# Base SMS

Send text messages through one gateway. No Bilimbi counterpart; the shape follows Base Mail. Decision: [ADR 02](../../../docs/architecture/decisions/02-sms-second-factor.md).

## Use it

- Get `Arc<Gateway>` from service `bilibi_sms::SERVICE` (`base/sms.gateway`) and call `send(&PhoneNumber, text)`.
- Parse every number with `PhoneNumber::parse` (E.164: `+`, 8 to 15 digits, no leading zero; spaces, hyphens, dots and brackets are dropped). Show it to others with `masked()` (`+94 ••• ••• 567`).
- A text is 1 to 160 characters with no control characters.
- Check `available()` before offering SMS. When it is false, refuse: never pretend a message went.

## Drivers

| Driver | Where | What it does |
|---|---|---|
| Simulator (`Outbox`) | `BILIBI_ENV` development or staging | Writes each message to a private file in `BILIBI_SMS_DIR` (default `.local/sms-outbox`), newest 100 kept |
| None | production | `available()` is false; `send` answers `Error::Unavailable` |

- The host turns the simulator on (`enable_simulator`) from `BILIBI_ENV`. The boot log line `sms:` says which applies.
- `/dev/sms-inbox` (Web) lists the simulator's messages with the code shown large. It is routed only where the simulator runs.
- A real provider is a new `Driver` that sends through `base/integration`, with its credentials as encrypted settings. None is built yet.

## Rules

- The outbox files hold live codes. They are for development and staging only.
- Never log a message's text. Drivers report failures as `Error::Delivery` for operators' logs.
