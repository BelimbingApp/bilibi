# Bilibi

I'm Kiat, a solo builder. I started Belimbing in 2025. It is named after Bukit Belimbing, a small industrial area in Malaysia where about a hundred small and medium companies operated. The people there are the experts in their own work. They know what they want. The software built for them often does not work the way they want. It is expensive, inflexible, and the licenses are a burden. That is the digitization bottleneck. With AI, this must change. Belimbing is an application platform. It started in Bukit Belimbing, but it is for businesses everywhere.

Bilibi is the third iteration of Belimbing, built in Rust. Belimbing was the first, in Laravel. Bilimbi was the second, in Elixir and Phoenix. Rust was chosen for low memory use and for a better AI agent contract. Talking to small business owners, I realized it is impractical to ask them to provision servers and maintain them. So Bilibi is multi-tenant from the ground up, and an IT service provider can run it as SaaS for their clients.

Bilibi is MIT-licensed, like the earlier iterations. The architecture is the same four roots: Base, Core, Domains, and Extensions. Base and Core supply the shared foundation: companies, users, permissions, and audit. Domains are business areas, this is where the domain experts work with AI to build what they want. Extensions are company-specific changes, such as a custom report or a new feature. You own what you build, you can share it, sell it, or keep it to yourself.

**Status:** work in progress.

- Security built in from the start: per-tenant isolation in every query, fine-grained permissions, append-only audit, two-factor step-up. Not certified for any compliance standard yet.
- Agent-operable: people and AI agents act through the same checks.
- Business areas (Finance, Factory, and so on) are separate repositories you mount in. Company-specific changes live in your own extension, never in the core.
- Multi-tenant: one installation serves many companies, each isolated from the others.

## Who it is for

- Small and medium businesses now; larger organisations as enterprise sign-in and compliance arrive.
- Developers and builders serving businesses.
- IT service providers.
- SaaS founders and builders.

## Why Bilibi

- Your people know the work. They describe it, an AI agent builds it, and they check that it does the job.
- You own it. When the job changes, you change the system.
- You need a small team, not a large one. Sign-in, permissions, and audit are already there.
- The goal: a user asks an AI agent for what they want done, and the agent does the work through Bilibi's MCP.

## Do not choose Bilibi if

- You want a finished package, and someone else to blame when it does not fit.
- Your people cannot describe how the work goes, or check whether what was built is correct.
- You need it in production next month, with backups, monitoring and support already in place.

## What works today

- **Sign-in:** server-held sessions, Argon2id passwords, password reset.
- **Permissions:** roles, capabilities and direct grants; a deny always wins; sensitive fields can be masked.
- **Companies, people and addresses:** lists, details and inline edits, scoped to the tenant.
- **Audit:** every change records old and new values and who made it, in the same save; append-only.
- **Notifications and mail:** a live notification bell; mail goes to `.local/mailbox/` until an SMTP relay is set.
- **Agent access:** agents sign in for a person through OAuth and work through `/api/v1` and `/mcp`; a Plugin for ChatGPT can be downloaded from settings.
- **Database tables:** an owner-only, read-only view of every table, with data, schema and relationships.

## Run it locally

On Ubuntu, Debian or WSL2:

```sh
./scripts/setup.sh
./scripts/start-app.sh
```

`setup.sh` installs everything once (Rust, PostgreSQL 18, Caddy, mkcert, Node.js) and is safe to rerun.

Open the address printed by the launcher and sign in as `ai@agent.my` / `bilibi-dev` (development seed only). One checkout owns <https://bilibi.localhost>; other checkouts use their own `http://127.0.0.1:<port>` address. Setting `BILIBI_DEV_PORT` selects a backend port, without taking over another checkout's HTTPS address.

The launcher starts a private database, builds, migrates, seeds and rebuilds on change. While the HTTPS app is down, its waiting page shows the current build step and elapsed time. A failed build shows the latest diagnostics; fix the error and save a file to rebuild. Cargo output stays in the terminal, and a previous successful build keeps serving until its replacement is ready. Restart it after changing an environment variable.

## Configuration

| Variable | Use |
|----------|-----|
| `DATABASE_URL` | PostgreSQL 18. Unset: the private instance on port 5438 |
| `APP_ORIGIN` | The exact browser origin, no trailing slash |
| `BIND_ADDR` | Listen address, default `127.0.0.1:3000` |
| `MAIL_HOST` | SMTP relay. Unset: write mail to `.local/mailbox/` |
| `BILIBI_USER_SIGNING_KEY` | 64 hex characters, for email verification links |
| `BILIBI_SETTINGS_KEY` | 64 hex characters. Encrypts secret settings and authenticator keys. Two-factor setup refuses without it |
| `BILIBI_SMS_DIR` | Where the SMS simulator writes messages (development and staging only). Default `.local/sms-outbox/`; read them at `/dev/sms-inbox` |
| `BILIBI_ENV` | `development`, `staging` or `production`. Unset means `production`. Any other value refuses to boot |

Every page, consent page and agent answer says which Bilibi it is, so nobody mistakes a test copy for the live system. Every instance but production shows a badge in the top bar (a debug build leaves it to the status bar's `dev` marker) and on the sign-in page; reviews and agent plans name every instance, production included. The name and colour are the Instance settings (`instance.label`, `instance.colour`), read through `bilibi_base::system::Instance::resolve` ([Base System](apps/base/system/README.md)).

`BILIBI_ENV` decides one more thing: whether Base Step-up is enforced.

| `BILIBI_ENV` | Step-up operations (declared by each module, set per tenant) |
|--------------|--------------------------------------------------------------|
| `development` | Never ask for a code |
| `staging`, `production`, unset | Ask as each tenant's policy says: a check within a sliding 30-minute window, capped at 6 hours (tenant settings), or every time |

- Set `BILIBI_ENV=development` only on a developer machine. A missing value never weakens a guard.
- Each account sets up its authenticator app or a phone for text-message codes at `/settings/two-factor`. Tenants choose what asks for it at `/authz/step-up`, sign-in included. See [Base Step-up](apps/base/step_up/README.md) and [Base SMS](apps/base/sms/README.md).
- Text messages: development and staging use the simulator (`/dev/sms-inbox`); production has no SMS driver yet, so SMS is refused there.

Terminate TLS at a reverse proxy, and keep the database on a private network: Bilibi connects to PostgreSQL without TLS.

## Run it on a server

`scripts/install-server.sh` sets up Dev, Staging and Production instances under `/opt/bilibi`, each with its own database, service and address, and puts releases into service. `bilibi-web create-owner` then creates the first account, the server owner, on the empty database. `BILIBI_ENV` is also the instance's role: only `development` builds software. See [`docs/server-install.md`](docs/server-install.md).

## Add a business area

A Domain such as Finance is its own repository, mounted as a real folder (not a submodule or symlink):

```sh
git clone https://github.com/BelimbingApp/bilibi-dom-finance.git apps/domains/finance
python3 scripts/discover-modules.py
```

Rules for mounting and unmounting: [`apps/domains/AGENTS.md`](apps/domains/AGENTS.md). Extensions mount the same way under `apps/extensions/`.

## Checks

There are no unit tests. Every change must pass these and then be tried in the running app:

```sh
python3 scripts/discover-modules.py --check
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

## Limits

- Bilibi creates its own schema; it does not adopt an existing database.

## Read next for AI agents

- [`AGENTS.md`](AGENTS.md): how agents build and change this repository.
- [`DESIGN.md`](DESIGN.md): the UI design rules.
- [`docs/architecture/`](docs/architecture/README.md): layers, modules, data, audit, security, and the decision records.
- [`docs/versions.md`](docs/versions.md): versions and release tags, the software catalogue, and how the running program shows what it is made of.

## License

MIT. See [LICENSE](LICENSE).
