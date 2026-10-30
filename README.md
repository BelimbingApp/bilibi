# Bilibi

I'm Kiat, a solo builder. I started Belimbing in 2025. It is named after Bukit Belimbing, a small industrial area in Malaysia where about a hundred small and medium companies operated. The people there are the experts in their own work. They know what they want. The software built for them often does not work the way they want. It is expensive, inflexible, and the licenses are a burden. That is the digitization bottleneck. With AI, this must change. Belimbing is an application platform. It started in Bukit Belimbing, but it is for businesses everywhere.

Bilibi is the third iteration of Belimbing, built in Rust. Belimbing was the first, in Laravel. Bilimbi was the second, in Elixir and Phoenix. Rust was chosen for low memory use and for a better AI agent contract. Talking to small business owners, I realized it is impractical to ask them to provision servers and maintain them. So Bilibi is multi-tenant from the ground up, and an IT service provider can run it as SaaS for their clients.

Bilibi is MIT-licensed, like the earlier iterations. The architecture is the same four roots: Base, Core, Domains, and Extensions. Base and Core supply the shared foundation: companies, users, permissions, and audit. Domains are business areas, this is where the domain experts work with AI to build what they want. Extensions are company-specific changes, such as a custom report or a new feature. You own what you build, you can share it, sell it, or keep it to yourself. Bilibi is also the framework for building them: Domains and Extensions plug into its foundation and follow its rules.

**Status:** version 0.1, in active development.

- Security built in from the start: per-tenant isolation in every query, fine-grained permissions, append-only audit, two-factor step-up. Not certified for any compliance standard yet.
- Agent-operable: people and AI agents act through the same checks.
- Business areas (Finance, Factory, and so on) are separate repositories you mount in. Company-specific changes live in your own extension, never in the core.
- Multi-tenant: one installation serves many companies, each isolated from the others.

## Who it is for

- Small and medium businesses, and larger organisations connecting their existing identity provider.
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

- **Sign-in:** server-held sessions, Argon2id passwords, password reset, limits on wrong passwords (per account and per sender, operator settings), and optional [company OIDC single sign-on](apps/core/user/sso.md) with provider 2FA and role mapping, plus optional [SCIM provisioning](apps/core/user/scim.md) with immediate deactivation.
- **Permissions:** roles, capabilities and direct grants; a deny always wins; sensitive fields can be masked.
- **Companies, people and addresses:** lists, details and inline edits, scoped to the tenant.
- **Audit:** every change records old and new values and who made it, in the same save; append-only.
- **Notifications and mail:** a live notification bell; mail goes to `.local/mailbox/` until an SMTP relay is set.
- **Agent access:** agents sign in for a person through OAuth and work through `/api/v1` and `/mcp`. Connection details are on My settings, under AI connections. The tab holds one prompt the person pastes into their AI app, so the agent connects itself.
- **Database tables:** an owner-only, read-only view of every table, with data, schema and relationships.

## Where to start

Start in one of two places, depending on what you want to do:

| You want to… | Start in | What you get |
| --- | --- | --- |
| Build your own product on Bilibi | [`bilibi-app-maker`](https://github.com/BelimbingApp/bilibi-app-maker) | Your own application: Bilibi at a tested version, ready for you to add Domains, release, and install on your servers |
| Change Bilibi itself, or build a Domain or Extension for everyone | `bilibi` (this one) | Bilibi's latest source code, to change and send back as pull requests |

Both start the same way: clone the repository, then run `./bilibi start`. Bilibi opens in your browser at the sign-in page.

For example, a company that wants Bilibi for its own business clones the app maker, runs `./bilibi start`, and adds the Finance Domain. When it works for them, they push it to their own private repository and release it to their server. Someone fixing a Bilibi bug clones this repository instead, and sends the fix as a pull request.

If you started here and want to keep what you built as your own product, `./bilibi new my-bilibi` turns it into an application, in a new folder next to this one.

### The three kinds of repository

| Repository | What it holds |
| --- | --- |
| `bilibi` (this one) | Bilibi's source code: the Platform |
| [`bilibi-app-maker`](https://github.com/BelimbingApp/bilibi-app-maker) | The app maker: where every application starts, with its starting files and the `./bilibi` commands |
| Your application, such as `your-company/my-bilibi` | One product: the Bilibi, Domain and Extension versions it uses, and its releases |

You clone the app maker, and your clone becomes your application. It keeps the maker's working parts as they are, such as the `./bilibi` commands and the release scripts, and you replace its sample parts with your own.

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

Bilibi opens in your browser at the sign-in page when it is ready. Sign in as `ai@agent.my` / `bilibi-dev` (development seed only). If no browser opens, open the address the launcher prints. To stop it opening a browser, set `BILIBI_OPEN_BROWSER=0`. Every checkout defaults to <https://bilibi.localhost> and gets its own private database. To choose another address, put `BILIBI_DEV_HOST=<name>.localhost` in `.env` (see [Local launcher](#local-launcher)). If another checkout owns the address, startup stops and names that checkout. Follow the printed `BILIBI_DEV_HOST` line in your `.env`, then start again. Restart the launcher after changing the address; it releases its old name when it registers the new one. Setting `BILIBI_DEV_PORT` selects a backend port, without taking over another checkout's HTTPS address.

### Supported environment

Use apt-based Ubuntu or Debian, including either distribution in WSL2, with Git, Bash and network access. Installing missing packages needs root or sudo privileges. Setup may ask for your sudo password in the terminal; one command does not mean unattended installation.

Python 3.11 or newer is required. Ubuntu 20.04 and 22.04, and Debian 11, ship older Python versions. Upgrade to Ubuntu 24.04 or newer, or Debian 12 or newer, before starting; setup does not replace the distribution's Python. If local HTTPS trust needs manual setup (including Windows trust in WSL2), use the printed loopback HTTP address. The launcher falls back to HTTP when local HTTPS is unavailable.

### Commands in a Platform clone

Run commands from the clone's root. `./bilibi help` shows the same list. Your branches, staged files and uncommitted edits are always kept.

| Command | What it does | When to use it |
| --- | --- | --- |
| `./bilibi start --no-open` | Starts without opening a browser tab | When you do not want a new tab |
| `./bilibi start` | Installs anything missing, runs Bilibi, and opens it in your browser | Every time you want to run Bilibi |
| `./bilibi add <name>` | Adds a Domain or Extension, such as `finance`. You can also paste the `add` command that the Software page shows. | When you want to try or develop a Domain or Extension. Run `start` again afterwards. |
| `./bilibi update` | Gets the latest Bilibi changes, and moves the Domains and Extensions you added to their newest recommended versions, plus those from repositories you trust | Before you start work, to stay current |
| `./bilibi status` | Lists Bilibi and what you added, with each one's branch and uncommitted changes | When you want to see what you have changed |
| `./bilibi setup` | Installs the tools only, without starting Bilibi | When you want the tools ready in advance |
| `./bilibi new <name>` | Turns what you have here into your own application, in a new folder next to this one | When you want to keep what you built as your own product |
| `./bilibi help` | Shows the command list | Any time |

`check`, `build` and `release` work only in an application; here they say how to make one.

`new` copies the [app maker](https://github.com/BelimbingApp/bilibi-app-maker). The [application workspace guide](docs/application-workspaces.md) covers the next step, Git ownership and migration; [ADR 03](docs/architecture/decisions/03-application-workspaces-one-front-door.md) records the decision. `./bilibi start` installs the tools and safeguards the app needs.

Run `./bilibi update` before `./bilibi start` to bring an existing clone up to date. It fast-forwards your branch from its upstream. A published branch whose changes already reached the default branch, including an exact squash merge, can safely return to that default branch. Other local commits and source edits stop the update with a concrete next step. Recognised generated composition and its reproduced dependency lock are regenerated; real edits are never discarded or silently stashed. Update never downgrades a component. Clean official component branches fast-forward from their upstreams under the same safeguards. A component from a repository outside the official list (your own or your company's) is asked about once: `<owner>/<repo> isn't in Bilibi's official list. Trust it for updates? (y/n)`. Yes records it in `.local/update-trust.txt` (delete its line to stop trusting it) and its branch then moves under the same safeguards. Without a terminal, update never asks: it skips the component and prints how to trust it. A dirty, unpublished, diverged, symlinked or untrusted component stays where it is, with its path and reason printed; this does not stop the other components.

### Commit safeguard

**Commit component changes in their own repositories.** `start`, `setup` and `add` install a pre-commit hook in this repository. The hook runs `python3 scripts/discover-modules.py --check --staged`. It rejects staged mounted Domain or Extension linkage and foreign path packages in `Cargo.lock`, and ignores HEAD, so a commit that removes a mounted composition already at HEAD is allowed. CI runs the full `--check`, which also judges the committed tip, and remains authoritative because Git hooks can be bypassed.

Installation is local to this repository and safe to repeat. An existing executable `pre-commit` is retained as `pre-commit.bilibi-original` and runs after the discovery check; other hooks and `core.hooksPath` stay unchanged. Relative and absolute hook paths inside the repository are supported. Installation refuses external shared directories, symlinks, tracked hooks and a conflicting backup rather than overwrite them. `start` and `setup` print that refusal and carry on; `add` stops until you resolve it.

For an unsafe hook location, create an untracked directory inside this checkout (for example `.local/hooks`), copy your existing hooks into it, then deliberately set `git config --local core.hooksPath .local/hooks` and run `python3 scripts/install-platform-hook.py`. Keep any existing backup for inspection; do not overwrite it. If your project tracks hooks, keep its originals and use this local copy for chaining. Do not point the local setting at a shared directory.

A rejected commit names its repository and offending file. Unstage the mounted composition files (or stage their unmounted form), commit Domain or Extension edits in their own repositories, and follow the [unmount instructions](apps/domains/AGENTS.md) before preparing Platform composition changes. Do not hide generated changes with index flags.

### Keep your own application

Run `./bilibi new my-bilibi` from the Platform root, then `cd ../my-bilibi && ./bilibi start`. It creates your own application repository from the current Platform, Domains and Extensions. Commit and push source changes first; the command refuses unpublished commits and occupied destinations. Generated composition changes may stay local.

The application saves source URLs, exact commits and the resolved dependency lock. Its sources are independent clones under `sources/platform/`; the trial's database, media, configuration and keys stay untouched. You can also keep a Platform-only application. See [application workspaces](docs/application-workspaces.md) for Git ownership, releases, existing-checkout migration and rollback.

### Local launcher

The launcher reads its settings from your shell first, then from an optional `.env` in the folder of the `./bilibi` you ran (the application root for an app-maker application), then uses defaults. `.env` is never committed; copy `.env.example` to `.env` and uncomment a line to change it. A line the launcher does not know is ignored with a message. Restart the launcher after editing it. The lines it accepts:

| Line in `.env` | What it changes | Default |
|----------------|-----------------|---------|
| `BILIBI_DEV_HOST` | This instance's address, a name under `.localhost`, served over HTTPS | `bilibi.localhost` for every checkout |
| `BILIBI_DEV_PORT` | The port Bilibi itself listens on behind Caddy | The first free port from 4300 |
| `BILIBI_OPEN_BROWSER` | Open the sign-in page in your browser when the app is ready, `1` or `0` | `1` |

Servers never read `.env`: `install-server.sh` writes each server instance its own `env` file ([server installation](docs/server-install.md)).

The launcher sets `BILIBI_ENV=development` unless you already set it, so the install seed identifies a fresh local copy as Dev. A saved role in Instance settings takes precedence. It starts a private database, builds, migrates, seeds and rebuilds on change. While the HTTPS app is down, its waiting page shows the current build step and elapsed time. A failed build shows the latest diagnostics; fix the error and save a file to rebuild. Cargo output stays in the terminal, and a previous successful build keeps serving until its replacement is ready. Restart it after changing an environment variable.

`./bilibi start` stores three keys for this checkout when they are not already set. `BILIBI_SETTINGS_KEY` is `.local/dev/settings.key`. `BILIBI_USER_SIGNING_KEY` is `.local/dev/user-signing.key`. `BILIBI_MEDIA_SIGNING_KEY` is `.local/dev/media-signing.key`. The first run creates a missing file as 64 hex characters (32 random bytes) with mode 0600. Later runs read that same file. The launcher leaves an existing key file as it is. Replacing `settings.key` would make every secret setting and authenticator key unreadable. `.local/` is not committed. Restart the launcher after you change one of these files.

## Configuration

| Variable | Use |
|----------|-----|
| `DATABASE_URL` | PostgreSQL 18. Unset: this checkout's private instance under `.local/postgres18`, on a port chosen once (5438 when free) and remembered in `.local/postgres18/port` |
| `APP_ORIGIN` | The exact browser origin, no trailing slash. Staging and Production refuse to start unless it is HTTPS; see [HTTPS for OAuth](docs/server-install.md#https-for-oauth) |
| `BIND_ADDR` | Listen address, default `127.0.0.1:3000` |
| `MAIL_HOST` | SMTP relay. Unset: write mail to `.local/mailbox/` |
| `BILIBI_USER_SIGNING_KEY` | 64 hex characters, for email verification links. Unset: `./bilibi start` supplies one |
| `BILIBI_SETTINGS_KEY` | 64 hex characters. Encrypts secret settings and authenticator keys. 2FA setup refuses without it. Unset: `./bilibi start` supplies one |
| `BILIBI_MEDIA_SIGNING_KEY` | 64 hex characters, for signed file links. Unset: `./bilibi start` supplies one |
| `BILIBI_SSO_ALLOW_PRIVATE_ENDPOINTS` | `1` lets company sign-in reach identity providers on private networks and literal-loopback HTTP. Unset: public HTTPS only. See [company sign-in](apps/core/user/sso.md) |
| `BILIBI_ENV` | `development`, `staging` or `production`. Initial role for the install seed. Unset means `production`; `./bilibi start` defaults to `development`. Later role changes use Instance settings. Other values refuse installation |

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

`add` uses the official repository's default branch. Repeating it keeps an existing checkout, branch and edits, and refreshes discovery and the working dependency lock. A conflicting destination or different origin is refused for you to inspect. Generated composition and lock changes stay local to Platform. Software's developer commands add `--kind`, `--source` and `--commit` so the clone is the exact repository and release commit it shows; an existing folder at another commit is left unchanged with the steps to move it.

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
