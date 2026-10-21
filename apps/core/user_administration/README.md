# Core User Administration read contract

`core/user_administration` owns the bounded tenant-wide Users administration
index (Bilimbi `Core.UserAdministration`): one read facade, `list_users`, and
the capability-gated `/users` page over it (`admin.user.list`). Core User owns
every account command and the `/users/new` and `/users/{id}` pages the index
links to, the `admin.user` menu entry and the `users` Grid table. This crate
owns no migration, capability, menu entry or write.

## Visibility and options

The read is the scope's tenant: every live account of the tenant, with the
standing of the company it signs in under. Accounts at an archived (or
deleted) company stay visible with the company name and an `archived` badge,
and the index presents them read-only: no delete, no impersonation. A deleted
company is named without a link. Bilibi's model allows an account with no
company (the operator's agent account, a person not yet affiliated), which
Bilimbi's did not; such accounts are listed with no company rather than
hidden, so an administrator can still reach them. This is data visibility, not
actor authorization: the route guard checks `admin.user.list` on every request
and live event, and `list_users` proves it again.

`Options` is the strict normalized contract, Bilimbi's bounds: `search` is
`None` or 1–255 bytes; `role_ids` at most 100 unique positive ids with OR
semantics; `sort` one of name, email, company name or created; `page` 1
through 1,000,000; `page_size` exactly 25, 50, 100 or 300. Anything else is
`Error::Invalid`. Search is a parameterized contains pattern with `ILIKE` and
the typed `%`/`_` escaped, as Bilibi's other lists search (Bilimbi kept the
PHP `LIKE` wildcard meaning). Every ordering ends with the account id
descending, and an out-of-range page keeps the real total.

One facade call is one PostgreSQL statement: a CTE pipeline of tenant
companies, filtered users, the count, the ordered bounded page and
page-only role aggregation (`src/query.rs`). The application never receives an
unbounded tenant collection. A row's roles follow Bilimbi's
`company_visibility`: system roles (unowned) and the custom roles owned by a
company the actor may see (`Grants::visible_companies`, the same list
`bilibi_authz::roles::list` takes); the Roles filter offers the same roles.

## The page (`src/web.rs`)

`/users` is the shared flexible table hosted by Grid `PageColumns` (table
`users`; the arrangement, row height and lenses are remembered per account,
and an address that carries `cols`/`z` wins) over bounded URL state with
Bilimbi's names: `search`, `roleIds` (comma-separated), `page`, `perPage`,
`sortBy`, `sortDir`. Page sizes normalize to 25/50/100/300, anything else to
25; `""` and the PHP-falsey `"0"` disable search; changing search, roles, page
size or sorting resets to page 1. A first Created sort is descending; Name,
Email and Company begin ascending. Built-in columns are Name (initials avatar
in the normal row height only, record link under `admin.user.view`), Email,
Company (link under `admin.company.view`, `archived` badge), Roles (chips:
green for a system role) and Created; the add box also offers the walked
columns of the `users` Grid table and the scalar facts of the account page
(Person, Email Verified, Updated) through the User module's `record_fields`.

The toolbar is the search and the Roles multi-select (the tenant's first 100
roles by name). Create (`admin.user.create`) opens `/users/new`; the pin
control pins `nav-admin-user`.

Per row, under `admin.user.impersonate`, the impersonate action is a
submitting link to `POST /admin/impersonate/{id}`, Bilimbi's route, which the
impersonation host (K14) serves; it is disabled with the reason for the
actor's own account and an archived-company account. Under
`admin.user.delete`, Delete opens the shared confirm dialog for the listed
account; `delete` acts on that held account, never a client-supplied id, and
calls Core User's `delete_account`, which re-proves the actor, refuses the
actor's own account and an archived company, retires the row and ends its
sessions. Outcomes are the page's flash. An actor who may neither delete nor
impersonate sees no actions column.

## Checks

No unit tests. `cargo clippy --workspace --all-targets --locked -- -D warnings`
and the running app: migrate a disposable database, install the walkthrough
fixture, sign in as `admin@example.test` and open `/users`.
