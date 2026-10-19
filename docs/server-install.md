# Running Bilibi on a server

`scripts/install-server.sh` sets up a Bilibi server: one or more instances (Dev, Staging, Production), each with its own address, database, system service and release folders under `/opt/bilibi`. This page explains what it creates and why, how to put a release into service, and how an instance knows which one it is.

## The short version

Build both programs on your own machine, from your local Bilibi checkout:

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

Two systemd unit templates serve every instance: `bilibi@<instance>` runs `current/bilibi-web run` as `bilibi`, reads `env`, restarts after a crash and permits writes under `data/` and `incoming/`. `bilibi-installer@<instance>` runs `/opt/bilibi/build/bin/bilibi-installer` as `bilibi-build` and carries out install and undo jobs from Software. Servers have no build worker or toolchain requirement. Build releases on your own machine.

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

All builds happen in a local Bilibi checkout. The role identifies the instance; it never enables a server build.

Click the role in the status bar to open **Instance settings** (`/system/instance`). A Core Administrator in the platform-operator organisation can change the role, name, colour and update source. Every role change needs a fresh 2FA code, even on Dev, and is audited. The page warns before moving to or from Production: this changes warnings and 2FA rules without moving any data. Other accounts cannot change the role.

The name, colour and source also remain available in the generic operator settings. Empty name and colour follow the current role:

| Setting | Default | Meaning |
| --- | --- | --- |
| Instance name shown on every page | Dev, Staging or Production | What the banner and every confirm page say, like "Production" or "Dev (laptop)" |
| Banner colour | info (blue), warning (amber), danger (red) by role | One of the design's status colours |
| Install releases tested on | empty | The instance whose tested release this one installs: a name on this machine, like `dev`, or an address, like `https://staging.erp.example.com` |

In code, `bilibi_base::system::Instance::resolve` returns the role, label, colour and source together. **The instance banner, the Software page and the agent confirm pages all read it**, so a purchase order confirmed on Dev and the Software page on Dev cannot disagree about where they are.

## One machine or two

**Dev and Production on one machine** use separate databases, ports and addresses. Build in your local Bilibi checkout, install the release on Dev or Staging to check it, then promote the tested release to Production.

**Dev and Production on two machines**: run `create` once on each machine with that machine's instance. Production's "Install releases tested on" setting then holds Dev's address, and the same pairing key is set on both (Administration › System › Release files). Production downloads the release from Dev when the owner installs it; nothing is pushed to Production. A Dev the servers cannot reach (a laptop): download the release file on Dev's Release files page and upload it on Production's. The three ways, the checks and the pairing key: [`apps/base/software/README.md`](../apps/base/software/README.md), "Release transport".

Staging is optional. Set Production to install from Staging when you have one, or straight from Dev when you do not.

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

The installer reads the instance's `env` for its database and port. It copies a release from another instance's folder on this machine (the one named in "Install releases tested on"), or from `incoming/`, where the program puts a release it downloaded from a source on another machine or received as an uploaded file.

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
If nobody can sign in any more, that is account recovery, which this command does not do.
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

## The waiting page

While the service is stopped, Caddy answers every request with status 503 and the waiting page, marked with the header `X-Bilibi-Waiting`. The page says which instance is pausing (**PRODUCTION erp.example.com**, "Production is updating", in the role's colour), polls the same address every second or two, and reloads itself once an answer arrives without that header. People land back on what they asked for. A pause that runs long shows how the server owner can check the service.

The page is rendered once per instance from `scripts/waiting-page.html` into `site/waiting.html`, so the development launcher and the server share one page.

## Build locally

A local Bilibi checkout and a coding agent are the operator’s build tools. Find a Domain or Extension in Software or through MCP, read which release accepts this server and which commits CI recorded as tested together, then follow its `local_steps` in that checkout. For example:

```sh
git clone -- https://github.com/BelimbingApp/bilibi-dom-finance apps/domains/finance
git -C apps/domains/finance checkout --detach <recommended-commit>
python3 scripts/discover-modules.py
./scripts/build-assets.sh --check
cargo metadata --format-version 1 > /dev/null
python3 scripts/discover-modules.py --pin
cargo build --release --locked -p bilibi-web
./target/release/bilibi-web status
```

Keep other mounted Domains and Extensions. For an existing clone, fetch and check out the chosen commit instead. Run the composition locally against its own database and check its pages in the browser. Build for the server’s operating system, processor and system library version.

Copy `target/release/bilibi-web`, `.local/composition/manifest.txt` and `.local/composition/Cargo.lock` into a release folder. Put the first release on a test instance with `scripts/install-server.sh release` and check it there. Its Release files page downloads the existing transport bundle; upload that on Staging or Production and install it from Software, or configure the source and let the target download it. Install and undo retain the same fresh 2FA check, backup, automatic switch-back and audit path. There is no new server build or packaging path.

A folder prepared this way has no `release.json`. An uploaded file's review therefore warns that its change list is known only once the installer reads the program. The installer records the pinned composition and changes in the job's result.

When upgrading an older installation, `create`, `installer` and `release` stop and disable each former `bilibi-worker@<instance>` service, then remove that obsolete unit and its sudoers rule. Rerunning is safe, and a fresh server is left alone. The new `base-software-003-local-builds` migration cancels queued and running build jobs with local build guidance, preserving their history. Install and undo jobs continue normally.

## Reference

Script commands:

| Command | What it does |
| --- | --- |
| `create <instance> --role dev\|staging\|production --site <host> [--port <n>] [--tls internal]` | Packages, users, folders, database, `env`, service, Caddy site. Rerun to change the site; the saved role is preserved and the port stays fixed. Change the role in Instance settings with 2FA |
| `release <instance> <bilibi-web or folder>` | Copy into `releases/<n>`, migrate, switch, check |
| `create-owner <instance> [--email <address>] [--name <name>] [--password-stdin] [--company-name <name> --company-code <code>]` | The first account, the server owner; asks for anything not given, refuses once an owner exists (it then only finishes an unfinished company link) |
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
