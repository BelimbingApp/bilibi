# Base Step-up

Ask the human account holder to prove it is them again before an operation goes ahead, on any channel. GitHub calls this "sudo mode". No Bilimbi counterpart.

- **This module names no consumer** (no page, table, sign-in or role) and no second-factor method.
- Each owner declares its own operations and calls the facade from its own module.
- The second factor is a bound `Provider`. Today that is Core User's authenticator app (TOTP).

## Require step-up for an operation

Two lines in the owning module, nothing anywhere else.

1. Declare it (discovered like capabilities and menu items, validated at boot):

   ```rust
   bilibi_step_up::declare!(
       "domain/module",
       bilibi_step_up::Operation::new("domain.module.thing.approve", "Approve a thing")
   );
   ```

   - Key: lower case, prefixed by the owning module id with `/` as `.`; unique.
   - Label: what administrators read on the policy page (1–120 characters).
   - `.off_by_default()`: tenants turn it on. `.floor()`: on, and tenants cannot turn it off. `.always()`: a fresh code on every action, on every server, development included; no policy and no floor change can relax it. For an action that changes the server itself (Base Software's `base.software.change`), nothing else.

2. Call it in the facade, after authorizing and before writing:

   ```rust
   self.step_up.require(scope, KEY, &format!("thing:{id}")).await?; // Arc<StepUp> from SERVICE
   ```

   - `target` names what changes. It is audited, so put no secret in it.
   - A refusal is `Error::StepUpRequired { operation, reason }`. Return it unchanged: every channel already knows what to do with it.
   - A page route can wrap its access instead: `Access::StepUp { operation: KEY, inner: &Access::Owner }`.

- An operation may also guard one setting (`.guards_setting("key")`): the settings editor then asks for it before changing, clearing or restoring that setting.
- A module that is not mounted declares nothing, so its rows leave the policy page.
- Never decide in code whether step-up applies. That is the tenant's policy.
- A confirmation form that takes a code itself calls `require_code` for its `.always()` operation. A recent browser check, including enrollment, cannot replace a code on that submission. Missing codes are audited refusals; an account without 2FA receives the usual enrollment prompt.
- Call `require` as often as the action needs: a presented code is spent by the first call and covers the rest of the request.

## Policy

Tenants decide per operation on **Administration › Authorization › Step-up 2FA** (`/authz/step-up`, `admin.authz.step-up.manage`).

| Setting | Scope | Default | Bounds |
|---|---|---|---|
| Per operation: off, on, ask every time | Tenant (`step_up.policy`) | The declared default | — |
| Window after each protected action (page: "Minutes before asking again") | Tenant (`step_up.window_minutes`) | 30 minutes | 5–720 |
| Cap after the check (page: "Hours before always asking again") | Tenant (`step_up.cap_hours`) | 6 hours | 1–24, and at least the window |
| Floor: operations tenants cannot turn off | Installation (`step_up.floor`) | Declarations marked `.floor()` | — |
| Longest window / cap any tenant may set (page: "Longest time any organisation may choose") | Installation (`step_up.window_limit_minutes`, `step_up.cap_limit_hours`) | 720 minutes / 24 hours | Same bounds |

- Changing any of these is itself a step-up operation (`base.step_up.policy.update`, on the floor). Settings audits it.
- The installation owner sees the floor and the maxima on the same page.
- The stored Dev role turns ordinary enforcement off. Base System binds the live `Enforcement` policy, which reads `instance.role` on each check; changing the role takes effect without restarting. A missing binding enforces. A fixture binary with no authenticator may call `StepUp::set_enforced(false)` after installing its services; the host never does. An operation declared `.always()` is enforced everywhere, as "ask every time", and the policy page shows it as fixed.

## Proofs per channel

| Channel | Proof | Window |
|---|---|---|
| Web (browser session) | A check on the confirm page (`Provider::prompt`) | On the session row. Ends on sign-out, termination, expiry or an impersonation switch |
| Agent (`/api/v1`, `/mcp`) | `step_up_code`: the holder's current code, sent with the commit | Per agent connection and holder. Not shared with browsers or other agents |

- **Window:** sliding. Each protected action inside it restarts it. The cap counts from the check and wins.
- **Ask every time:** each action needs its own check. On the web, one check made within 5 minutes covers one such action. For an agent, a code per action.
- **Codes:** a presented code is verified and used in one step, for the operation and target of the first check that meets it. It then covers the rest of that one request, for example a role change that also gives full access, or every operation in one committed plan. In any later request the same code is refused.
- **Never:** an impersonating operator or an agent cannot satisfy step-up with anything but the account holder's own code.
- Signing the holder out everywhere (`Sessions::end_user_sessions`) also ends their agent windows.

## Agent flow

1. The agent commits a plan. A step-up operation answers `403` (API) or an `isError` tool result (MCP):

   ```json
   {"error":"step_up_required","operation":"domain.module.thing.approve","reason":"code_required","message":"…"}
   ```

2. The agent asks its human for the current code from their authenticator app.
3. It commits the same plan again with `"step_up_code": "123456"`. A failed commit leaves the plan code usable until it expires.
4. That opens the agent's window: later protected commits on that connection pass without a code until the window or cap ends, unless an operation asks every time.

`reason` is one of:

| Reason | Meaning |
|---|---|
| `code_required` | No code was sent |
| `invalid_code` | Wrong, expired or already used |
| `locked` | Too many failed codes |
| `not_enrolled` | The holder has no second factor yet |
| `every_time` | The operation asks for a code on every action |
| `window_expired` | The sliding window ran out |
| `window_capped` | The cap after the check was reached |
| `impersonating` | The request comes from an impersonated session |
| `unavailable` | No provider is installed |

On the web `required` replaces `code_required`. `refusal(operation, reason)` builds the answer body.

## Audit

Every check that applies writes one action:

| Event | When |
|---|---|
| `step_up.window_opened` | A code was accepted and opened a window |
| `step_up.passed` | A check passed; `proof` is `window` or `fresh_check` |
| `step_up.refused` | A check was refused; `reason` as above |
| `step_up.failed` | A code was refused; `reason` is `invalid_code` or `locked` |

- Each row carries the operation key, label, owning module, target, channel and mode.
- The actor is the account holder. An agent row adds `agent_connection` and `on_behalf_of`; an impersonated session adds the operator.
- No code is ever recorded. The provider writes its own audit of the check (`user.two_factor.*`).

## Provider

`StepUp::bind` once at boot. Core User does this in `src/two_factor/provider.rs`.

| Method | Answers |
|---|---|
| `enrolled` | Does the holder have a confirmed second factor? |
| `session` | The browser session's window (`SessionProof::Window`), or one fresh check (`SessionProof::Fresh`) |
| `spend` | Verify a code and use it, for the `Purpose` (operation, target, channel) |
| `prompt` | Where a browser checks in, or enrolls first, and comes back to `back` |

## Try it

On a migrated disposable database, select Staging in Instance settings (`BILIBI_ENV=staging` supplies it only during the first install seed):

- Enroll at `/settings/two-factor`. There are no recovery codes: after a lost phone another admin resets the account (`core.user.two_factor.reset`, `.always()`), or the server owner runs `bilibi-web two-factor-reset`.
- Open `/authz/step-up` and turn an operation on.
- `cargo run -p bilibi-web --example step-up-reference -- <user id> auditor assign` with `BILIBI_SESSION_TOKEN` set to a browser session's cookie. It prints `ok` or the `step_up_required` answer.

`StepUp::enrolled(scope)` reads the holder’s live enrollment state for shared `bilibi_ui::StepUp` forms. `required(scope, key)` answers whether policy protects a field without spending proof. These reads do not authorize a write; the owner still calls `require` or `require_code`.
