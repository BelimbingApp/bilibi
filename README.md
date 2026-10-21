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
- **Agent access:** agents sign in for a person through OAuth and work through `/api/v1` and `/mcp`. Connection details are on My settings, under AI connections. The tab holds one prompt the person pastes into their AI app, so the agent connects itself.
- **Database tables:** an owner-only, read-only view of every table, with data, schema and relationships.

## Run it locally

1. Clone Bilibi and enter its folder:

```sh
git clone https://github.com/BelimbingApp/bilibi
cd bilibi
```

2. Start it:

```sh
./bilibi start
```

`start` checks the tools, runs the existing setup when needed, activates newly installed Rust tools in the same shell, and launches local sources. Setup installs Rust, PostgreSQL 18, Caddy, mkcert and Node.js. You do not need a GitHub account. An AI agent follows these same steps.

Open the address printed by the launcher and sign in as `ai@agent.my` / `bilibi-dev` (development seed only). One checkout owns <https://bilibi.localhost>; other checkouts use their own `http://127.0.0.1:<port>` address. Setting `BILIBI_DEV_PORT` selects a backend port, without taking over another checkout's HTTPS address.

### Supported environment

Use apt-based Ubuntu or Debian, including either distribution in WSL2, with Git, Bash and network access. Installing missing packages needs root or sudo privileges. Setup may ask for your sudo password in the terminal; one command does not mean unattended installation.

Python 3.11 or newer is required. Ubuntu 20.04 and 22.04, and Debian 11, ship older Python versions. Upgrade to Ubuntu 24.04 or newer, or Debian 12 or newer, before starting; setup does not replace the distribution's Python. If local HTTPS trust needs manual setup (including Windows trust in WSL2), use the printed loopback HTTP address. The launcher falls back to HTTP when local HTTPS is unavailable.

### Commands in a Platform clone

Run commands from the clone's root. Local branches, staged files and uncommitted edits are kept.

| Command | What it does |
| --- | --- |
| `./bilibi start` | Prepare missing tools, install the commit safeguard, show local changes and start the development app |
| `./bilibi setup` | Run the repeatable setup and install the commit safeguard without starting the app |
| `./bilibi add <component>` | Install or verify the safeguard, then mount an official Domain or Extension from `catalogue.json` |
| `./bilibi status` | Show each component's path, branch, commit and local changes |
| `./bilibi new <name>` | Planned next step for keeping your composition in your own application repository; currently prints this guidance |
| `./bilibi check`, `pin`, `build` | Require that future application repository; currently refuse and point to `new` |

The application workspace is being delivered in stages; see [ADR 03](docs/architecture/decisions/03-application-workspaces-one-front-door.md). `scripts/setup.sh` and `scripts/start-app.sh` remain available directly. Direct setup also installs the safeguard; after installing tools directly, load `$HOME/.cargo/env` before using the direct launcher.

### Commit safeguard

**Commit component changes in their own repositories.** The Platform pre-commit hook runs `python3 scripts/discover-modules.py --check`, including its staged-file checks. It rejects mounted Domain or Extension linkage and foreign path packages in `Cargo.lock`. CI remains authoritative because Git hooks can be bypassed.

Installation is local to this repository and safe to repeat. An existing executable `pre-commit` is retained as `pre-commit.bilibi-original` and runs after the discovery check; other hooks and `core.hooksPath` stay unchanged. Relative and absolute hook paths inside the repository are supported. Installation refuses external shared directories, symlinks, tracked hooks and a conflicting backup rather than overwrite them.

For an unsafe hook location, create an untracked directory inside this checkout (for example `.local/hooks`), copy your existing hooks into it, then deliberately set `git config --local core.hooksPath .local/hooks` and run `python3 scripts/install-platform-hook.py`. Keep any existing backup for inspection; do not overwrite it. If your project tracks hooks, keep its originals and use this local copy for chaining. Do not point the local setting at a shared directory.

A rejected commit names its repository and offending file. Unstage the mounted composition files, commit Domain or Extension edits in their own repositories, and follow the [unmount instructions](apps/domains/AGENTS.md) before preparing Platform composition changes. Do not hide generated changes with index flags.

### Local launcher

The launcher sets `BILIBI_ENV=development` unless you already set it, so the install seed identifies a fresh local copy as Dev. A saved role in Instance settings takes precedence. It starts a private database, builds, migrates, seeds and rebuilds on change. While the HTTPS app is down, its waiting page shows the current build step and elapsed time. A failed build shows the latest diagnostics; fix the error and save a file to rebuild. Cargo output stays in the terminal, and a previous successful build keeps serving until its replacement is ready. Restart it after changing an environment variable.

`start-app.sh` stores three keys for this checkout when they are not already set. `BILIBI_SETTINGS_KEY` is `.local/dev/settings.key`. `BILIBI_USER_SIGNING_KEY` is `.local/dev/user-signing.key`. `BILIBI_MEDIA_SIGNING_KEY` is `.local/dev/media-signing.key`. The first run creates a missing file as 64 hex characters (32 random bytes) with mode 0600. Later runs read that same file. The launcher leaves an existing key file as it is. Replacing `settings.key` would make every secret setting and authenticator key unreadable. `.local/` is not committed. Restart the launcher after you change one of these files.

## Configuration

| Variable | Use |
|----------|-----|
| `DATABASE_URL` | PostgreSQL 18. Unset: the private instance on port 5438 |
| `APP_ORIGIN` | The exact browser origin, no trailing slash. Staging and Production refuse to start unless it is HTTPS; see [HTTPS for OAuth](docs/server-install.md#https-for-oauth) |
| `BIND_ADDR` | Listen address, default `127.0.0.1:3000` |
| `MAIL_HOST` | SMTP relay. Unset: write mail to `.local/mailbox/` |
| `BILIBI_USER_SIGNING_KEY` | 64 hex characters, for email verification links. Unset: `start-app.sh` supplies one |
| `BILIBI_SETTINGS_KEY` | 64 hex characters. Encrypts secret settings and authenticator keys. 2FA setup refuses without it. Unset: `start-app.sh` supplies one |
| `BILIBI_MEDIA_SIGNING_KEY` | 64 hex characters, for signed file links. Unset: `start-app.sh` supplies one |
| `BILIBI_ENV` | `development`, `staging` or `production`. Initial role for the install seed. Unset means `production`; `start-app.sh` defaults to `development`. Later role changes use Instance settings. Other values refuse installation |

Every page, consent page and agent answer says which Bilibi it is, so nobody mistakes a test copy for the live system. The status bar always shows the stored server role; Staging also shows a top-bar badge. Dev and Staging show a sign-in badge; reviews and agent plans name every instance, production included. The name and colour are the Instance settings (`instance.label`, `instance.colour`), read through `bilibi_base::system::Instance::resolve` ([Base System](apps/base/system/README.md)).

The stored server role also decides whether ordinary Base Step-up checks apply. Click the status-bar role to open Instance settings. A Core Administrator can change it with a fresh 2FA code; changing to or from Production carries a warning and every change is audited.

| Server role | Step-up operations (declared by each module, set per tenant) |
|--------------|--------------------------------------------------------------|
| Dev | Skip ordinary checks; role and software changes still need a fresh code |
| Staging, Production | Ask as each tenant's policy says: a check within a sliding 30-minute window, capped at 6 hours (tenant settings), or every time |

- Choose Dev only for a development copy. A missing install value defaults to Production.
- Each account sets up its authenticator at `/settings/two-factor`. Tenants choose what asks for it at `/authz/step-up`. See [Base Step-up](apps/base/step_up/README.md).

Terminate TLS at a reverse proxy, and keep the database on a private network: Bilibi connects to PostgreSQL without TLS.

## Run it on a server

`scripts/install-server.sh` sets up Dev, Staging and Production instances under `/opt/bilibi`, each with its own database, service and address, and puts releases into service. `bilibi-web create-owner` then creates the first account, the server owner, on the empty database. The install seed saves the initial `BILIBI_ENV` role in settings; later changes use the protected Instance settings page. Build software on your own machine, then install the tested release on any of them. See [`docs/server-install.md`](docs/server-install.md).

## Add a business area

A Domain such as Finance is its own repository, mounted as a real folder (not a submodule or symlink):

```sh
./bilibi add finance
./bilibi start
```

`add` uses the official repository's default branch. Repeating it keeps an existing checkout, branch and edits, and refreshes discovery and the working dependency lock. A conflicting destination or different origin is refused for you to inspect. Generated composition and lock changes stay local to Platform. Software's developer commands include the recommended release commit when preparing an update for a server.

Rules for manual mounting, private repositories and unmounting: [`apps/domains/AGENTS.md`](apps/domains/AGENTS.md) and [`apps/extensions/AGENTS.md`](apps/extensions/AGENTS.md). Extensions use the same `add` command and mount under `apps/extensions/`.

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
