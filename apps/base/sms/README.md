# Base SMS

Send text messages through one gateway. No Bilimbi counterpart; the shape follows Base Mail. Decision: [ADR 03](../../../docs/architecture/decisions/03-sms-second-factor.md).

## Use it

- Get `Arc<Gateway>` from service `bilibi_sms::SERVICE` (`base/sms.gateway`) and call `send(&PhoneNumber, text)`.
- Parse every number with `PhoneNumber::parse` (E.164: `+`, 8 to 15 digits, no leading zero; spaces, hyphens, dots and brackets are dropped). Show it to others with `masked()` (`+94 ••• ••• 567`).
- A text is 1 to 160 characters with no control characters.
- Check `available()` before offering SMS. When it is false, refuse: never pretend a message went.

## Drivers

| Driver | Server role (`instance.role`) | What it does |
|---|---|---|
| Simulator (`Outbox`) | Dev or Staging | Writes each message to a private file in `BILIBI_SMS_DIR` (default `.local/sms-outbox`), newest 100 kept |
| None | Production | `available()` is false; `send` answers `Error::Unavailable` |

- The gateway reads the stored role (Base System) on every use, so changing the role on `/system/instance` takes effect at once. A failed lookup counts as Production.
- `/dev/sms-inbox` (Web) lists the simulator's messages with the code shown large, and answers 404 on Production.
- A real provider is a new `Driver` that sends through `base/integration`, with its credentials as encrypted settings. None is built yet.

## Rules

- The outbox files hold live codes. They are for development and staging only.
- Never log a message's text. Drivers report failures as `Error::Delivery` for operators' logs.
