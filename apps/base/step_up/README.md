# Base Step-up

Ask the human account holder to prove it is them again before an operation goes ahead, on any channel. GitHub calls this "sudo mode".
- **This module names no consumer** (no page, table, sign-in or role) and no second-factor method.
- Each owner declares its own operations and calls the facade from its own module.
- Each second-factor method registers a `Provider`. Core User registers its authenticator app (TOTP); Extensions register beside it.

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
| Web (browser session) | A check on the confirm page (the chosen method’s `Provider::prompt`) | On the session row. Ends on sign-out, termination, expiry or an impersonation switch |
| Agent (`/api/v1`, `/mcp`) | `step_up_code`: the holder's current code, sent with the commit | Per agent connection and holder. Not shared with browsers or other agents |

- **Window:** sliding. Each protected action inside it restarts it. The cap counts from the check and wins.
- **Ask every time:** each action needs its own check. On the web, one check made within 5 minutes covers one such action. For an agent, a code per action.
- **Codes:** a presented code is verified and used in one step, for the operation and target of the first check that meets it. It then covers the rest of that one request, for example a role change that also gives full access, or every operation in one committed plan. In any later request the same code is refused.
- **Never:** an impersonating operator or an agent cannot satisfy step-up with anything but the account holder's own code.
- Signing the holder out everywhere (`Sessions::revoke`) also ends their agent windows.

## Agent flow

1. The agent commits a plan. A step-up operation answers `403` (API) or an `isError` tool result (MCP):

   ```json
   {"error":"step_up_required","operation":"domain.module.thing.approve","reason":"code_required","message":"…"}
   ```

2. The agent asks its human for the current code from their 2FA method.
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
| `unavailable` | No usable provider or session adapter is installed |
| `method_required` | More than one method is enrolled; qualify the code |

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

Register your method from its module's service factory:

```rust
step_up.register(
    bilibi_step_up::Method::new("example.factor.code", "Security code"),
    provider,
)?;
```

Declare `base/step_up` as both a Cargo and descriptor dependency. Registration orders methods by ID and rejects duplicate or malformed IDs at boot. **Never replace Core User's registration.** There is no Base dependency on an Extension.

| Provider method | Responsibility |
| --- | --- |
| `available(sql, account)` | Optional presentation read; defaults to true. A company method may hide its setup/choice until configured. It never grants enrollment. `FactorAccount::company()` is the current workplace for a scoped read, or `None` for account-wide lifecycle work |
| `enrolled(sql, account)` | Read live enrollment for the sealed `FactorAccount`; bind its tenant and user in every query, and its current company when the method is workplace-specific |
| `start(scope, purpose)` | Start or resend a challenge after an explicit action; return `Challenge` availability, masked destination and retry seconds |
| `spend(scope, code, purpose)` | Atomically validate and consume proof for that holder and exact operation, target and channel; own throttling and audit |
| `prompt(scope, back)` | Return the method's local browser prompt destination |
| `setup(scope, back)` | Return its local enrollment/management destination |
| `lifecycle(sql, account, event)` | Clean up inside the supplied owner transaction and return every affected record as a `Change` |

`enrolled`, `prompt` and `setup` are presentation reads. They must never create or send a challenge, consume proof, or write enrollment. `StepUp::methods` returns these live choices; Core User's settings and confirm pages use them. Prompt/setup destinations must be local paths, and the method must validate the return address again. Build method pages with Base UI components. The method owns its data and delivery through the existing Platform services.

Call `StepUp::start(scope, method_id, Purpose { operation, target, channel })` only for an explicit start/resend action. It refuses impersonation, unknown operations and a mismatched channel; an unenrolled method returns `Availability::Unavailable`. `Availability::RetryLater` and `retry_after_seconds` let a page explain a cooldown without delivering again; they are advice to the page, not enforcement. Base and Core User keep no start throttle, so a method's `start` must itself limit deliveries per account, across all pending sign-in entries and step-up targets, never per target. Anyone holding a leaked password can start a sign-in challenge for every new pending entry. Never include a plaintext code or destination in the result. Extensions expose any agent challenge operation through their own `agent_api` contribution, using this same facade.

Use `Method::new(id, label).code(length, numeric)` for a different code format (the default is six digits; lengths 1–64 are supported). Protected browser forms and the Software MCP card read `StepUp::enrolled_methods(scope)`, an enrollment-only list that never reads a method's `prompt` or `setup`, so a failing destination cannot break a page. They use the shared UI `ProofInput`, which builds its method choice and code fields from live enrollment and submits `method_id:code` without changing the owner’s event schema.

A single enrolled method accepts the existing plain `step_up_code`. With several enrolled methods, send `"method_id:code"`. Base routes to exactly one provider; it never tries the same code against every method. Missing enrollment and unknown methods fail closed. An accepted code still covers the remaining checks in the same request.

### Sign-in proof

`StepUp::verify(scope, method_id, code, purpose)` consumes one enrolled method's proof independently of session windows or policy bypasses. It validates the declared purpose and channel, audits the result, and returns `Spent`. It creates no window or session. Only the identity owner uses it to finish a separately sealed pending sign-in; ordinary protected actions continue to use `require` or `require_code`.

For `core.user.sign_in`, provider `start` and `spend` receive an internally proved holder scope without an admitted session row. Bind delivered codes to the complete purpose, including the pending-entry target. Finish verification through the provider's own audited transaction; the identity owner rechecks pending state before admission. The shared credential card reads only enrollment and code formats; it never follows signed-in setup or prompt routes.

### Shared browser proof

The identity owner binds `SessionProvider` once with `bind_session`, independently of factor registration. Core User supplies the existing session window and the neutral choice-page destination. A method opens browser proof with `Sessions::step_up` only after consuming its own valid challenge in an audited transaction. Factors do not implement session storage.

`SessionProof::Fresh { seconds, consume: false }` is a read only. `consume: true` spends the fresh check. `holds`, `required`, enrollment reads and destination rendering never send challenges or spend proof.

### Factor lifecycle

Owners call `StepUp::lifecycle(tx, user, event)` after authorizing inside their `MutationBoundary`. The tenant comes from that trusted transaction's context. Hooks run by method ID, and their changes share the owner's audit trace. An error, cancellation, capture failure or rollback prevents all cleanup from committing. Hooks must not open a transaction, commit, send notifications or perform external delivery.

- `Reset` removes confirmed and pending enrollment, pending challenges and method-owned proof/attempt state.
- `Delete` removes all method state for the deleted account.
- `Revoke` clears pending challenges and method-owned proofs while retaining enrollment and failed-attempt lockouts.

Core User calls reset and deletion hooks. `Sessions::revoke(tx, user, except)` calls revocation hooks before ending the account's browser sessions and agent windows. It returns session IDs for announcement after commit; never log them. The shared lifecycle records a bounded `step_up_lifecycle` marker even when no provider rows changed, so a failed or cancelled hook poisons the mutation boundary.

### Upgrade notes for agents

Extension owners implementing the former `Provider` must:

1. Replace `StepUp::bind(provider)` with `register(Method::new(id, label), provider)` from the Extension's service factory. Choose a stable, unique namespaced ID and declare the code format with `.code(length, numeric)` if it differs from six digits.
2. Move `session` to the identity owner's `SessionProvider`; ordinary factor Extensions implement no session adapter. Custom identity owners use `bind_session` and supply the neutral `prompt` destination too.
3. Change `enrolled(scope)` to `enrolled(&dyn ProofClient, FactorAccount)` and use the supplied connection, account tenant and user. Add side-effect-free `setup` and `prompt` destinations.
4. Implement explicit `start` returning `Challenge`, and transactional `lifecycle` returning all `Change` values. Bind delivered proof to the complete `Purpose`; keep phone numbers and delivery state in the Extension.
5. Handle `SessionProof::Fresh`'s new `consume` field if implementing the session adapter. Update account-wide sign-out callers from `Sessions::end_user_sessions(sql, tenant, user, except)` to `sessions.revoke(tx, user, except)` on a service wired with `with_factors(step_up)`.
6. Qualify codes as `method_id:code` when several methods are enrolled. Keep `require`/`require_code` at the protected owner's boundary.

Core User owns [pending sign-in admission](../../core/user/README.md#sign-in-admission). Its anonymous card uses these registered methods without opening a session. This contract adds no delivery method, secret reader or company policy.

## Try it

On a migrated disposable database, select Staging in Instance settings (`BILIBI_ENV=staging` supplies it only during the first install seed):

- Enroll at `/settings/two-factor`. There are no recovery codes: after a lost phone another admin resets the account (`core.user.two_factor.reset`, `.always()`), or the server owner runs `bilibi-web two-factor-reset`.
- Open `/authz/step-up` and turn an operation on.
- `cargo run -p bilibi-web --example step-up-reference -- <user id> auditor assign` with `BILIBI_SESSION_TOKEN` set to a browser session's cookie. It prints `ok` or the `step_up_required` answer.

`StepUp::enrolled(scope)` reads the holder’s live enrollment state for shared `bilibi_ui::StepUp` forms. `required(scope, key)` answers whether policy protects a field without spending proof. These reads do not authorize a write; the owner still calls `require` or `require_code`.
