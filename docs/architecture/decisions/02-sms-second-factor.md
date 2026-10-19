# ADR 02: Text-message codes as a second 2FA method, and 2FA at sign-in

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5-max (author)
**Scope:** How a code sent by text message joins the authenticator app behind Base Step-up, how signing in becomes one more step-up operation, and how development tests both without a phone
**Last Updated:** 2026-10-19

> The `Agents` value is self-reporting: what the agent reports about itself.
>
> This record builds on [ADR 01](./01-agent-first-operations.md): one operation contract with four doors (section 1), step-up as a tenant policy over discovered operations with a 30-minute window renewed on use and capped at 6 hours (section 4), and prompt injection deferred (section 5). It does not repeat those decisions.

## Context

**The ask:** 2FA by text message (SMS), usable by any feature, for login and for sensitive actions such as changing a user's role or posting a journal entry.

Most of it already existed as Base Step-up (`apps/base/step_up/README.md`):

| Asked for | Already built |
| --- | --- |
| Action-level, agnostic | An owner declares an operation and calls `StepUp::require` from its own facade. Step-up names no caller |
| Auto-discovered operations | `bilibi_step_up::declare!`, collected at boot, listed on `/authz/step-up` |
| Optional per company | Per-tenant policy per operation: off, on, ask every time |
| Admin-only, audited settings | `admin.authz.step-up.manage`; changing the policy is itself step-up guarded and audited |
| 30 minutes renewed on use, 6-hour cap, configurable | Tenant settings `step_up.window_minutes` and `step_up.cap_hours`, within installation maxima |
| One contract, four doors | GUI, `/api/v1` and `/mcp` call the same contracts (ADR 01). The CLI client is not built |
| Role changes guarded | `base.authz.principal_role.set` and four other grant changes, on by default |

**What was missing:**

- A text-message method. The only second factor was an authenticator app (TOTP).
- A phone number on the account.
- Sign-in as a consumer. Sign-in issued a full session after the password.
- A way to test codes without a phone. Development (`BILIBI_ENV=development`) switched step-up off entirely, so no code was ever asked there.

## Decision

### 1. SMS is a second method, beside the authenticator app

**One method per account: the app when it is on, otherwise a text message to the account's phone number.**

- Core User's two-factor owns both methods (`apps/core/user/README.md`). Base Step-up stays method-agnostic, with one bound provider.
- The provider gains one call, `challenge`: a factor that delivers codes sends one to an agent's human when the agent owes a code. The refusal then says `code_sent`.
- The app stays the recommended method. SMS is weaker: SIM swap, interception and real-time phishing relays defeat it, and NIST SP 800-63B §5.1.3.3 treats codes over the phone network as a restricted authenticator.

**Example.** An agent commits a role change for Ada, who has a phone number and no app. Bilibi texts Ada a code and answers `step_up_required` with `reason: code_sent`. Ada reads the code to the agent, which commits the same plan again with `step_up_code`.

### 2. Base SMS owns delivery, through one transport bound at boot

- New Base module `base/sms` (`apps/base/sms/README.md`): `Message`, E.164 `PhoneNumber`, the `Sms` service and a `Transport` trait. It names no consumer.
- The only transport today is the development simulator. Staging and production bind none, so text messages are unavailable there and only apps work.
- A real provider is a later `Transport`: its credentials become encrypted installation settings with an editor, and its HTTP goes through `base/integration`. Twilio, Dialog and Notify.lk were named; none is chosen.

### 3. Code rules

| Rule | Value | Why |
| --- | --- | --- |
| Code | 6 digits from the OS CSPRNG, uniform | Typed on a phone |
| Stored | Argon2id hash only (the recovery-code hasher) | The database never holds a usable code |
| Lifetime | 5 minutes, used once | Short enough to make a stolen code stale |
| Wrong attempts | The 5th voids the code; the account's existing lock (5 failures in 15 minutes locks checks for 15 minutes) also applies | Guessing 1 in 10⁶ with 5 tries is out of reach |
| Resend | One per 60 seconds, at most 5 an hour, per account, rechecked under the account's row lock | Bounds cost and abuse per account |
| Text | Ends with `@<host> #<code>` (WICG origin-bound one-time codes) | A phone fills the code only on the real site |
| Undelivered | Voided | Nobody can enter what never arrived |

### 4. The phone number belongs to the account, and an administrator sets it

- `users.phone_number` (E.164) and `phone_verified_at`. It is a credential, so it is not the Person's contact phone, which is free directory text.
- Set on `/users/new` or in place on `/users/{id}`. A change is the step-up operation `core.user.phone_number.set` (on by default), because it redirects the account's codes. It voids outstanding codes and clears verification.
- The first correct code sent to a number verifies it: receiving the code proves the holder has the phone.
- An account with neither a phone nor an app cannot pass a check. Sign-in tells it to ask an administrator; nobody is locked out silently.

### 5. Sign-in is one more step-up operation

**`core.user.sign_in`, declared by Core User and off by default. A tenant turns it on like any other operation.**

1. A right password inserts the session **pending**. Every guard refuses a pending session (fail closed).
2. Core User asks Base Step-up on it. Policy off: the session completes at once. Policy on: the browser goes to `/login/verify`, in the sign-in page's layout.
3. The right code completes the session and opens its step-up window, so the next protected action within the window does not ask again.

**Example.** A tenant leaves sign-in off and keeps role changes on: its people sign in with a password and enter a code the first time they change a role, then not again for 30 minutes of activity.

### 6. Development enforces step-up, and a simulator stands in for the phone

- Every deployment enforces step-up, development included. The development exception is removed.
- Development binds the SMS simulator and serves its inbox at `/dev/sms-inbox`. Elsewhere the route is not bound at all.
- The inbox is anonymous, because a tester needs the code before signing in.
- Console walkthroughs that change grants, with nobody to give a code, turn the disposable fixture tenant's policy off through Base Step-up first (`bilibi_walkthrough_fixture::console_grants`). That write is audited like a change on the policy page.

### 7. Consumers today

| Operation | Owner | Default |
| --- | --- | --- |
| `core.user.sign_in` | Core User | Off |
| `base.authz.principal_role.set` and the four other grant changes | Base Authz | On |
| `core.user.phone_number.set` | Core User | On |
| A finance posting | The Finance Domain, when mounted: it declares its own operation and calls `StepUp::require` | Its own declaration |

No Finance Domain is mounted in this repository, so no change here is needed for it.

## Consequences

**Built:** `base/sms` with the simulator, the text-message method and its throttles, the phone number on accounts, sign-in step-up with pending sessions, the sign-in code page, the confirm page's text-message mode, the development inbox and enforcement in every deployment. The check is the running app: `docs/runtime-parity.md`.

**Open items:**

| Item | State |
| --- | --- |
| Real SMS provider | Not chosen. Staging and production have no text messages until one is |
| "Per company" | The ask said per company; Bilibi's policy is per tenant, and the companies inside one tenant share it. A per-company policy needs its own design |
| Self-service phone number | Not offered: the holder asks an administrator |
| CLI door | Not built (ADR 01) |

**Risks this decision accepts:**

- **SMS is a weak factor.** SIM swap and phishing relays defeat it. The app remains available and wins when on.
- **Dev servers.** Anyone who reaches a `development` server can read every code at `/dev/sms-inbox`, so text-message 2FA protects nothing there. Keep Dev servers private.
- **Agents can trigger texts.** An agent that owes a code makes Bilibi text the holder. The resend throttle bounds this to 5 an hour per account.
- **Cost.** With a real provider each text costs money. The throttle is per account, not per tenant.

### Future design topic: prompt injection and an open window

Deferred with ADR 01 section 5, and wider now:

- A code a human gives opens a 30-minute window on that agent connection. A prompt-injected agent can use the window for actions the human never reviewed.
- An injected agent can also ask the human for a fresh code and get it, since the text says only "do not share it".
- Nothing here mitigates either beyond the throttle, the audit trail and revocable grants.
- Directions to study: "ask every time" for agents on sensitive operations, naming the operation in the text, and binding a code to one reviewed plan.

## Prior art

- GitHub's sudo mode: a recent check covers sensitive actions for a while.
- NIST SP 800-63B §5.1.3.3: out-of-band codes over the phone network are a restricted authenticator.
- WICG, "Origin-bound one-time codes delivered via SMS".
