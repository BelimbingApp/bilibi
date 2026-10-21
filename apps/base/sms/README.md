# SMS

`base/sms` owns outgoing text messages: one validated `Message`, one `Sms` service (`base/sms.sender`) and one bound `Transport`. No Bilimbi counterpart; modelled on Base Mail.

- Owners build a `Message` and call `Sms::send`. They never call a provider or write message files themselves.
- `Message::new` and `Message::one_time_code` are the only place a recipient and body are checked.
- The module names no consumer. Core User's two-factor codes are the first.

## Send a message

```rust
let to = bilibi_sms::PhoneNumber::parse("+94 77 123 4567").ok_or(…)?; // stored and sent as +94771234567
let message = Message::one_time_code(to, "Bilibi verification code: 123456. …", "erp.example.com", "123456")?;
sms.send(&message).await?; // Arc<Sms> from bilibi_sms::SERVICE
```

| Rule | Value |
|---|---|
| Recipient | E.164 (`PhoneNumber`): `+`, 7 to 15 digits, the first not 0. Spaces, hyphens, dots and parentheses are dropped. `FORMAT` is the field error |
| Body | 1 to 459 characters (`BODY_MAX`, three GSM-7 segments), no control character but a line break |
| One-time code | Ends with `@host #code` (WICG origin-bound one-time codes), so a phone offers the code only on that site |
| Display | `PhoneNumber::masked` shows `+94 ••• ••• 567` to someone proving they hold the number |

## Transports

**Bound once at boot.** The host decides; nothing reads a provider setting yet.

| Saved instance role | Transport | `send` |
|---|---|---|
| Dev | **Simulator** (`Outbox`): one private JSON file per message in `BILIBI_SMS_DIR` (default `.local/sms-outbox`), newest 100 kept, directory `0700`, files `0600` | Stores the message |
| Staging, Production | None | `Error::Unavailable` |

- The host binds the simulator on every server behind a `Simulation` gate (`Sms::use_simulator`) that reads the saved `instance.role` (Base System) on every send. A role change takes effect without a restart, and a role that cannot be read refuses.
- `Sms::available` says whether anything can be sent now. An owner that needs SMS must treat "unavailable" as a missing feature, not a failure to retry.
- The boot log prints the transport (`sms: …`), never a message.
- A real provider is a future `Transport` bound with `Sms::bind`. Its credentials will be encrypted installation settings with an editor (`AGENTS.md`, Configurable), and its HTTP calls go through `base/integration`.

## Development inbox

`/dev/sms-inbox` shows the simulator's messages newest first, one card per message: recipient, time, the code apart, then the full text.

- It shows messages only while the simulator is allowed (the saved role is Dev). Otherwise the path answers 404 like any unknown path.
- It is anonymous on purpose: a tester needs the code before signing in. Anyone who reaches a development server can read every code on it.
- The page model is `InboxPage` (`src/inbox.rs`); the host renders it in the credential layout.

## Delivery contract

`send` returns `Error::Delivery(detail)` with a transport detail for logs, not for users. A caller undoes what depended on delivery when it fails: Core User voids a code that was not delivered.
