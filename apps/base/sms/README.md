# SMS

`base/sms` owns outgoing text messages (ADR 03): one validated `PhoneNumber` and `Message`, one `Sms` service (`base/sms.gateway`) and a `Gateway` trait. Owners build a `Message` and call `Sms::send`; they never call a provider or write message files themselves. No Bilimbi counterpart.

## Which gateway runs

Decided on every use from the stored server role (`instance.role`, Base System's `Instance::resolve`), like Step-up's enforcement, so changing the role in the status bar takes effect without a restart:

| Stored role | Gateway | `available()` | `/dev/sms-inbox` |
|---|---|---|---|
| Dev, Staging | **Simulator**: each message is one file in `BILIBI_SMS_DIR` (default `.local/sms`) | true | Shown |
| Production | None yet | false | 404 |

- The boot log prints the simulator's directory (`sms: …`), never a number or a code.
- A real provider (Notify.lk, Dialog, Twilio: to be chosen) is one more `Gateway` that sends through `base/integration`, with its key as an encrypted setting in the settings editor, never an environment variable.

## Types

| Type | Rule |
|---|---|
| `PhoneNumber::parse` | E.164 only: `+`, a country code not starting with 0, 8 to 15 digits. Spaces, dashes, dots and brackets a person types are dropped |
| `PhoneNumber::masked` | `+94 ••• ••• 567`: what a page may show someone who has only signed in with a password |
| `Message::new` | 1 to 459 characters (three GSM-7 segments), no control character but a line break |

## Simulator

- Files are JSON (`to`, `body`, `sent_at_ms`), the directory `0700` and files `0600`: they hold live codes.
- Only the newest 200 are kept. `Simulator::recent(limit)` lists them newest first and skips a file it cannot read.
- The inbox page is the host's (`apps/web/src/auth.rs`, `sms_inbox`): anonymous on purpose, because the tester is in the middle of signing in. On a Production role it answers 404, read on each request.

## Delivery contract

`send` returns `Error::Unavailable` with no gateway and `Error::Delivery(detail)` on a transport failure. The detail is for logs; callers show a generic failure and undo what depended on delivery (Core User voids the code it sent).
