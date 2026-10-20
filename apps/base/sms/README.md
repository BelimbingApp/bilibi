# Base SMS

Outgoing text messages, built like `base/mail`: one validated `Message`, one `Sms` service (`base/sms.sender`), and a driver chosen once at boot from env. No Bilimbi counterpart.

- Owners build a `Message` and call `Sms::deliver`. They never write message files or call a provider themselves.
- Today the only consumer is Core User's SMS one-time codes (`apps/core/user/README.md`, "SMS codes").

## Choosing the driver

| `SMS_DRIVER` | Dev, Staging | Production |
|---|---|---|
| unset | `sandbox` | `disabled` |
| `sandbox` | Each message is one private file in `BILIBI_SMS_DIR` | Refuses boot: the files hold live codes |
| `disabled` | No text messages; SMS codes cannot be turned on | Same |
| anything else | Refuses boot, naming the supported drivers | Same |

| Variable | Use |
|---|---|
| `SMS_DRIVER` | The driver (above) |
| `SMS_FROM` | Sender name, at most 20 printable characters. Default `Bilibi` |
| `BILIBI_SMS_DIR` | Sandbox folder. Default `.local/sms` (newest 100 kept, folder `0700`, files `0600`) |

- The instance role is `bilibi_system::Role::from_env` (`BILIBI_ENV`).
- The boot log names the driver (`sms: sandbox .local/sms …`), never a credential.

## Sandbox inbox

`/dev/sms-inbox` shows the sandbox's messages as a phone would, newest first: the masked recipient, sender, time and text, with the code drawn large.

- **Public**, because a tester at a code prompt is not signed in yet. So it exists only on a **development** instance with the sandbox driver; anywhere else it answers 404.
- Reload to see new messages.

## Adding a real provider

1. Implement `Transport` for it (`send`, `describe`). Send through `base/integration` (`Integration::send`), the only outgoing HTTP client, so every call is logged.
2. Accept its name in `Sms::from_lookup` with its own `SMS_*` variables. A half-configured provider refuses boot, as a half-configured mail relay does.
3. Keep secrets out of the boot log and the exchange log.

## Rules

- `Message::new` takes an E.164 number (`normalize_phone`: `+` and 8 to 15 digits; spaces, dashes, dots and brackets are dropped) and a body of 1 to 480 characters without control characters other than a newline.
- `mask` shows a number to someone proving they hold it: `+60 ••• ••• 789`.
- `Sms::available` says whether any driver sends; pages hide SMS choices when it is false.
