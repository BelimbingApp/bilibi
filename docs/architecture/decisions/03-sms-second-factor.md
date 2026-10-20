# ADR 03: SMS one-time codes as a second factor, starting with a sandbox

**Document Type:** Architecture Decision Record
**Status:** Proposed. Built: the sandbox driver, phone numbers on accounts, SMS codes at sign-in and at every step-up check. Not built: a real SMS provider.
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How a person proves it is them with a code sent by text message, beside the authenticator app, and how text messages leave Bilibi
**Last Updated:** 2026-10-20

> The `Agents` value is self-reporting: what the agent reports about itself.

## Context

- ADR 01 (section 4) makes 2FA a per-company policy on any action, sign-in included. Until now the only second factor was an authenticator app (Core User, TOTP).
- Not everyone can or will install an authenticator app. A text message reaches any phone.
- There is no SMS provider account yet, and testing must not cost money or need a real phone.

## Decision

### 1. SMS is a second method, not a replacement

- A person may use the authenticator, SMS codes, or both. Wherever an authenticator code is accepted (sign-in, the Confirm access page, an agent's `step_up_code`), an SMS code is accepted too.
- Base Step-up still names no method. Core User's one provider accepts either code. "Enrolled" means either method is on.
- There are no recovery codes (PR #84). A lost phone: an admin's 2FA reset turns off the authenticator and SMS together. An SMS-only account's way back is an administrator entering a new number, which the person then proves.

### 2. Text messages leave through one Base module, chosen by env

- `base/sms` mirrors `base/mail`: a validated `Message`, one `Sms` service, a driver picked at boot from `SMS_DRIVER`.
- Today: `sandbox` (each message is a private file, read on `/dev/sms-inbox`) and `disabled`. Unset means sandbox on Dev and Staging, disabled on Production (by `BILIBI_ENV`); `sandbox` on Production refuses boot.
- A real provider is one more driver, sending through `base/integration`, with its own `SMS_*` variables. Env, not a setting, because delivery is chosen like mail's transport: per server, by its operator, at boot.

### 3. The number belongs to the account, and only its holder proves it

- `users.phone_number` (E.164) and `users.phone_verified_at`. An administrator may enter or change the number (user record, new-user form). The person may enter it on their 2FA settings page while SMS is off.
- A verified number is SMS being on. Proving it needs the password again and a code sent to it. Changing the number clears the proof.
- Contact phone numbers on a Person (`persons.phone`) are never used for codes: anyone who may edit contacts could otherwise redirect someone's codes.

### 4. Code rules

| Rule | Value |
|---|---|
| Code | 6 random digits (OS CSPRNG), stored only as an Argon2id hash |
| Lifetime | 5 minutes, single use, for its purpose (`verify` or `check`) and number |
| Wrong attempts | 5 per code, then it dies; failures also count toward the per-account lock (5 in 15 minutes) |
| Sending | Once per 60 seconds and 5 per hour per account; a new code ends the older ones |
| Agents | When an agent is refused for a code and the person's only method is SMS, Bilibi sends a code (within the bounds) |

## Consequences

- People without an authenticator app can meet a company's 2FA policy.
- SMS is weaker than an authenticator app (SIM swap, interception). A per-company switch to forbid SMS is a possible later change; it is not built.
- Testing needs no phone: the development inbox shows every code.
- Going live needs a provider account and a driver; the pages and the rules do not change.
- Deferred, as in ADR 01: prompt injection misusing an agent's open window.
