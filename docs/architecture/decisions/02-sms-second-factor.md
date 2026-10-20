# ADR 02: Text-message codes as a second 2FA method, and sign-in as a step-up consumer

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How an account proves itself with a code sent by text message (SMS) beside the authenticator app, and how sign-in asks for a code under each tenant's policy
**Last Updated:** 2026-10-20

> The `Agents` value is self-reporting: what the agent reports about itself. Its effort level is unknown; ask the user who ran the harness before this record is accepted.
>
> Builds on [ADR 01](./01-agent-first-operations.md), which already records the four doors, step-up as a tenant policy, the 30-minute window with a 6-hour cap for humans and agents alike, and the deferred prompt-injection risk. This record does not restate them.

## Context

- Step-up (`apps/base/step_up`) already guards actions, not pages. Owners declare operations, tenants choose per operation, and one bound provider checks the person. Core User's authenticator app (TOTP) was the only method.
- Some people cannot or will not install an authenticator app. Adopters asked for codes by text message.
- Sign-in asked for a password only. A tenant could not require a code at sign-in, though it could for other actions.
- Bilibi had no way to send a text message.

## Decision

### 1. One provider, two methods

- Core User stays the one bound `Provider`. It accepts a code from **either** method the account has: the authenticator app or a verified phone.
- Step-up still names no method. It gains two small, method-agnostic pieces:
  - **`Provider::challenge`**: a code is owed by a caller that cannot be sent to a prompt (an agent). A method that delivers codes sends one. A method the person carries (the app) does nothing. The default does nothing.
  - **`StepUp::applies(scope, key)`**: whether a tenant's policy asks for a check, readable before any session exists.
- A person may have both methods. A six-digit code is tried against the app first, then against the latest text-message code. Recovery codes stay with the app.

### 2. Text-message codes

| Rule | Value |
|---|---|
| Code | 6 digits, uniform, from the OS CSPRNG |
| Storage | Argon2id hash only, with the number it went to and its purpose (`verify_phone` or `check`) |
| Life | 5 minutes, used once; a new code ends the earlier ones |
| Wrong tries | 5 per code, then it ends. The account-wide lock (5 failures in 15 minutes locks 15 minutes) applies on top |
| Resend | Once per 60 seconds, at most 5 per hour, per account |
| Phone | One per account, E.164. A second factor only once its holder accepted a code sent to it |

- **The holder adds a number** in My settings: password again, a code to the new number, then the code back.
- **An administrator may set or change a number** on the user's page. It is then unverified until the holder confirms it. Changing a phone and **resetting 2FA** are a new step-up operation, `core.user.two_factor.reset` (on by default). A reset deletes the app and its recovery codes, unverifies the phone and ends the account's sessions.
- A person with no verified method is told to contact an administrator. Nobody is locked out silently.

### 3. Sending: Base SMS, simulator first

- New module `base/sms`: one validated `PhoneNumber` (E.164) and one `Gateway` with a `Driver` interface. Callers never talk to a provider.
- **The only driver today is the simulator.** It writes each message to a private file. `/dev/sms-inbox` shows them like a phone screen, so testers can sign in without a phone.
- The simulator runs in `development` and `staging` only. In `production` the gateway is unavailable and `/dev/sms-inbox` is not routed at all. SMS is refused rather than pretended.
- A real provider is a second driver that sends through `base/integration`, with its credentials as encrypted operator settings. The provider (Twilio, Notify.lk, Dialog or other) is chosen when an account exists. Bilibi does not ship a placeholder driver.

### 4. Sign-in is one more consumer

- Core User declares `core.user.sign_in` ("Sign in"), **off by default**. It appears on Administration › Authorization › Step-up 2FA with every other operation, per tenant.
- When it is on, a right password creates a **pending sign-in** instead of a session: a random token in its own cookie (`Path=/login`, 10 minutes), only its SHA-256 stored. `/login/verify` takes the code. Only an accepted code opens the session.
- The code goes through `StepUp::require` with the code presented, exactly as an agent's code does. The policy, the provider, the lock and the audit are the shared ones.
- An account whose only method is a phone gets its code at once; "Resend code" respects the limits.
- The accepted sign-in code also opens the new session's step-up window.

### Defaults

| Operation | Default |
|---|---|
| Sign in (`core.user.sign_in`) | Off |
| Change a user's 2FA (`core.user.two_factor.reset`) | On |
| Role and capability changes (existing, `base/authz`) | As declared there |
| Finance operations | Declared by the Domain that adds them; none is mounted here |

## Consequences

- SMS is the weaker method. SIM-swap and interception can defeat it; NIST SP 800-63B calls it restricted. Tenants who need more should keep the app, or a later security-key provider.
- Text messages cost money per message. The limits bound that, and the cost of a pending sign-in is one message.
- A tenant that turns sign-in 2FA on blocks its own members who have no method until they set one up or an administrator helps. The policy page should warn about this; it does not yet.
- Agents: a `step_up_required` answer now also texts the holder a code when the holder has a phone, within the same limits.
- Production has no SMS until a real driver lands.

**Open items:**

| Item | State |
|---|---|
| Real SMS driver | Not built; needs a provider choice and an account |
| Sign-in attempt limit on the password itself | Not built (`apps/web/src/auth.rs` says so) |
| Phone field on the create-user form | Not built; set it on the user's page after creating |
| Live countdown on "Resend code" | The server states the wait; no ticking timer |
| Warning on the policy page before turning sign-in 2FA on | Not built |
| Prompt injection | Still deferred (ADR 01 section 5) |
