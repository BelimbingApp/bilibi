# Running Bilibi on a server

`scripts/install-server.sh` sets up a Bilibi server: one or more instances (Dev, Staging, Production), each with its own address, database, system service and release folders under `/opt/bilibi`. This page explains what it creates and why, how to put a release into service, and how an instance knows which one it is.

## The short version

Build both programs on your own machine, from your local Bilibi checkout. Install Node.js >=22 and npm there; Cargo generates and embeds the stylesheet during the build:

```sh
cargo build --release --locked -p bilibi-web -p bilibi-installer
```

Copy the programs from `target/release/` and the whole `scripts/` directory to a fresh Ubuntu or Debian server. The server needs neither Cargo nor a Rust compiler. On that server, as root:

```sh
sudo scripts/install-server.sh create production --role production --site erp.example.com
sudo scripts/install-server.sh release production /path/to/bilibi-web
```

```sh
sudo scripts/install-server.sh create-owner production
sudo scripts/install-server.sh installer /path/to/bilibi-installer
```

The first command installs PostgreSQL 18 and Caddy, creates the users and folders, writes the service and the proxy site, and prints what to do next. The second copies the program into `releases/1`, updates the database, points `current` at it and starts the service. The third asks you for your organisation's name, your email and a password, and creates the first account: the server owner. The fourth starts the installer, the service that puts later releases into service when the owner asks on the Software page. Open `https://erp.example.com`, sign in with that email, and Caddy serves the app, with a certificate it fetched itself.

Want a Dev instance on the same machine to try changes first? Add one:

```sh
sudo scripts/install-server.sh create dev --role dev --site dev.erp.example.com
sudo scripts/install-server.sh release dev /path/to/bilibi-web
```

Notice what you did **not** do: pick ports, write database passwords, write a unit file or a proxy config, or edit the database to get a first login. The script generated them, and it tells you when something you asked for conflicts with what exists.

You may already have questions: where do the files go, how does a page know it is on Production, what happens to people using the app during a switch, and what about building? We cover each below.

## What the script creates

Each instance lives in one folder:

```
/opt/bilibi/
  production/
    env               the instance's settings for the service: role, address, database, keys (root:bilibi, 0640)
    releases/1/       bilibi-web, SHA256SUMS, and the build's manifest.txt and Cargo.lock when it has them
    releases/2/
    releases/current -> 2        the release in service; the installer moves this link
    current -> releases/current  fixed, so the service always starts current/bilibi-web
    backups/job-<n>/  the database backup the installer took before job <n> changed anything
    incoming/         a release on its way in: downloaded from the source or uploaded as a file, until the installer copies it
    data/             the folder the running program writes: media/ uploads, mailbox/ outgoing mail
    site/waiting.html the page Caddy shows while the service is stopped
  dev/                the same, for Dev
  build/              home of bilibi-build: bin/bilibi-installer
```

Two system users share the work, so the program that serves pages can never rewrite itself:

| User | Runs | Owns |
| --- | --- | --- |
| `bilibi` | the service `bilibi@<instance>` | `data/`, and `incoming/` together with `bilibi-build`; it can read `releases/` and `env` |
| `bilibi-build` | the installer `bilibi-installer@<instance>`: put releases into service | `releases/`, `backups/`, `/opt/bilibi/build`, and `incoming/` with `bilibi`; it may start, stop and restart `bilibi@*` and reload Caddy through sudo, nothing else |

Two systemd unit templates serve every instance: `bilibi@<instance>` runs `current/bilibi-web run` as `bilibi`, reads `env`, restarts after a crash and permits writes under `data/` and `incoming/`. `bilibi-installer@<instance>` runs `/opt/bilibi/build/bin/bilibi-installer` as `bilibi-build` and carries out install and undo jobs from Software. Servers have no build worker or toolchain requirement. Build releases locally or through your application repository’s GitHub Actions; see [application workspaces](application-workspaces.md).

Each instance has its own PostgreSQL role and database (`bilibi_production`, `bilibi_dev`) on the server's PostgreSQL 18 cluster, with a generated password kept only in `env`. Dev cannot read Production's data.

The offline waiting page identifies the instance folder and address; it does not cache a server role.

Caddy has one site per instance in `/etc/caddy/sites/bilibi-<instance>.caddy`, imported by `/etc/caddy/Caddyfile`. The site proxies to the instance's local port and serves the waiting page while the backend is down. For a public name Caddy fetches a certificate on its own; for a private name pass `--tls internal` and Caddy signs one itself.

## How an instance knows which one it is

The role comes from **`instance.role` in the settings table**. The explicit install seed takes its first value from `BILIBI_ENV` in the instance's `env` file: `development`, `staging` or `production` (unset means Production). After that, the saved setting wins, including after a restart or repeat install. Editing `BILIBI_ENV` does not change an installed server's role.

| Role | Install value | Purpose | 2FA step-up |
| --- | --- | --- | --- |
| Dev | `development` | Try a locally built composition | Off, except role changes and other actions that always require a fresh code |
| Staging | `staging` | Test a release before Production | On |
| Production | `production` | Run the tested release | On |

Builds run in a development workspace or in your application repository’s GitHub Actions. The role identifies the instance; it never enables a server build.

Click the role in the status bar to open **Instance settings** (`/system/instance`). A Core Administrator in the platform-operator organisation can change the role, name and colour. Every role change needs a fresh 2FA code, even on Dev, and is audited. The page warns before moving to or from Production: this changes warnings and 2FA rules without moving any data. Other accounts cannot change the role.

The name and colour also remain available in the generic operator settings. Empty name and colour follow the current role:

| Setting | Default | Meaning |
| --- | --- | --- |
| Instance name shown on every page | Dev, Staging or Production | What the banner and every confirm page say, like "Production" or "Dev (laptop)" |
| Banner colour | info (blue), warning (amber), danger (red) by role | One of the design's status colours |

In code, `bilibi_base::system::Instance::resolve` returns the role, label and colour together. **The instance banner, the Software page and the agent confirm pages all read it**, so a purchase order confirmed on Dev and the Software page on Dev cannot disagree about where they are.

## One machine or two

**Dev and Production on one machine** use separate databases, ports and addresses. Build in your local Bilibi checkout, install the release on Dev or Staging to check it, then promote the tested release to Production.

**Dev and Production on two machines** use the same release archive from GitHub or a saved upload. Each instance keeps its own local release numbers, database and backup. No instance serves release files to another.

**Install from GitHub**: on Software › Releases, name your application repository (for example your-company/my-bilibi), save a read-only token only when the repository is private, and accept the repository. That asks for your 2FA code. Then review one setup file for this server. The server downloads that file, checks it, backs up, switches and can switch back or Undo, the same way as an upload. Uploading a file still works when you do not want GitHub to supply the release.

Staging is optional. When used, install and check the exact GitHub asset there first, then review a separate Production install of that asset with fresh 2FA. See [Staging and Production](../apps/base/software/README.md#staging-and-production) for the agent report and identity checks.

## Putting a release into service

**After the first release, the owner does this from the Software page**, and the installer service does the work: copy and check, pause, back up, update the database, switch, check, and switch back by itself when the check fails. The page also offers Undo. What each step does, and what Undo loses, is in [`apps/installer/README.md`](../apps/installer/README.md).

The first release has no program to ask through, so `release` puts it in service from the terminal. It takes a `bilibi-web` program, or a folder holding it next to the build's `manifest.txt` and `Cargo.lock`:

```sh
sudo scripts/install-server.sh release production /home/me/bilibi-0.2.1/
```

Step by step, we:

1. Copy the program into the next numbered folder, `releases/<n>`, and write its SHA-256 checksum.
2. Stop the service. From here Caddy shows the waiting page.
3. Update the database with the new program's `migrate` and apply its production seeds. Nothing migrates on boot, so this must happen before the new program serves. If it fails, the previous release is started again and nothing has changed.
4. Point `releases/current` at `<n>` and start the service. The instance's `current` is a fixed link to `releases/current`, so the link that moves lives in the folder `bilibi-build` owns, and the installer can move it too; an instance created before this shape is converted the next time you run `create`, `installer`, `release` or `switch`. A job that finds an unconverted instance ends before the pause with "Nothing changed" and says to run `installer` or `create`.
5. Check that the backend answers within 60 seconds. If it does not, point `current` back at the previous release and start that.

`switch` goes to any kept release the same way, after checking its checksum:

```sh
sudo scripts/install-server.sh switch production 1
```

> [!WARNING]
> Migrations are forward-only. Switching back to an older release after a newer one changed tables leaves those changes in place; the older program may not expect them. That case is the Software page's Undo, which restores the backup the installer took first and says what would be lost, with a count. Use `switch` only for releases that share the same database shape.

`status` lists every instance, its role, address, service and installer states, and releases.

## The installer service

Build the installer on your own machine:

```sh
cargo build --release --locked -p bilibi-installer
```

Copy the finished program to the server, then run there:

```sh
sudo scripts/install-server.sh installer /path/to/bilibi-installer
```

This copies the program to `/opt/bilibi/build/bin/bilibi-installer`, owned by root so neither Bilibi user can change it, and starts `bilibi-installer@<instance>` for every instance on the machine. A `create` after that starts one for the new instance too. Run `installer` again with a newer program to update it: the services restart, and a job one of them was in the middle of is settled by the rules in [`apps/installer/README.md`](../apps/installer/README.md) ("If the server restarts mid-way"): before the database changed it ends `interrupted` and nothing changed; after, it finishes.

The installer reads the instance's `env` for its database and port. It copies a release from `incoming/`, where the program puts a verified GitHub download or uploaded archive.

The installer unit is skipped quietly while `/opt/bilibi/build/bin/bilibi-installer` does not exist, so starting `bilibi@<instance>` before you have run `installer` does not make it fail and retry.

## The first account

A fresh database has no accounts, so nobody can sign in. `create-owner` creates the first one: the server owner, a Core Administrator of the organisation that runs this server (the platform operator). Run it once, after the first release is in service:

```sh
sudo scripts/install-server.sh create-owner production
```

It asks at the terminal, and what you type for the password is not shown:

```
Operator company name (your organisation): Example Operations
Operator company code [example_operations]:
Owner email: owner@example.com
Owner name [owner]: Ng Kiat Siong
Password:
Confirm password:
Platform operator ready: tenant 1 “Example Operations”, company 1 “Example Operations”.
Owner created: owner@example.com (account 1), Core Administrator of the platform operator.
The owner signs in under the platform operator's company.
Sign in at https://erp.example.com/login as owner@example.com.
```

Notice that the company questions come first only on a database that has no operator company yet. If you ran `bilibi-web platform-provision` before, the command finds that company and asks only about the account. The password must be at least 8 characters; a shorter one is refused and asked for again.

**Run it a second time and it refuses**, because the server already has an owner:

```
Not created: this install already has an owner. The platform operator holds 1 account(s); the first is owner@example.com.
Sign in as one of them at https://erp.example.com/login. To add people, use Users > New user there.
If nobody can sign in any more, that is account recovery, which this command does not do. If an owner only lost their phone, see [Lost phone](#lost-phone).
```

The command is the same `bilibi-web create-owner` the script runs for you, in the instance's environment. From a script with no terminal, give every answer as a flag and the password on standard input:

```sh
printf '%s\n' "$PASSWORD" | sudo scripts/install-server.sh create-owner production \
    --company-name "Example Operations" --company-code example_operations \
    --email owner@example.com --name "Ng Kiat Siong" --password-stdin
```

> [!WARNING]
> There is no `--password` flag, on purpose: an argument lands in your shell history and in the process list. Type it, or pipe it in with `--password-stdin`.

Everything the command creates is written the way the app writes it, so it shows in the audit log under Administration → Audit Log → Actions as **System · create-owner**: the retained `user.bootstrap.completed` action with the account, email and role, and the data changes for the account, its Core Administrator role and the bootstrap receipt that marks the account as the server owner. The command works with any `BILIBI_ENV`; on Dev, the development seed's `ai@agent.my` already exists and the command refuses, which is the point.

Behind the scenes this is the development seed's bootstrap with your details instead of the seed's, through the same owners: Company provisions the operator company, User creates the account and the receipt, Authz grants the role ([`apps/core/user/README.md`](../apps/core/user/README.md), Bootstrap administrator).

## Lost phone

Bilibi has no 2FA recovery codes. A person who loses their phone asks another admin to reset their 2FA (Users › the person › **Reset 2FA**, which needs the admin's own code). When the person is the only admin, the server owner resets it from the server:

```sh
sudo scripts/install-server.sh two-factor-reset production --email owner@example.com
```

```
2FA is off for owner@example.com. The account was signed out and notified. Sign in at https://erp.example.com/login with the password, then set up the authenticator app again.
```

- It turns off every 2FA method of one account, signs the account out and notifies the person. Their password is untouched.
- Already off is not an error: it says so and changes nothing.
- It shows in the audit log as **System · two-factor-reset**.
- The command is `bilibi-web two-factor-reset --email <address>` in the instance's environment.

## The waiting page

While the service is stopped, Caddy answers every request with status 503 and the waiting page, marked with the header `X-Bilibi-Waiting`. The page says which instance is pausing (**PRODUCTION erp.example.com**, "Production is updating", in the role's colour), polls the same address every second or two, and reloads itself once an answer arrives without that header. People land back on what they asked for. A pause that runs long shows how the server owner can check the service.

The page is rendered once per instance from `scripts/waiting-page.html` into `site/waiting.html`, so the development launcher and the server share one page.

## Build locally

[ADR 02](architecture/decisions/02-build-in-development-keep-everything-in-step.md) records why builds stay in development, servers only install tested releases, and official software is kept in step.

**Prepare application releases from the outer application repository.** Create it from your trial with `./bilibi new <name>`. Commit and push component changes in their own repositories, run `./bilibi pin`, then review and commit the saved recipe and composition together. Run `./bilibi check` with a separate empty disposable database, then `./bilibi build`. The pinned builder restores the saved lock and verifies the exact source selection; the application's README describes publication. See [application workspaces](application-workspaces.md) for the directory layout and migration.

### Focused component builds

Direct Platform and component clones remain supported for focused contributions. A local Bilibi checkout and an optional coding agent are the operator’s build tools. Find a Domain or Extension in Software or through MCP, read which release accepts this server and which commits CI recorded as tested together, then follow its `local_steps` in that checkout. For example:

```sh
./bilibi add finance --kind domains --source https://github.com/BelimbingApp/bilibi-dom-finance --commit <recommended-commit>
python3 scripts/discover-modules.py
./scripts/build-assets.sh --check
cargo metadata --format-version 1 > /dev/null
python3 scripts/discover-modules.py --pin
cargo build --release --locked -p bilibi-web
./target/release/bilibi-web status
```

Keep other mounted Domains and Extensions. For an existing clone at another commit, `add` changes nothing and prints the fetch and checkout steps to run once your work is saved. Run the composition locally against its own database and check its pages in the browser. Build for the server’s operating system, processor and system library version.

Copy `target/release/bilibi-web`, `.local/composition/manifest.txt` and `.local/composition/Cargo.lock` into a release folder, then write its checksum and archive it:

```sh
mkdir -p /path/to/release
cp target/release/bilibi-web .local/composition/manifest.txt .local/composition/Cargo.lock /path/to/release/
(cd /path/to/release && sha256sum bilibi-web manifest.txt Cargo.lock > SHA256SUMS)
tar -C /path/to/release -czf /path/to/release.tar.gz bilibi-web SHA256SUMS manifest.txt Cargo.lock
```

Upload that archive on **Software › Releases** and review the install. To build and publish complete assets on GitHub, create your application with `./bilibi new <name>` and use its release workflow; see [application workspaces](application-workspaces.md). Install the same published asset on Staging and Production with separate reviews and fresh 2FA. Backup, automatic switch-back, audit and Undo use the same path for GitHub and upload.

A folder prepared this way has no `release.json`. An uploaded file's review therefore warns that its change list is known only once the installer reads the program. The installer records the pinned composition and changes in the job's result.

When upgrading an older installation, `create`, `installer` and `release` stop and disable each former `bilibi-worker@<instance>` service, then remove that obsolete unit and its sudoers rule. Rerunning is safe, and a fresh server is left alone. The new `base-software-003-local-builds` migration cancels queued and running build jobs with local build guidance, preserving their history. Before upgrading to the GitHub-and-upload release, finish or cancel active instance-source installs; the migration refuses while one remains, including the install that delivers the upgrade. Deliver the upgrade itself as a GitHub asset or an uploaded archive, and update the installer alongside the app. Completed history and Undo are preserved.

## Reference

Script commands:

| Command | What it does |
| --- | --- |
| `create <instance> --role dev\|staging\|production --site <host> [--port <n>] [--tls internal]` | Packages, users, folders, database, `env`, service, Caddy site. Rerun to change the site; the saved role is preserved and the port stays fixed. Change the role in Instance settings with 2FA |
| `release <instance> <bilibi-web or folder>` | Copy into `releases/<n>`, migrate, switch, check |
| `create-owner <instance> [--email <address>] [--name <name>] [--password-stdin] [--company-name <name> --company-code <code>]` | The first account, the server owner; asks for anything not given, refuses once an owner exists (it then only finishes an unfinished company link) |
| `two-factor-reset <instance> --email <address>` | Turn off one account's authenticator after a lost phone, sign the account out and notify it ([Lost phone](#lost-phone)) |
| `installer <bilibi-installer>` | Copy the installer program to `/opt/bilibi/build/bin` and start `bilibi-installer@<instance>` for every instance; rerun to update it |
| `switch <instance> <n>` | Put a kept release in service |
| `status [<instance>]` | Roles, addresses, service and installer states, releases |

Keys in `env` (the service's environment; `README.md` explains each):

| Key | Set to |
| --- | --- |
| `BILIBI_ENV` | Initial role for the install seed: `development`, `staging` or `production`; later changes use Instance settings |
| `APP_ORIGIN` | `https://<site>`, the exact address people browse |
| `BIND_ADDR` | `127.0.0.1:<port>`, where Caddy forwards to; the first free port from 4300 unless you pass `--port` |
| `DATABASE_URL` | the instance's own database and role on the local cluster |
| `BILIBI_SETTINGS_KEY`, `BILIBI_USER_SIGNING_KEY`, `BILIBI_MEDIA_SIGNING_KEY` | generated 64-hex-character keys; keep them, losing `BILIBI_SETTINGS_KEY` loses every secret setting and authenticator key |
| `BILIBI_MAIL_DIR` | `data/mailbox`, until an SMTP relay is set |
| `BILIBI_INSTANCE_DIR` | The instance folder: the program lists and serves releases, and receives bundles under `incoming/` for the installer |

Files outside `/opt/bilibi`: `/etc/systemd/system/bilibi@.service`, `/etc/systemd/system/bilibi-installer@.service`, `/etc/sudoers.d/bilibi-build`, `/etc/caddy/sites/bilibi-<instance>.caddy`, and one `import` line in `/etc/caddy/Caddyfile`.

## Where to go next

- [Repeat the fresh-server proof](server-proof/README.md): disposable systemd lab, local Finance build, file upload, 2FA, Undo and a failed health check.
- The design behind this layout: the Software page plan, in the Firstmate scout report `bilibi-domain-chooser` (sections 2, 3 and 6).
- What each environment variable does: [`README.md`](../README.md), Configuration.
- 2FA step-up and why Dev skips it: [`apps/base/step_up/README.md`](../apps/base/step_up/README.md).
- The instance identity in code: [`apps/base/system/README.md`](../apps/base/system/README.md).

## HTTPS for OAuth

Use an HTTPS `APP_ORIGIN` on Staging and Production. This is a breaking change: Bilibi now refuses to start there with an HTTP origin, and `/oauth/token` refuses to issue tokens on one. Move such an install behind HTTPS and update `APP_ORIGIN` before upgrading.

Startup also reads the instance role from settings. If the database is not migrated and seeded, it reports that instead of the HTTPS message.

The address must be the exact public origin people browse, without a path, query or credentials. TLS may terminate at your reverse proxy while Bilibi listens on a private HTTP socket. A Dev installation may use HTTP only on `localhost` or a loopback IP. Token issuance rechecks the stored instance role, so changing a running Dev copy to Staging or Production also requires HTTPS.

AI apps register themselves without an operator-maintained client list. Their return addresses must use HTTPS, except HTTP callbacks to the person's own loopback address. Each app supplies its own callback paths; loopback ports may vary. See [Agent Access](../apps/core/agent_access/README.md#client-registration) for matching and retention rules.

Upgrading from a static client list:

- Migration `004-client-registration` deletes the retired `agent_access.oauth_clients` setting, including any client an operator added.
- Apps configured with `bilibi-claude-code` or `bilibi-codex` keep working on their current grants and refreshes. On their next authorization they must register again; point them at the AI connections prompt.
