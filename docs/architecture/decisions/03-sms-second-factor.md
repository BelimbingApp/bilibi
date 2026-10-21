# ADR 03: SMS as a second factor, and sign-in as a step-up operation

**Document Type:** Architecture Decision Record
**Status:** Proposed
**Agents:** claude-code/opus-5.5 (author)
**Scope:** How a one-time code sent by SMS joins the authenticator app behind Base Step-up, how sign-in becomes one more step-up operation, and how SMS is delivered in development and testing before a real gateway is chosen
**Last Updated:** 2026-10-21

> Builds on [ADR 01](./01-agent-first-operations.md), section 4 ("Step-up is a policy, not a feature of one page") and section 5 ("Prompt injection is deferred"). Nothing here changes those decisions.

## Context

**Step-up already protects actions, not pages.** An owner declares an operation (`bilibi_step_up::declare!`) and calls `StepUp::require` from its own facade. Each tenant turns each operation off, on or "ask every time" at `/authz/step-up`. A check opens a window of 30 minutes, renewed on use and capped at 6 hours, the same for a person and for an agent acting for one. Every door (GUI, CLI, API, MCP) reaches the same facade, so no door can skip the check.

Two things were missing:

- **A second factor for people without an authenticator app.** Many users in our first market expect a code by text message.
- **Sign-in itself.** A stolen password opened a full session. A tenant could protect role changes but not the front door.

**Example.** A finance company turns on "Sign in" and keeps "Give or remove a user's role" on. A clerk signs in with email and password, receives `Your Bilibi code is 482913` on their phone, types it, and works. Thirty minutes later an administrator changes a role without being asked again, because the sign-in check opened the same window. A second company leaves "Sign in" off and only protects role changes.

## Decision

### 1. SMS is a second method of the one provider, not a second provider

- Base Step-up keeps exactly one bound `Provider` (Core User's `Authenticator`). It now accepts either an authenticator code or the latest SMS code. There are still no recovery codes: after a lost phone an administrator resets the account's authenticator or changes its phone.
- **Enrolled** means: a confirmed authenticator, or a verified phone while SMS is available.
- Step-up still names no consumer and no method. Owners change nothing.

### 2. The phone number belongs to the account

- `users.phone_number` holds E.164 (`+94771234567`), `users.phone_verified_at` when the holder last proved it.
- **An administrator sets it** (create form, or in place on the user page). Setting or changing it clears `phone_verified_at` and is itself a step-up operation (`core.user.account.phone.set`, on by default), so a stolen administrator session cannot quietly redirect someone's codes.
- **The holder verifies it** on `/settings/two-factor` by entering a code sent to it. The holder cannot change it, only an administrator can.
- **No verified phone and no authenticator:** the holder cannot complete a check. Pages say so and tell them to contact an administrator. Nobody is locked out silently.

### 3. SMS codes

| Rule | Value |
| --- | --- |
| Code | 6 digits from the OS CSPRNG, uniform |
| Storage | Argon2id hash only (the password hasher); the plain code exists only in the message |
| Life | 5 minutes, single use; a new code supersedes the previous one |
| Wrong attempts | 5 per code, then the code is void; every miss also counts toward the account's existing lock (5 failures in 15 minutes lock all checks for 15 minutes) |
| Resend | Once per 60 seconds, at most 5 per hour, per account |
| Audit | `user.two_factor.sms_sent`, `.verified`, `.failed`, `.locked`, `.phone_verified`, plus Step-up's own `step_up.*` rows |

### 4. Delivery goes through `base/sms`

- A new Base module, `base/sms`, owns outgoing text messages, as `base/mail` owns email: one validated `Message`, one `Sms` service, a `Gateway` trait.
- **Simulator gateway.** Dev and Staging servers write each message as a private file under `.local/sms/`. `/dev/sms-inbox` shows them like a phone screen, so a tester can sign in without a phone.
- **Production** has no simulator: the inbox answers 404 and `Sms::available()` is false until a real gateway is added. The role is the stored `instance.role`, read on each use, so the status-bar role switch applies without a restart. A tenant that turns on SMS-only checks in production before then sees "SMS is not available on this server".
- **A real gateway** (Notify.lk, Dialog, Twilio: to be chosen) is one more `Gateway` that sends through `base/integration` with its key as an encrypted setting, never an environment variable. It is out of scope here.

### 5. Sign-in is a step-up operation, off by default

- Core User declares `core.user.sign_in` ("Sign in"), **off by default**. Role changes stay on by default, as declared by Authz.
- A correct password now creates the session **pending a second factor**. Base Session refuses a pending session everywhere: no page, socket, API call or download accepts it.
- Core User then calls `StepUp::require` for `core.user.sign_in` on that session:
  - Off for the tenant, or enforcement off (development): the session is admitted at once.
  - Required: an SMS code is sent (SMS method) and the browser goes to `/login/verify`, built on the sign-in card. A correct code admits the session and opens its step-up window.
  - Not enrolled: the session is deleted and the sign-in card says to contact an administrator.
- A pending session lives at most 15 minutes. "Back to login" deletes it.

### 6. Agents get the code the same way

- When Step-up refuses an agent for a missing code and the holder's only method is SMS, it asks the provider to send one (`Provider::challenge`, throttled as above). The agent asks its human for the code and repeats the call with `step_up_code`, as for an authenticator code.

### 7. Prompt injection stays deferred

ADR 01 section 5 still holds. A new, specific risk is recorded here for that future design: **an agent's open step-up window can be misused by hostile content the agent reads**. After one approved code, an injected instruction could ride the remaining window (up to 30 minutes, renewed on use, 6-hour cap) to perform other protected operations. Options to study then: no window for agents (a code per action), a window bound to one plan, or a shorter agent window. Nothing in this record mitigates it.

## Consequences

- **Good:** one policy page lists every protected action, including "Sign in", grouped by module, with no list to maintain. A Domain that declares an operation gets SMS for free.
- **Good:** the check lives in the operation contract, so GUI, CLI, API and MCP behave the same.
- **Cost:** sign-in now writes the session before the second factor. The pending flag is the only thing that stops it being used, so every session read goes through Base Session's `resolve`.
- **Cost:** SMS is weaker than an authenticator (SIM swap, interception). Tenants that can should prefer the authenticator; both may be enrolled and either code works.
- **Open:** the real gateway; per-tenant choice of method; self-service phone change with a step-up check.
- **Open, found in testing:** with "Sign in" on, a user whose phone an administrator just set or changed (so unverified) and who has no authenticator cannot sign in, so cannot verify it. Today the only way out is turning "Sign in" off for the tenant while they verify. Options for the owner: let the first sign-in code to an administrator-set number count as its verification, or let an administrator issue a one-time sign-in bypass.

## Not done here

- **Tests.** Bilibi has no unit tests (`AGENTS.md`). The behaviour is proven in the running app: sign-in with and without the check, wrong and expired codes, the resend throttle, the window and cap, the per-company switch, a non-administrator refused the policy page, and the inbox absent in production.
- **Sample users with phones.** Bilibi keeps no demo seeds. Example data belongs in `fixtures/walkthrough`, loaded into a disposable database.
