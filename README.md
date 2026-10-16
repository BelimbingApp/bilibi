# Bilibi

Bilibi is an MIT-licensed Rust platform for building business applications. It supplies the shared foundation (companies, users, permissions, audit) so a team builds only what its business needs.

- Rust, Axum, Askama, PostgreSQL 18.
- Live server-rendered pages. No SPA framework.
- Agent-operable: people and AI agents act through the same checks.
- Modular: Base, Core, Domains, Extensions.
- Companies own their customisations as ordinary, version-controlled source.

## Who it is for

- Teams that want a business application without rebuilding sign-in, tenancy, roles and audit.
- Companies that want to own and change their software, not rent a fixed product.
- Developers who work with coding agents and want conventions the agents can follow.

## Principles

- **Start beyond the boilerplate.** Tenants, companies, people, accounts, roles, settings, mail, audit and notifications ship in the platform.
- **AI-native.** An agent operation boundary exposes search, describe, guide and read. Agent connections use OAuth device codes and per-person tokens. An agent request carries the same actor and permission checks as a browser request.
- **Company-owned customisation.** A company adds its own Domain and Extension repositories. The Platform stays unmodified and updatable.
- **Self-contained modules.** Each module owns its routes, templates, SQL and migrations. Modules reach each other through declared APIs and optional ports.
- **Tenant-safe by default.** Tenant comes from the session, never from client input. Every application query binds data and filters by tenant.
- **Evidence over trust.** Capability checks and mutations are written to append-only audit tables in the same transaction.
- **Scan-first UI.** Compact, warm, consistent pages for long working sessions. See `DESIGN.md`.

## For agents and designers

- [`AGENTS.md`](AGENTS.md): how agents build, test and change this repository.
- [`DESIGN.md`](DESIGN.md): the UI design rules.
- [`apps/domains/AGENTS.md`](apps/domains/AGENTS.md): mounting Domain repositories.
- [`apps/extensions/AGENTS.md`](apps/extensions/AGENTS.md): mounting Extension repositories.

## What is in it today

- **Sign-in and sessions**
  - Server-held random sessions. Argon2id passwords. 24-hour expiry.
  - Forgotten-password and reset pages. Reset ends all sessions.
  - Password change at `/settings/password`.
  - Session and tenant are re-checked on every request, live event and push.
  - No sign-in attempt throttle yet.
- **Authorization**
  - Roles, capabilities and direct grants. Direct denies override every allow.
  - Every capability check writes a tenant-scoped decision-log row.
  - Field restrictions mask values before rendering. Admin at `/authz/field-restrictions`.
  - See [Authz](apps/base/authz/README.md).
- **Companies**
  - Tenant-scoped, soft-delete-aware list: search, status filter, sort, 25/50/100 rows.
  - Detail with inline edits. A row lock and version check reject stale edits.
  - Accounts link to a Person and a company. Archiving a company freezes its accounts.
  - See [User](apps/core/user/README.md).
- **Audit**
  - Old and new values plus actor, in the same transaction as the save.
  - Append-only. Secrets redacted by field name.
  - See [Audit](apps/base/audit/README.md).
- **Notifications**
  - Saved changes create a durable notification and push the bell of the recipient only.
  - `/notifications` lists them with All, Unread and Read tabs.
- **Mail**
  - Messages go to `.local/mailbox/` (newest 100) unless `MAIL_HOST` sets an SMTP relay.
  - Email verification links need `BILIBI_USER_SIGNING_KEY` (64 hex characters).
  - See [Mail](apps/base/mail/README.md).

## Requirements

- Stable Rust 1.99
- Node.js 22 or later with npm (only to rebuild CSS and JS)
- PostgreSQL 18 server and client tools
- Caddy, running with its admin API at `http://localhost:2019`
- mkcert
- Python 3.11 or later

## Quick start

Ubuntu or WSL2:

```sh
sudo apt install caddy mkcert libnss3-tools postgresql-18 postgresql-client-18 python3
cargo install cargo-watch --version 8.5.3 --locked
./scripts/start-app.sh
```

Then open <https://bilibi.localhost> and sign in.

| Field | Value |
|-------|-------|
| Email | `ai@agent.my` |
| Password | `bilibi-dev` |

What `./scripts/start-app.sh` does:

- Checks prerequisites.
- Starts a private PostgreSQL 18 in `.local/postgres18` on port 5438, or uses your `DATABASE_URL`.
- Discovers modules, builds assets and compiles.
- Migrates and seeds.
- Configures local HTTPS and watches for changes.

Watch behaviour:

- Changes under `apps/`, workspace manifests, the lockfile and `scripts/` rebuild, migrate and restart.
- An open page shows "Rebuilding…", reconnects with backoff, then reloads.
- Restart the launcher after changing `DATABASE_URL`, `APP_ORIGIN`, `BIND_ADDR`, `PG18_BIN` or `BILIBI_DEV_PORT`.
- Ctrl-C stops the app. Caddy keeps running.
- Build timings: `target/cargo-timings/cargo-timing.html`.

## Local HTTPS

- A mkcert certificate is loaded into the running Caddy through its admin API.
- The launcher changes only its own tagged route, TLS policy and certificate.
- Certificate files live in `.local/https`. No shared Caddy file is edited.
- Backend port is 4300, or the next free port.
- Helper scripts: `scripts/local-https.sh`, `scripts/local-https-api.py`.

## Manual HTTP run

```sh
export DATABASE_URL='postgres://bilibi:bilibi@127.0.0.1:5438/bilibi'
export APP_ORIGIN='http://localhost:3000'
python3 scripts/discover-modules.py
cargo run -p bilibi-web -- migrate
cargo run -p bilibi-web -- seed
cargo run -p bilibi-web -- run
```

- Open <http://localhost:3000>. Same sign-in as above.
- `scripts/dev-run.sh` is the watched build, migrate and run step the launcher calls.
- Migrations and seeding never run automatically on app boot.

## Cloud and agent machines

- `.agents/setup`: one-time setup of Rust, PostgreSQL 18 and a warm `target/` on a Debian 12 machine.
- `.agents/resume`: per-session step. Puts cargo on `PATH` and starts the private PostgreSQL on port 5438.
- Both are idempotent and share the layout of `scripts/start-app.sh`.

## Configuration

| Variable | Default and use |
|----------|-----------------|
| `DATABASE_URL` | Private instance on port 5438 when unset |
| `BIND_ADDR` | `127.0.0.1:3000` |
| `APP_ORIGIN` | Must match the browser origin exactly, no trailing slash |
| `PG18_BIN` | PostgreSQL 18 binaries directory |
| `BILIBI_DEV_PORT` | Launcher backend port, default 4300 |
| `MAIL_HOST` | SMTP relay. Unset writes to `.local/mailbox/` |
| `BILIBI_USER_SIGNING_KEY` | 64 hex characters, for email verification |

- POST requests and WebSocket upgrades require the `APP_ORIGIN` Origin.
- HTTPS origins set Secure cookies. Terminate TLS at a reverse proxy.
- PostgreSQL connections use `NoTls`. Use a trusted private connection.

## Seed data

- `seed` runs production seeds, then provisions once:
  - tenant `Bilibi local development`
  - company `Bilibi Development`
  - `ai@agent.my` with the grant-all `core_admin` role
  - sample departments
- The bootstrap runs once. Re-running keeps the account, password and roles.
- A database with other accounts or a platform operator, but no such administrator, is refused.
- `seed-production` runs only the production seeds.
- Older databases with demo data are cleaned by the next `seed`, which prints each change ([`retire-demo.sql`](apps/web/src/retire-demo.sql)).
- Walkthroughs never use a seeded database. They install the [walkthrough fixture](fixtures/walkthrough/src/lib.rs) into a fresh, migrated database.
- To run it: `cargo run -p bilibi-walkthrough-fixture` against that database.

## Module layout

Each module is a Cargo crate under one of four roots.

| Root | Holds |
|------|-------|
| `apps/base/` | Foundations: database, audit, authz, mail, workflow, UI, module registry |
| `apps/core/` | Core business modules: user, company, person, address, affiliation, agent access |
| `apps/domains/` | Optional business domains. Empty here. Mounted as nested checkouts |
| `apps/extensions/` | Company extensions. Empty here. Mounted as nested checkouts |
| `apps/web/` | Executable host: routes, sockets, explicit seed |
| `fixtures/walkthrough/` | Example-only data. Never a product dependency |

Dependency rule: Base, then Core, then Domains, then Extensions. Lower layers never depend on higher ones.

Notable Base modules:

- `module_registry`: discovered module graph and typed service registry
- `database`: pool, migrations ledger, mutation boundary
- `audit`: evidence schema, capture, redaction, SQL console engine
- `authz`: vocabulary, live grants, decisions, masking
- `agent_api`: operation boundary for agents
- `ui`: shell, credential layout, styles, small live browser adapter

How modules compose:

- Each module submits a `bilibi_module_registry::Module` descriptor through `inventory`.
- The descriptor names ID, layer, dependencies, migrations and service factories.
- `python3 scripts/discover-modules.py` generates composition files before Cargo resolves packages. The launcher runs it. CI checks it.
- Boot rejects missing or duplicate modules, upward dependencies, cycles, duplicate migrations and conflicting service names.
- A service factory sees only its own module and declared direct dependencies.
- See [module registry](apps/base/module_registry/README.md).

## Domains and Extensions

- A Domain is its own repository, `BelimbingApp/bilibi-dom-<id>`.
- Mount it as a real nested checkout at `apps/domains/<container>`. Not a submodule, not a symlink.

```sh
git clone <repository URL> apps/domains/<container>
python3 scripts/discover-modules.py
```

- Never commit mounted composition files or `Cargo.lock` changes to the Platform.
- Reach another container through an optional port (`apps/base/ports/README.md`).
- Domains own no CSS or JavaScript.
- Full rules, unmounting, CI and build pinning: [`apps/domains/AGENTS.md`](apps/domains/AGENTS.md).
- Extensions mount under `apps/extensions/`. See [`apps/extensions/AGENTS.md`](apps/extensions/AGENTS.md).

## Live pages

- Each connection owns its route, list parameters and rendered page.
- Sort, search and paging events reload through the same APIs as HTTP.
- The browser patches server HTML by element id. Focus, scroll and drafts survive.
- Intents are queued and acknowledged by request ID.
- Lost connections reconnect with backoff and remount the same URL.
- A 15-second refresh catches suspension, expiry and role changes.
- Details: [docs/live-pages.md](docs/live-pages.md).
- CSS is a pinned Tailwind build, committed. Node is only needed to rebuild it ([assets](apps/base/ui/assets/README.md)).

## Checks

There are no unit tests. The checks are discovery, formatting, Clippy and running the app.

```sh
python3 scripts/discover-modules.py --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

## Limits

- Broadcasts are process-local. Run one app process.
- No production login throttling or distributed notification fan-out yet.
- Creates its own schema. It does not adopt an existing database.

## Lineage

Bilibi follows the ideas of [Belimbing](https://github.com/BelimbingApp/belimbing), a Laravel platform for business applications.

## License

MIT. See [LICENSE](LICENSE).
