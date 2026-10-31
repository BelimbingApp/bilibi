# User credentials

`core/user` owns accounts, their Core identity links, credentials,
notifications and the account's own appearance and pins: the account binder
(`src/identity.rs`, below), the password and email services, the account's
self-service settings pages that call them, the `/notifications` page and the
top-bar bell (the `shell.notifications` embed), the last two in `src/inbox.rs`
over `src/notifications.rs`. Other modules notify through `base/notifications`
(`NotificationBoundary::notify` with a typed `Notice` and recipients, inside
their own transaction); run `examples/notifications-walkthrough.rs` against a
disposable database to see the after-commit fan-out. The anonymous sign-in,
forgot-password and reset-password
pages belong to the Web host, as Bilimbi's do.

Role assignments on `/users/{id}` use `base.authz_agent.principal_role.set`, the same reviewed operation as the agent. The page retains its account checks; the operation checks role authority and step-up at commit.

Base Records kind `user` names and links staff accounts through the same tenant, non-deleted, staff-kind and `admin.user.view` rules as the account detail page. Customer accounts are not exposed through that kind.

## Services (`core/user.accounts`, `UserService`)

| Method | Purpose |
|---|---|
| `login(email, password)` / `logout` | Bilimbi's global sign-in: the address alone names the account; the sign-in lock is checked, the password verified, then the account's standing. Refusals are `CredentialError::{Rejected, CompanyArchived, WorkspaceUnavailable}`. A required second factor returns a pending credential, not a session; see [Sign-in admission](#sign-in-admission) |
| `change_password(token, csrf, current, new)` | Signed-in change. Needs the current password; ends the account's other sessions |
| `request_password_reset(email)` | Always `Ok` for unknown, frozen or throttled accounts. Mail is sent after returning |
| `reset_password(email, token, new)` | Consumes the one-time token, replaces the password, ends every session; a frozen account's link is as invalid as an unknown one |
| `account(scope, user)` | One account of the tenant with its `AccountLinks`, version and timestamps; `admin.user.view` |
| `link_person(scope, user, person)` / `unlink(scope, user)` | The account binder's commands; `admin.user.update`. See below |
| `create_account` / `update_account` / `set_password` / `delete_account` | Account administration (`src/accounts.rs`): `admin.user.create`, `.update`, `.update`, `.delete`. See "Account administration" |
| `update_profile(token, csrf, name, email)` / `landing_pages` / `landing_choice` / `save_landing` / `landing_route` | The signed-in account's own profile and landing page; no capability |
| `send_email_verification(token, csrf)` / `verify_email(link)` | Signed link to the account's current address; clicking proves mailbox control |

Refusals are `CredentialError` variants a page maps to a field error or flash
(`IncorrectPassword`, `Policy`, `InvalidOrExpiredToken`, ...). Unknown, frozen,
expired, reused and wrong-email tokens are one indistinguishable
`InvalidOrExpiredToken`.

Use `password::{hash, verify, check_policy}` for any password work instead of
calling `argon2` directly. It runs Argon2id on the blocking pool, bounds input
(8 characters to 1024 bytes) and `verify(.., None)` costs the same as a real
check, so timing does not reveal which emails exist.

## Company sign-in

Optional company OIDC, linking, provisioning, role mapping and provider re-authentication are documented in [Company sign-in](sso.md). With no enabled connection, password sign-in and existing factor settings keep their behavior.

## Automatic provisioning

Companies can create a SCIM token to provision people and groups, update mapped roles and end access immediately. See [Automatic provisioning (SCIM)](scim.md). With no token, SCIM requests return 401 and existing sign-in behaviour stays unchanged.

## Sign-in admission

Enable **Sign in** on the tenant's 2FA requirements page to ask for a second factor before admitting a session.

- `core.user.sign_in` is off by default. It follows the existing Step-up policy and instance enforcement: ordinary checks are skipped in Dev. Both On and Ask every time require a fresh code at each sign-in.
- `UserService::login(email, password)` verifies the password and live account standing. `Login.pending == false` means its token is an admitted session. `true` means it is only a pending credential; never install that token as a session or expose it to a live page.
- `pending_sign_in(token)` returns only CSRF and live enrolled method formats. It exposes no `Scope`. `start_sign_in_challenge(token, csrf, method)` explicitly starts or resends delivery. `complete_sign_in(token, csrf, method, code)` consumes that method's proof and returns a fresh admitted session, or `None` for a refused proof. `cancel_sign_in(token, csrf)` ends pending entry.
- Pending credentials are hashed in `user_sign_ins`, outside `sessions` and agent grants. Normal session, API and live guards cannot authenticate them. Admission locks the pending row and rechecks expiry, standing, password version and the current workplace's company sign-in policy, so parallel submits admit at most once and a password challenge cannot fall into an enforcing workplace. Company sign-in keeps its provider company through the pending factor and admits the session in that workplace. Once pending, sign-in always requires proof even if policy is later turned off.
- The operator setting **Sign-in challenge lifetime** (`user.sign_in_minutes`) defaults to five minutes, with a range of 1–30. Expiry uses the database clock, never extends on resend, and is recorded once when the expired credential is next presented. Cancelled, expired, revoked and admitted credentials cannot be reused.
- Password verification, pending entry, challenge starts, admission, cancellation and observed expiry are audited through `MutationBoundary`, without credentials or codes. Providers audit their own checks; Base Step-up also records proof acceptance or refusal for `core.user.sign_in`. Reset, account deletion and session revocation end pending sign-ins through Core User's lifecycle adapter.
- With no enrolled method, a required sign-in stays pending and explains how to ask an administrator for help. It never admits, enrolls a method or sends a challenge from a GET. Before resetting the last method, keep another administrator signed in to turn off the tenant's sign-in requirement while the holder enrolls again. Reset does not bypass that policy.

Factor Extensions use the [Base provider contract](../../base/step_up/README.md#provider). Sign-in calls the same `start` and `spend` hooks with operation `core.user.sign_in`, a pending-entry ID as target, and channel Web. These hooks must work without a session row. Delivered codes must bind to that complete purpose. Core User does not throttle `start_sign_in_challenge`, and each password sign-in creates a new pending entry. A method's `start` must therefore throttle deliveries per account across all pending entries and step-up targets; `retry_after_seconds` is advice to the page, not enforcement. Providers finish their own proof transaction before Core User begins admission; an admission failure may consume a code but never create a session.

### Upgrade notes for agents

- Replace the free `login(&db, email, password)` with `users.login(email, password)` (or `login(&users, ...)`). The service carries the installed factor registry and policy.
- Check `Login.pending` before treating its token as a session. Custom sign-in adapters must continue through the pending methods above and admit only the fresh token returned by `complete_sign_in`.
- Replace `bilibi_user::identity::standing` and `identity::AccountLinks` with the crate-root exports `bilibi_user::standing` and `bilibi_user::AccountLinks`.
- Factor providers must accept the sign-in purpose without assuming an admitted session. Keep factor setup and ordinary step-up prompt routes signed in; the shared anonymous challenge card handles sign-in.

## Appearance and pins

The signed-in account's theme, time-display mode, locale and sidebar pins
(`src/preferences.rs`; Bilimbi `DisplayPreferences`, `Pins`). Every call takes
the sealed session `Scope`, so a request can never name another account, and
all of it is keyed by the current tenant + account IDs: membership is untouched
and nothing moves when K9 binds Person/Affiliation.

| Method | Purpose |
|---|---|
| `presentation(scope)` | One snapshot: theme, time-display `mode` + company `timezone`, resolved `locale`/`language` and the account's own locale choice |
| `save_preference(token, csrf, kind, value)` | `theme` (`light`/`dark`/`system`), `timezone` (`company`/`local`/`utc`) or `locale` (empty follows the installation). Unsupported kinds and values are `PreferenceError::Invalid`, never guessed |
| `list_pins(client, scope)` / `toggle_pin(..)` / `reorder_pins(..)` | Pin or unpin a page (audited), renumber the order (a display preference, so not audited) |

Theme is the Settings value `ui.theme` at the account's scope (`system` is the
absence of a row); time-display mode and locale are the existing
`ui.timezone.mode` and `ui.locale` values written through their owners
(`base/datetime`, `base/locale`). Pins are `user_pins`: one row per normalized
URL per account (path with trailing slash, fragment and host dropped, query
sorted; `url_hash` is md5 of it), up to 8192 bytes per URL and at most 100 pins, written under a lock on the
account's row. A URL that would leave the site, or an icon name the UI catalogue does not know, is refused. Pins to a page this
deployment no longer serves stay stored and are not drawn.

Web adapters (`apps/web/src/shell.rs`): `POST /preferences` (`{kind,value}`), `GET /api/pins`, `POST /api/pins/toggle` (`{label,url,icon}`) and `POST /api/pins/reorder` (`{pins:[{id}]}`), each with Origin and CSRF checks and a 422 `{error}` for a refused value. The shell renders the account's theme (`<html data-theme>` on first paint), mode and pins (`data-pins`) itself, and `/settings/appearance` (`src/appearance.rs`) is the same three choices as a live form. It subscribes to committed theme and time zone display changes for the signed-in account and reloads the saved choices immediately, including changes from the top bar. An impersonated session reads the viewed account's choices but `PreferenceError::Impersonating` refuses every write (the shell answers 403 `{"error":"impersonating"}` and draws its controls locked), as Bilimbi does.

## Impersonation (K14; `src/impersonation.rs`)

`UserService::impersonate` and `leave_impersonation` are Bilimbi's `BilimbiWeb.Impersonation`, behind the host's `POST /admin/impersonate/{id}` and `/admin/impersonate/leave` (`apps/web/src/impersonation.rs`). Entering needs `admin.user.impersonate`, a session that is not already impersonating, another account, and a target whose `bilibi_user::standing` is live. A disabled account or retired Person is refused; an archived workplace removes only that desk. Each switch is one transaction through `Sessions::switch`: the session must still hold the identity the request came from, its CSRF secret rotates, and the retained `impersonation.started`/`impersonation.stopped` action (operator as actor, target as subject, Bilimbi's payload) commits with it, so a trail that cannot be written refuses the switch. A return that cannot be made ends the session instead of trapping the operator. Every authentication re-proves both accounts, so removing or freezing either ends the borrowed session. The viewed account starts at its remembered or first live desk. Its desks remain the reach boundary. The operator's already authenticated live session satisfies the workplace sign-in check during this authorized impersonation; no target-provider session is required. The start action audits entry into the view, and each later desk change records `user.company.switched` with both identities.

## Current company

`UserService::choose_company(token, csrf, company, request)` handles the shell's **Switch desk** form. It proves the requested desk, writes the session choice and the account's remembered `last_desk_id`, and records `user.company.switched` through one MutationBoundary transaction. A refused switch records `user.company.switch_refused`. The choice grants no capability.

Every authentication proves the selected desk again. An invalid or absent selection uses the remembered valid desk, then the person's first live desk, or no desk. Impersonation uses the viewed person's desks and parks the operator's selection separately. A queued job retains its captured desk and refuses lost access rather than selecting another one.


## Token storage

`password_reset_tokens` has one row per account (primary key tenant + user), so
it can never grow past the user count. Only the SHA-256 of the 256-bit random
token is stored; the plaintext exists in the emailed link alone. A request is
throttled to one per 60 seconds, a link lives one hour, and each request
deletes expired rows. Consuming a token deletes its row inside the audited
transaction, so a racing second use loses. If delivery fails the row is
removed so the person can ask again.

Verification links are stateless: `v1.{tenant}.{user}.{issued}.{hmac}` signed
with `BILIBI_USER_SIGNING_KEY` (64 hex characters; separate from the Settings
and Media keys). The MAC covers the current email, so changing the address voids
links already sent. Without the key `send_email_verification` and `verify_email`
answer `VerificationUnavailable`.

Emailed links point at `APP_ORIGIN`: `/reset-password/{token}?email=…`,
served by the Web host, and `/verify-email/{link}`, whose page belongs to a
later slice.

## Account identity

**A staff account belongs to its tenant and optionally names a Person.** Its workplaces are the person's explicit desks, never an account-owned company or employment affiliation. Migration `core-user-025-tenant-accounts` turns existing development staff company links into first desks, clears those links and constrains staff rows to tenant/person identity. No grants become desks and no other conversion is performed.

`standing(client, tenant, user)` re-proves the live account and Person on every authentication, returning `Live(AccountIdentity)`, `Unknown` or `Frozen(Identity)`. `AccountVerifier` supplies the same identity to browser sessions and agent authentication. Retiring the Person or disabling the account ends eligibility. Archiving a workplace removes that desk from use; it does not freeze a tenant's staff accounts or their other desks.

Customer accounts retain their separate portal identity ([ADR 16](../../../docs/architecture/decisions/16-customer-accounts-and-portal.md)). A business customer's affiliation and company are still re-proved through their owners, including archival and effective dates. This identity never supplies staff work access. Staff email remains unique platform-wide; customer email is unique within its tenant.

`link_person` and `unlink` take `admin.user.update`, lock the Person and account, and write one audited mutation. Changing the linked Person ends the account's sessions because its desks and identity have changed. Company access is managed through [Desks](#desks), not these identity commands.

## Company accounts

**Use `UserService::list_company_accounts(scope, company)` and `company_account(scope, company, user)` for staff pickers.** A caller already holding a database transaction uses the static `UserService::list_company_accounts_on(client, scope, company)` and `company_account_on(client, scope, company, user)` variants to preserve that connection. These read live staff accounts whose live Person has an explicit desk at that live, unarchived company. The caller authorizes its own company target; every answer remains within the sealed scope's tenant.

`CompanyAccount` carries id, name, email, verification, Person, the requested organisation and timestamps. It contains no credential or employment affiliation. Staff without a desk are not listed. Business affiliations neither add nor remove workplace access.

## Pages (`core/user.pages`)

`/settings/profile`, `/settings/password` (`src/web.rs`, `templates/password.html`), `/settings/two-factor` (below) and `/settings/appearance` are the signed-in account's own settings inside the shell, on the settings layout of `base/ui/templates/settings.html` with the typed `bilibi_ui::SettingsTab` (Bilimbi `SettingsComponents.settings_layout`, adapted to the shared horizontal tabs). Open **My settings** in the account panel at the bottom of the sidebar to start at Profile. AI connections uses the same five-tab layout in `core/agent_access`. The password page is live: the form submits as the `change-password` event carrying its field values, the state keeps the field errors of the last attempt (`can't be blank`, the password policy, `does not match password`, `is incorrect`), and a success or failure is a flash in the page until dismissed. The route needs a session and no capability; the current password proves the holder inside `change_password`.

`/settings/profile` (`src/account_ui/profile.rs`, Bilimbi `ProfileLive`) is
the account's own name, email and landing page. The account is the
session's, never a submitted id; `update_profile` normalizes the email,
refuses a taken one (`has already been taken`) and leaves a changed address
unverified, which the saved flash says. The landing page is the user-scope
setting `ui.landing_menu_id` (`accounts::LANDING_KEY`, contributed in
`contributions.rs`): the select offers exactly the menu pages the account can
open (`landing_pages`, the menu contributions read through the account's
effective capabilities), `save_landing` refuses any other value, and the
Web host's sign-in asks `landing_route` for where to go, falling back to the
workspace when the choice is unset or no longer reachable.

## Account administration (K16; `src/accounts.rs`, `src/account_ui/`)

`/users/new` (`admin.user.create`; Bilimbi `FormLive`) creates a tenant account from name, email and a password, plus an optional existing Person and first desk. Without an existing Person it creates one through Core Person. The first desk defaults to the selected workplace. User creation, Person creation and the desk commit together through MutationBoundary. The account stores no staff company or affiliation.

`/users/{id}` (`admin.user.view`; Bilimbi `ShowLive`) is the read-first
page: the account as facts, each editing in place through the shared inline
editors for an actor holding `admin.user.update`.

- Name and email commit through `update_account`, which carries the row's
  `version` (migration `008-accounts.sql`): a stale draft is refused rather
  than applied over another operator's save, and a changed email is
  unverified again. The refusal is worded on the fact.
- Person is an inline choice: `link_person` assigns it and `unlink` removes it. A changed Person ends the account's sessions. The same **Desks** panel used on the person's record appears on the account record.
- Roles & Permissions is Bilimbi's `UserAccessPanel` over Authz: assigned
  roles as chips, a Roles picker filtered by the escalation guard (a role is
  grantable only by an actor holding everything it grants;
  `bilibi_authz::roles::grants_of`), the Effective Permissions disclosure
  (`effective_capabilities_of`, grouped by domain; blue for a direct grant,
  red for a deny rule) with deny/remove controls and the Add Capabilities
  picker. Role writes use `base.authz_agent.principal_role.set`; direct-capability
  writes use `Grants::set_direct`. Both authorize themselves; a change to the acting account's own access confirms
  first through the shared dialog. Each absent control states its reason
  where it would be.
- Change Password (`set_password`) needs no current password, ends every
  session of the account and audits a marker, never a hash.
- Delete (`admin.user.delete`, `delete_account`) confirms through the shared
  dialog, never the actor's own account; the row is retired (`deleted_at`,
  status `deleted`) so audit history keeps its subject, the email is free
  again and the account's sessions end. The page returns to `/users`.


The header carries the record-history panel (`audit.record-history`, kind
`user`) and the pin control. Every command re-proves the actor, authorizes
inside its `MutationTransaction`, and is audited as kind `user`.

The tenant-wide `/users` index is `core/user_administration` (K13); Core
User contributes the `admin.user` menu entry to it and the `users` Grid table
(link `person`, inbound `accounts`), as Bilimbi's
Core User does.

## Account panels (K13; `src/panels/`)

Core User mounts four embeds by id on other owners' pages, as Bilimbi's
`company.users`, `employee.accounts`, `dashboard.users` and `dashboard.people`
do; the host page never names this module. Each read is one tenant-owned
statement over `users` and proves the embed's capability again. Dashboard
summaries filter to the current workplace; record panels keep their explicit
company, Person or tenant target.

- `company.users` (record panel, kind `core/company`, `admin.company.view`):
  the accounts whose Person has a desk in the company on the shared FlexTable
  (name, email, verified), searched, sorted and paged in the panel's own
  state. Read-only, as Bilimbi's.
- `person.accounts` (record panel, kind `core/person`, `admin.user.view`):
  the accounts that sign in as the Person, their live desks
  and email verification. Under `admin.user.update` and while the Person is
  current, a select links an unlinked account (`link_person`) and each row
  unlinks behind the shared confirm dialog (`unlink`), the same binder
  commands the account page uses; the outcome is the panel's own notice.
- `dashboard.users` (entry `base-dashboard-user-stats`): the Users stat
  strip, total/verified/pending. `dashboard.people` (section `recent-users`):
  the current workplace's first five accounts on the FlexTable with "All users".
  Both are offered by `admin.user.list` and read once per refresh period; without the
  capability they show an em dash and read nothing.

## Bootstrap administrator (`src/bootstrap.rs`)

The development seed's one-time `user_bootstrap` receipt names the administrator and the operator company. `settle_bootstrap_affiliation` composes the initial agent Person, its business contact affiliation and an explicit desk in one audited transaction. The staff account stores only its Person. The settled receipt is never replayed after a later unlink or retirement. A refused first desk leaves the receipt unsettled.

`create_first_administrator(grants, operator, input)` is the production
counterpart of the seed's bootstrap (Bilimbi `AdminBootstrap.run/1` with the
operator's own name, email and password), behind `bilibi-web create-owner`
(`apps/web/src/create_owner.rs`, documented for operators in
`docs/server-install.md`). On one `MutationBoundary` transaction as the
`create-owner` system principal (`Context::system`, since no account exists
yet to act as) it locks `users` as Bilimbi does, proves the given tenant and
company are the marked platform operator and its primary company, refuses
with `FirstAdministratorError::AccountsExist` while that tenant holds any
undeleted account (so it never promotes or replaces one), validates with the
administrator form's rules, inserts the account, takes the grant-all role
through Authz's `Grants::grant_first_administrator_in`, and writes the
receipt (replacing one whose account was deleted, the only way one can
remain). The transaction's changes are `user`, `authz.principal_role` and
`user.bootstrap`; it also records the retained `user.bootstrap.completed`
action with the account, email and role, as the seed does. The host then
calls `settle_bootstrap_affiliation` for the Person, business affiliation and first desk. `operator_accounts(tenant)` reads the same account count and first email without writing, so the host decides whether an owner exists before it asks any question; where one does, `create-owner` only settles an unfinished bootstrap and names that owner. Use this, not an
`INSERT INTO users`, for any setup path that needs a first account.

## Two-factor authentication (the step-up provider)

Authenticator-app codes (TOTP, RFC 6238: HMAC-SHA-1, 6 digits, 30 s) for the signed-in account. It is the second factor behind Base Step-up (`apps/base/step_up/README.md`). Bilimbi has no counterpart yet.

- **Boundary:** this module checks the account holder and nothing else. It never names what step-up protects, and those owners never name it. They declare operations and call `StepUp::require`; tenant policy decides.
- **Code:** `src/two_factor/` (facade on `UserService`, provider in `provider.rs`), pages in `src/two_factor_ui.rs`, reset doors in `src/two_factor/agent.rs`, migrations `012-two-factor.sql` and `013-drop-two-factor-recovery.sql`.

| Method | Purpose |
|---|---|
| `two_factor_status(scope)` / `account_two_factor_on(scope, user)` | The holder’s authenticator status / whether any registered method is enrolled for one account in the tenant |
| `begin_two_factor(scope, password)` | Password again, then a fresh 160-bit secret (OS CSPRNG), sealed and stored pending. Returns the QR code and typed key, once |
| `confirm_two_factor(scope, code)` | A current code turns it on and opens the session's step-up window |
| `step_up(scope, code)` | A current code opens the session's step-up window (the confirm page) |
| `begin_move(scope, code)` / `confirm_move(scope, to, code)` | Move to a new phone: a current code from the old phone is spent and a new secret is made (kept only in the page's state). The new phone's first code replaces the old secret, so the old phone stops working |
| `reset_two_factor_by_email(email)` | The server command's door (see below): turns off every registered method for one account, audited as the system principal `two-factor-reset` |
| `two_factor::Authenticator` | The registered `core.user.authenticator` provider and shared `SessionProvider`; contract: `apps/base/step_up/README.md` |

Pages (signed in only, live, `Cache-Control: no-store` like every page):

- `/settings/two-factor?method=core.user.authenticator`: set up (password, QR code, setup key, first code), or move to a new phone (a code from the old phone, a new QR code, the new phone's first code). There is no turn-off: Bilibi asks everyone for 2FA. The description is two bullets; the second says to ask an admin to reset 2FA after a lost phone.
- `/settings/two-factor/confirm?method=core.user.authenticator&return=<path>`: the authenticator step-up prompt. Success returns to `return`, which must be a local path (`safe_return`).

Authenticator entry uses the shared six-digit `CodeInput`: one native field, numeric keypad, OTP autocomplete and whole-code paste. This method’s entries take an authenticator code. With multiple registered methods, the settings and confirm routes first show a neutral choice; each method supplies its own setup and prompt destinations.

Security rules:

- **Secret at rest.** `user_two_factor.secret` is the AES-256-GCM envelope of `Settings::seal_owned` (`BILIBI_SETTINGS_KEY`), bound to `core/user.two_factor`, tenant and account. Without the key, setup refuses.
- **QR code.** The `otpauth://totp/<entry name>:<email>?secret=…&issuer=<entry name>&algorithm=SHA1&digits=6&period=30` URI is drawn on the server as one inline SVG path (`qrcode` crate, encoder only). It is sent only over the page's socket. No QR service, image URL or Bilibi URL carries it, and nothing logs it.
- **Pending until proved.** A secret is unconfirmed until a current code arrives. Leaving setup keeps the pending row; the next setup replaces it.
- **Verification.** Steps −1, 0 and +1 are each computed and compared in constant time (`subtle`). `last_step` only moves forward, by compare-and-set in the spending transaction, so a code is never accepted twice, even by two racing requests.
- **No recovery codes.** People lose where they saved them, so there are none (migration `013-drop-two-factor-recovery.sql` drops the table). After a lost phone, another admin resets the account's 2FA, or the server owner runs the command below.
- **Throttle.** Applies per account to every failed check: password, code, and an agent's code too. The fifth failure within 15 minutes locks every check for 15 minutes (`user_credential_attempts`, kind `step_up`; the shared rule is `src/credential_attempts.rs`). A success resets the count.
- **Impersonation.** An impersonated session cannot enroll, change or check two-factor.
- **Audit.** Each write is a `MutationBoundary` transaction: `user.two_factor.enrollment_started`, `.enabled`, `.verified`, `.moved`, `.failed` and `.locked`. A reset is `core.user.two_factor.reset` (an admin; the change names both people) or `user.two_factor.reset` (the server command, system principal `two-factor-reset`). A code spent for an agent records the purpose's operation key and channel (`for`, `channel`). A check refused while locked is the action `user.two_factor.refused`. No code, secret or recovery code is ever in a change, payload or log.

**Authenticator entry name.** Each server lists itself differently, so several Bilibi servers stay apart in one app: `Bilibi <instance name> (<role>)` with the account's email, or `Bilibi <role>` when the instance still has its role's own name. Both come from Base System's instance identity (`Instance::resolve`); there is no separate setting or input box, and the app lets a person rename the entry.

**Reset 2FA.** There are two ways back after a lost phone:

- An admin with `core.user.update` opens the person's page and chooses **Reset 2FA** (contract `core.user.two_factor.reset`, `src/two_factor/agent.rs`; the page and every agent door share it). It needs the admin's own fresh authenticator code every time (`.always()`). The audit row names both people, the person is signed out, and gets a notification and a mail (when mail is set up). An admin cannot reset their own 2FA, and an impersonated session cannot reset anyone's.
- The server owner runs `bilibi-web two-factor-reset --email <address>` (or `scripts/install-server.sh two-factor-reset <instance> --email <address>`) on the server. It is audited as **System · two-factor-reset**, signs the account out and notifies it. See `docs/server-install.md`.

The person then signs in with their password and sets the app up again.

The step-up window, cap and per-operation policy belong to Base Step-up, not here.

## Audit

Credential writes are audited as kind `user` with marker values
(`"password_hash":"replaced"`), never a hash; the audit layer redacts the field
name as well. Reset and verification are attributed to the account that proved
the token.

## Sign-in attempt limits

Two limits, both operator settings (Settings › Session), with these defaults:

| Limit | Default | Setting |
|---|---|---|
| Wrong passwords per account before a lock | 5 within 15 minutes | `user.sign_in.max_failures`, `user.sign_in.window_minutes` |
| Lock length | 1 minute, doubling with each lock in a row, up to 15 | `user.sign_in.lock_minutes` |
| Attempts per sender address, any account | 10 per minute | `user.sign_in.per_sender_per_minute` |

- **One counter:** wrong passwords share `user_credential_attempts` with two-factor (migration `018-credential-attempts.sql`), kind `sign_in`, counted apart from kind `step_up` so guessing at the form never blocks the owner's own 2FA checks. One window/lock rule: `src/credential_attempts.rs`; sign-in side: the password step in `src/identity.rs` (`password_identity`) and the limit in `src/sign_in.rs`, before any second-factor challenge.
- **A stranger cannot keep it locked:** a wrong password while locked is not counted and never extends the lock.
- **Hidden from strangers:** a locked account gets the same answer as a wrong password, even with the right one; unknown addresses are not counted per account (the per-sender limit covers them). Over the per-sender limit the form says "Too many sign-in attempts", naming no account.
- **Visible to the owner and administrators:** failures and locks are audited (`user.sign_in.failed`, `user.sign_in.locked`, actor the system principal `sign-in`); the owner is emailed a reset link when a lock starts; an administrator sees "Locked until" on the account page and can Unlock (`admin.user.update`, `user.sign_in.unlocked`).
- **Ends:** when the lock expires, after a password reset or an administrator's unlock. A successful sign-in forgets the count and the run of locks.

## Known gaps

Authorization of the link commands is tenant-scoped (K15).

## Walkthrough

`cargo run -p bilibi-user --example credentials-walkthrough` against a
disposable database after `migrate` (set
`BILIBI_DISPOSABLE_CREDENTIALS_DEMO=yes`, `BILIBI_MAIL_DIR` and
`BILIBI_USER_SIGNING_KEY`). It prints every outcome above and reads the dev
mailbox to follow the links.

## Saved database queries (C9; pages follow in C10)

Resolve `database_queries::QueryService` as `core/user.database_queries`. Use
`list`, `get(QueryKey::Id|Slug)`, `create`, `update`, `delete`, `duplicate` and
`execute` from [src/database_queries.rs](src/database_queries.rs), rather than
running editor SQL or writing `user_database_queries` directly. Each call
revalidates the account and explicit platform-operator identity. Reading and
executing require `admin.system.database-table.list`; saving, duplicating and
deleting require `admin.system.database-table.edit`. Admin grants alone do not
make a tenant the operator. The UI must also gate routes and events (C10).

Saved records belong to `(tenant_id, user_id)` on every operation, including integer IDs; a foreign owner's record is never returned. `QueryInput` contains name (required, 150 characters), SQL (required, 100,000 characters), optional slug, description and icon. A missing slug generates a unique readable slug on creation and preserves the slug on update. Creation/duplication slug selection is serialized per owner. Updates/deletes require the returned `version` and refuse stale drafts with `Error::Conflict`. All saved-query writes use the mutation boundary, so audit refusal rolls back the data. List search/sort/page are SQL operations; page sizes are bounded to 300.

`execute` accepts `ConsoleRequest` (SQL, display name, page, per-page) and
`ConsoleOptions` (named parameters, optional output-column sort and direction).
Use `extract_named_parameters` to build the editor's parameter fields. It skips
quoted strings/identifiers, escaped strings, dollar quotes, casts and comments.
Named `:value` parameters bind once by name in PostgreSQL's inferred text input
format, so `:id::bigint`, `:active::boolean`, dates and text work without SQL
interpolation. An absent/None parameter binds NULL. Parameter values never enter
the audit payload. `ConsoleAnswer` has the outcome, optional result and durable
action ID; a refused/failed command has no result. The editor passes unsaved SQL
and retains its draft across result paging. Output sorting is accepted only for
an exact returned column, duplicate column names retain separate values, and
arbitrary-precision JSON numbers preserve exact PostgreSQL numeric output.

The executor uses PostgreSQL `READ ONLY` with a 10-second statement timeout,
repeatable-read count/page snapshot, at most 1,000 result rows per page and
explicit rollback. Text guards are advisory; a function that tries a hidden
write is refused by PostgreSQL with SQLSTATE 25006. Every execution success,
refusal (including authorization) and failure is recorded before an answer;
audit failure returns no rows. Audit contains bounded SQL/name, status, guard
and count, never result rows or raw database error details (which could quote
data or parameter values). It cannot record during a total database outage and
therefore fails closed. Error details remain available in the operator's answer.

Run `cargo run -p bilibi-user --example database-queries-walkthrough` against a
**disposable** migrated database with `DATABASE_URL` and
`BILIBI_DISPOSABLE_QUERY_DEMO=yes`. This opt-in manual fixture prints CRUD,
concurrent slug creation, stale refusal, owner/tenant isolation, parameters,
column preservation, pagination, SQL/DB refusals, timeout, audit rollback and
private-result exclusion; it is not a unit-test suite. It explicitly makes
fixture tenant 1 the operator, grants fixture capabilities and installs private
fixture functions/triggers. Never run it against normal data.

The operator's saved-query screens live at `/admin/system/database-queries` (and `/{slug}`, with `/_new` for creation). They compose the shared list, form, confirmation and FlexTable components in `src/query_ui`; all storage and execution go through `database_queries::QueryService`. The list's column labels come from the detail's `query_ui::fields`; SQL text stays on the detail page. Both routes require explicit operator identity plus `admin.system.database-table.list`; saving, duplicating and deleting additionally require `admin.system.database-table.edit`. Every live action and refresh checks these requirements again. Queries remain private to their account, including for administrators.

The editor keeps SQL, name, description and named parameter values in its mounted state. Run executes the current draft; result sorting, rows-per-page, Back and page navigation retain that draft. A full reload loads saved SQL. Parameters bind as PostgreSQL values (a blank input binds NULL). The result pager of a saved query reflects `page` and `page_size` in the URL; periodic refresh does not rerun SQL. Save uses the loaded version, so a competing save is refused without replacing the draft. Deletion names the saved query in a shared confirmation and checks that same saved version. Result values are escaped text; only column names from the returned projection may be used for sorting. Read-only execution and its success/refusal/failure audit remain C9's responsibility.

The query editor uses the shared page header for its title, description and Back link. Query name and description use shared form fields; Discard, Save and saved-query actions sit in the form action row. Migration `core-user-014-drop-query-prompt` removes the unused prompt column while preserving saved queries and their SQL.

The [Base Step-up provider contract](../../base/step_up/README.md#provider) owns method registration, challenge starts and lifecycle hooks. Core User registers TOTP beside Extensions. With one method the existing routes retain their original flow. Reset (including the server command) reads enrollment across all methods in the owner's transaction, removes all factor state, revokes challenges and signs out the target. Account deletion also calls every method's deletion hook. Password changes/resets and account reassignment use `Sessions::revoke`; they retain enrollment. Every provider change shares the owner's mutation transaction and audit trace. Sign-in admission is described under [Sign-in admission](#sign-in-admission).

Saved notification, pin and database-query icons use the bare names in the [UI icon contract](../../base/ui/README.md#icons). Migration `core-user-019-bare-icon-names` removes the former CSS prefix from existing values and maps the duplicate impersonation name to `bilimbi-impersonate`. Unknown stored names render the shared neutral placeholder and warn once. Migration `core-user-021-database-table-pin-icons` sets the `database` icon on stored database table pins that had none.

## Desks

**A desk lets a staff person choose a workplace; their capabilities decide what they can do there.** Every desk is explicit; there is no own-company exception. `user_desks` stores assignments, with tenant-scoped keys to Person and Company; no grants are converted into desks. Company managers can assign themselves, audited in the same way as any assignment.

`core.user.desk.set` is the one reviewed operation behind **People with a desk here** on company records and **Desks** on person records. Its input is `company_id`, `person_id` and `assigned`. Preview checks company management and a live person with a staff account; commit repeats those checks and locks the company and person before writing a `user.desk` Change. Customer accounts never acquire staff access.

Core User binds Company's `WorkAccess` provider at boot. The switcher and every session choice check use the same desk proof. Removing a desk revokes that company's provider-sourced roles and ends the person's sessions in the same transaction; provider-authorized SCIM updates and deactivation remain available without the desk. Archiving a company uses the same owner path to remove every desk there and revoke every provider-sourced role tied to it, while ending only sessions working in that company. Company-record administration still requires its action capability and either the current desk company or the tenant-wide management capability; extra desks do not widen it. See [ADR 19](../../../docs/architecture/decisions/19-company-work-access-and-page-scope.md).
