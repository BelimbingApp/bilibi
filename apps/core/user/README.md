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

## Services (`core/user.accounts`, `UserService`)

| Method | Purpose |
|---|---|
| `login(email, password)` / `logout` | Bilimbi's global sign-in: the address alone names the account; the password is verified first, then the account's standing. Refusals are `CredentialError::{Rejected, CompanyArchived, WorkspaceUnavailable}` |
| `change_password(token, csrf, current, new)` | Signed-in change. Needs the current password; ends the account's other sessions |
| `request_password_reset(email)` | Always `Ok` for unknown, frozen or throttled accounts. Mail is sent after returning |
| `reset_password(email, token, new)` | Consumes the one-time token, replaces the password, ends every session; a frozen account's link is as invalid as an unknown one |
| `account(scope, user)` | One account of the tenant with its `AccountLinks`, version and timestamps; `admin.user.view` |
| `link_person(scope, user, person)` / `link_affiliation(scope, user, affiliation)` / `unlink(scope, user)` | The account binder's commands; `admin.user.update`. See below |
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
sorted; `url_hash` is md5 of it), at most 100, written under a lock on the
account's row. A URL that would leave the site is refused. Pins to a page this
deployment no longer serves stay stored and are not drawn.

Web adapters (`apps/web/src/shell.rs`): `POST /preferences` (`{kind,value}`), `GET /api/pins`, `POST /api/pins/toggle` (`{label,url,icon}`) and `POST /api/pins/reorder` (`{pins:[{id}]}`), each with Origin and CSRF checks and a 422 `{error}` for a refused value. The shell renders the account's theme (`<html data-theme>` on first paint), mode and pins (`data-pins`) itself, and `/settings/appearance` (`src/appearance.rs`) is the same three choices as a live form. It subscribes to committed theme and time zone display changes for the signed-in account and reloads the saved choices immediately, including changes from the top bar. An impersonated session reads the viewed account's choices but `PreferenceError::Impersonating` refuses every write (the shell answers 403 `{"error":"impersonating"}` and draws its controls locked), as Bilimbi does.

## Impersonation (K14; `src/impersonation.rs`)

`UserService::impersonate` and `leave_impersonation` are Bilimbi's
`BilimbiWeb.Impersonation`, behind the host's `POST /admin/impersonate/{id}` and
`/admin/impersonate/leave` (`apps/web/src/impersonation.rs`). Entering needs
`admin.user.impersonate`, a session that is not already impersonating, another
account, and a target whose `identity::standing` is live (an archived company is
refused). Each switch is one transaction through `Sessions::switch`: the session
must still hold the identity the request came from, its CSRF secret rotates, and
the retained `impersonation.started`/`impersonation.stopped` action (operator as
actor, target as subject, Bilimbi's payload) commits with it, so a trail that
cannot be written refuses the switch. A return that cannot be made ends the
session instead of trapping the operator. Every authentication re-proves both
accounts, so removing or freezing either ends the borrowed session.

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

## Account identity (K9)

An account optionally names a Person and, through one of that Person's
affiliations, the Organisation it signs in under: `users.person_id`,
`users.affiliation_id` and `users.organisation_id` (migration
`006-identity.sql`), the last two held consistent by the scoped foreign key
Affiliation published for this purpose, so a link can never name a Person or
company other than the affiliation's. The login email is unique platform-wide
(a partial unique index on `lower(email)` over live accounts); the migration
resolves the one duplicate the retired demo seed created explicitly and refuses
any other.

`identity::standing(client, tenant, user)` is the authentication-edge read:
the account row, then each stored link re-proved through its owner's public
read (`bilibi_company::eligibility::account_eligibility`,
`bilibi_affiliation::identity::account_standing`,
`bilibi_person::identity::account_standing`). It answers `Live(AccountIdentity)`,
`Unknown` or `Frozen(CompanyArchived | Identity)`, is read afresh every time and
is never written back as a derived status. `AccountVerifier`, bound into
`base/tenancy.authentication` at boot, answers every session check, live
refresh and bearer proof (`core/agent_access` calls the same `Authentication`)
with it, so a browser request and a token request carry the same sealed actor
facets (`Scope::actor().{person,organisation,affiliation}()`). Sign-in and
password reset call it before any session or token exists.

Freezing is Bilimbi's: an archived (or deleted) company ends its accounts'
ability to sign in, keep a session, redeem a reset link or use an agent grant
(`agent_access` refuses a grant whose recorded facets no longer match). Bilibi
adds the two states its model has and Bilimbi's lacks: an account whose
affiliation was unlinked or whose Person was retired is frozen the same way,
named "not attached to an active workspace" at sign-in. Pending and suspended
companies do not freeze accounts.

The link commands take the sealed scope and `admin.user.update`, run in one
`MutationTransaction` and honour the global lock order Company → Person →
Affiliation → User: `link_affiliation` locks both the account's current company
and the target's (`lock_writable_companies`), then takes the Affiliation owner's
`lock_for_account_binding` proof; `link_person` takes the Person owner's; both
refuse an affiliation of another Person (`LinkError::PersonMismatch`). An
archived current company refuses every command (`LinkError::Archived`): as in
Bilimbi, archive offers no restore or move, so the account stays frozen in
place. A change of organisation ends the account's sessions (announced after
commit), as Bilimbi's reassignment does. Each command is audited as kind `user`
with the old and new link ids. Authorization is tenant-scoped until K15 binds
company resource ownership; the account page (below) composes its link
controls from `account` and these commands.

Run the walkthrough against a fresh disposable migrated database (it installs
the walkthrough fixture):

```sh
BILIBI_IDENTITY_DEMO=disposable cargo run -p bilibi-user --example identity-walkthrough --locked
```

It prints the sealed facets of linked and unlinked sign-ins, the same facets
through the bearer path's `Authentication`, a denied and a refused link, unlink
and relink with the ended sessions, the archive of Company 002 freezing the
contractor's sign-in, open session, reset request and link commands, and the
unique-email refusal. It archives a fixture company, so never run it on a
working database.

## Company accounts (C11; `src/company_accounts.rs`)

The read other owners use to pick an account of one company (Production
Execution's labour picker; Bilimbi `User.list_company_users/2` and
`get_user/3`): `UserService::list_company_accounts(scope, company)` and
`company_account(scope, company, user)`, or the same `list`/`get` over a
caller's connection or transaction. Use these, not a query of `users`, so the
proof below stays in one place. Reads only and no capability, as in Bilimbi:
the caller authorizes the company target first. The scope's tenant bounds every
answer; a missing, deleted or other-tenant company or account is `NotFound`.

An account is listed only while `identity::standing`'s proof holds for the
company: live and `active`, its affiliation still linked and naming that
company, and its Person not retired. An account with no Person or affiliation
belongs to no company. An archived company stays readable with its accounts,
because archiving freezes sign-in, not history. Rows carry the Summary fields
(no credential is selected) plus the Person, affiliation and organisation ids,
ordered by id. Run the walkthrough against a fresh disposable migrated database
(it archives Company 002):

```sh
BILIBI_COMPANY_ACCOUNTS_DEMO=disposable cargo run -p bilibi-user --example company-accounts-walkthrough --locked
```

## Pages (`core/user.pages`)

`/settings/profile`, `/settings/password` (`src/web.rs`, `templates/password.html`), `/settings/two-factor` (below) and `/settings/appearance` are the signed-in account's own settings inside the shell, on the settings layout of `base/ui/templates/settings.html` with the typed `bilibi_ui::SettingsTab` (Bilimbi `SettingsComponents.settings_layout`, adapted to the shared horizontal tabs). Open **My settings** in the account panel at the bottom of the sidebar to start at Profile. Plugin for ChatGPT and Agent connections use the same six-tab layout in their owning modules. The password page is live: the form submits as the `change-password` event carrying its field values, the state keeps the field errors of the last attempt (`can't be blank`, the password policy, `does not match password`, `is incorrect`), and a success or failure is a flash in the page until dismissed. The route needs a session and no capability; the current password proves the holder inside `change_password`.

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

`/users/new` (`admin.user.create`; Bilimbi `FormLive`) creates an account
from the trusted administrator form: name, email, a plaintext password the
service hashes, and the optional party links of the approved model. The
Person select offers the live Persons the actor may list
(`admin.person.list`); once one is chosen the form offers that Person's
current affiliations (`admin.affiliation.list`) as "Signs in under", labelled
by company and title. `create_account` validates as Bilimbi's changeset does
(`can't be blank`, `must be an email address`, the password policy, `has
already been taken` for the platform-wide unique email), proves and locks the
links through their owners in the global order Company → Person →
Affiliation, and refuses an affiliation of another Person
(`AccountError::PersonMismatch`). Creation redirects to the account's page.
The tenant-wide `/users` index the Back link names is User Administration's
(K13).

`/users/{id}` (`admin.user.view`; Bilimbi `ShowLive`) is the read-first
page: the account as facts, each editing in place through the shared inline
editors for an actor holding `admin.user.update`.

- Name and email commit through `update_account`, which carries the row's
  `version` (migration `008-accounts.sql`): a stale draft is refused rather
  than applied over another operator's save, and a changed email is
  unverified again. The refusal is worded on the fact.
- Person and Company are choice facts (Bilimbi's company select) written
  through the account binder's commands: choosing a Person calls
  `link_person`, "None" calls `unlink`, and the Company select offers the
  Person's affiliations and calls `link_affiliation`; a note beside it says
  the change signs the account out of every session, as the binder does. The
  binder's refusals land on the fact (an affiliation of another Person, an
  archived company). A reader who may not list people or affiliations sees
  the facts read-only, with links to the records they may open.
- Roles & Permissions is Bilimbi's `UserAccessPanel` over Authz: assigned
  roles as chips, a Roles picker filtered by the escalation guard (a role is
  grantable only by an actor holding everything it grants;
  `bilibi_authz::roles::grants_of`), the Effective Permissions disclosure
  (`effective_capabilities_of`, grouped by domain; blue for a direct grant,
  red for a deny rule) with deny/remove controls and the Add Capabilities
  picker. Writes go through `Grants::assign_role` and `set_direct`, which
  authorize themselves; a change to the acting account's own access confirms
  first through the shared dialog. Each absent control states its reason
  where it would be.
- Change Password (`set_password`) needs no current password, ends every
  session of the account and audits a marker, never a hash.
- Affiliations lists the linked Person's current affiliations on the shared
  FlexTable (column settings, sort, row height), badges the one the account
  signs in under, and offers Unlink person behind the confirm dialog.
- Delete (`admin.user.delete`, `delete_account`) confirms through the shared
  dialog, never the actor's own account; the row is retired (`deleted_at`,
  status `deleted`) so audit history keeps its subject, the email is free
  again and the account's sessions end. The page returns to `/users`.
- An archived company makes the page read-only in every section, as
  Bilimbi's does: one warning under the header says why, and every write
  still re-proves the company through the binder's locks.

The header carries the record-history panel (`audit.record-history`, kind
`user`) and the pin control. Every command re-proves the actor, authorizes
inside its `MutationTransaction`, and is audited as kind `user`.

The tenant-wide `/users` index is `core/user_administration` (K13); Core
User contributes the `admin.user` menu entry to it and the `users` Grid table
(links `company` and `person`, inbound `users` and `accounts`), as Bilimbi's
Core User does.

## Account panels (K13; `src/panels/`)

Core User mounts four embeds by id on other owners' pages, as Bilimbi's
`company.users`, `employee.accounts`, `dashboard.users` and `dashboard.people`
do; the host page never names this module. Each read is one tenant-scoped
statement over `users` and proves the embed's capability again.

- `company.users` (record panel, kind `core/company`, `admin.company.view`):
  the accounts signing in under the company on the shared FlexTable
  (name, email, verified), searched, sorted and paged in the panel's own
  state. Read-only, as Bilimbi's.
- `person.accounts` (record panel, kind `core/person`, `admin.user.view`):
  the accounts that sign in as the Person, the company each signs in under
  and its verification. Under `admin.user.update` and while the Person is
  current, a select links an unlinked account (`link_person`) and each row
  unlinks behind the shared confirm dialog (`unlink`), the same binder
  commands the account page uses; the outcome is the panel's own notice.
- `dashboard.users` (entry `base-dashboard-user-stats`): the Users stat
  strip, total/verified/pending. `dashboard.people` (section `recent-users`):
  the first five accounts on the FlexTable with "All users". Both are offered
  by `admin.user.list` and read once per refresh period; without the
  capability they show an em dash and read nothing.

## Bootstrap administrator (`src/bootstrap.rs`)

The development seed's one-time `user_bootstrap` receipt names the platform
administrator and the operator company. `settle_bootstrap_affiliation` then
binds that account to the company the way every account names one: an agent
Person under the account's name, an affiliation with the company and
`link_affiliation_in` (the transaction-level form of `link_affiliation`), all
on one audited transaction as the administrator. `affiliation_settled_at`
marks the receipt settled, also when the account already named a Person, and
a settled receipt is never revisited, so a later unlink or retirement stands.
A refusal (the administrator lost the capability, the company is archived)
leaves it unsettled and the seed says why. Linking ends the account's open
sessions, as any company change does.

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
calls `settle_bootstrap_affiliation` for the company link. `operator_accounts(tenant)` reads the same account count and first email without writing, so the host decides whether an owner exists before it asks any question; where one does, `create-owner` only settles an unfinished link and names that owner. Use this, not an
`INSERT INTO users`, for any setup path that needs a first account.

## Two-factor authentication (the step-up provider)

Authenticator-app codes (TOTP, RFC 6238: HMAC-SHA-1, 6 digits, 30 s) for the signed-in account. It is the second factor behind Base Step-up (`apps/base/step_up/README.md`). Bilimbi has no counterpart yet.

- **Boundary:** this module checks the account holder and nothing else. It never names what step-up protects, and those owners never name it. They declare operations and call `StepUp::require`; tenant policy decides.
- **Code:** `src/two_factor/` (facade on `UserService`, provider in `provider.rs`), pages in `src/two_factor_ui.rs`, migration `012-two-factor.sql`.

| Method | Purpose |
|---|---|
| `two_factor_status(scope)` | On or off, and unused recovery codes |
| `begin_two_factor(scope, password)` | Password again, then a fresh 160-bit secret (OS CSPRNG), sealed and stored pending. Returns the QR code and typed key, once |
| `confirm_two_factor(scope, code)` | A current code turns it on, stores 10 new recovery codes (returned once) and opens the session's step-up window |
| `step_up(scope, code)` | A current code or one recovery code opens the session's step-up window (the confirm page) |
| `regenerate_recovery_codes(scope, code)` / `disable_two_factor(scope, code)` | Need a current code or a recovery code. The old set stops working / everything is deleted |
| `two_factor::Authenticator` | The `bilibi_step_up::Provider`, bound at boot: enrolled? session window, spend an agent's code for a purpose, where to prompt |

Pages (signed in only, live, `Cache-Control: no-store` like every page):

- `/settings/two-factor`: set up (password, QR code, setup key, first code), recovery codes with a text download, new codes, turn off.
- `/settings/two-factor/confirm?return=<path>`: the step-up prompt. Success returns to `return`, which must be a local path (`safe_return`).

Security rules:

- **Secret at rest.** `user_two_factor.secret` is the AES-256-GCM envelope of `Settings::seal_owned` (`BILIBI_SETTINGS_KEY`), bound to `core/user.two_factor`, tenant and account. Without the key, setup refuses.
- **QR code.** The `otpauth://totp/<issuer>:<email>?secret=…&issuer=…&algorithm=SHA1&digits=6&period=30` URI is drawn on the server as one inline SVG path (`qrcode` crate, encoder only). It is sent only over the page's socket. No QR service, image URL or Bilibi URL carries it, and nothing logs it.
- **Pending until proved.** A secret is unconfirmed until a current code arrives. Leaving setup keeps the pending row; the next setup replaces it.
- **Verification.** Steps −1, 0 and +1 are each computed and compared in constant time (`subtle`). `last_step` only moves forward, by compare-and-set in the spending transaction, so a code is never accepted twice, even by two racing requests.
- **Recovery codes.** 10 per set, 50 bits each (`xxxxx-xxxxx`, base32), stored as Argon2id hashes through `password::hash`. Every unused hash is checked. A used code keeps `used_at`.
- **Throttle.** Applies per account to every failed check: password, code, recovery code, and an agent's code too. The fifth failure within 15 minutes locks every check for 15 minutes (`user_two_factor_attempts`). A success resets the count.
- **Impersonation.** An impersonated session cannot enroll, change or check two-factor.
- **Audit.** Each write is a `MutationBoundary` transaction: `user.two_factor.enrollment_started`, `.enabled`, `.verified`, `.recovery_code_used`, `.recovery_codes_regenerated`, `.disabled`, `.failed` and `.locked`. A code spent for an agent records the purpose's operation key and channel (`for`, `channel`). A check refused while locked is the action `user.two_factor.refused`. No code, secret or recovery code is ever in a change, payload or log.

| Setting | Default | Use |
|---|---|---|
| `two_factor.issuer` (global, `admin.system.session.manage`) | `Bilibi` | The name apps list the account under (company or deployment name). No `:`; applies to new setups |

The step-up window, cap and per-operation policy belong to Base Step-up, not here.

## Audit

Credential writes are audited as kind `user` with marker values
(`"password_hash":"replaced"`), never a hash; the audit layer redacts the field
name as well. Reset and verification are attributed to the account that proved
the token.

## Known gaps

No throttle exists on guessing a password at login. Authorization of the link
commands is tenant-scoped (K15).

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

Saved records belong to `(tenant_id, user_id)` on every operation, including
integer IDs; a foreign owner's record is never returned. `QueryInput` contains
name (required, 150 characters), SQL (required, 100,000 characters), optional
slug, description, prompt and icon. A missing slug generates a unique readable
slug on creation and preserves the slug on update. Creation/duplication slug
selection is serialized per owner. Updates/deletes require the returned
`version` and refuse stale drafts with `Error::Conflict`. All saved-query
writes use the mutation boundary, so audit refusal rolls back the data. List
search/sort/page are SQL operations; page sizes are bounded to 300.

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

The operator's saved-query screens live at `/admin/system/database-queries`
(and `/{slug}`, with `/_new` for creation). They compose the shared list,
form, confirmation and FlexTable components in `src/query_ui`; all storage and
execution go through `database_queries::QueryService`. Both routes require
explicit operator identity plus `admin.system.database-table.list`; saving,
duplicating and deleting additionally require `admin.system.database-table.edit`.
Every live action and refresh checks these requirements again. Queries remain
private to their account, including for administrators.

The editor keeps SQL, prompt, description and named parameter values in its
mounted state. Run executes the current draft; result sorting, rows-per-page,
Back and page navigation retain that draft. A full reload loads saved SQL.
Parameters bind as PostgreSQL values (a blank input binds NULL). The result
pager of a saved query reflects `page` and `page_size` in the URL; periodic refresh does not rerun
SQL. Save uses the loaded version, so a competing save is refused without
replacing the draft. Deletion names the saved query in a shared confirmation
and checks that same saved version. Result values are escaped text; only column
names from the returned projection may be used for sorting. Read-only execution
and its success/refusal/failure audit remain C9's responsibility.
