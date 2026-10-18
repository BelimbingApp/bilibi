# Running Bilibi on a server

`scripts/install-server.sh` sets up a Bilibi server: one or more instances (Dev, Staging, Production), each with its own address, database, system service and release folders under `/opt/bilibi`. This page explains what it creates and why, how to put a release into service, and how an instance knows which one it is.

## The short version

On a fresh Ubuntu or Debian server, from a Bilibi checkout, as root:

```sh
sudo scripts/install-server.sh create production --role production --site erp.example.com
sudo scripts/install-server.sh release production /path/to/bilibi-web
```

The first command installs PostgreSQL 18 and Caddy, creates the users and folders, writes the service and the proxy site, and prints what to do next. The second copies the program into `releases/1`, updates the database, points `current` at it and starts the service. Open `https://erp.example.com` and Caddy serves the app, with a certificate it fetched itself.

Want a Dev instance on the same machine to try changes first? Add one:

```sh
sudo scripts/install-server.sh create dev --role dev --site dev.erp.example.com
sudo scripts/install-server.sh release dev /path/to/bilibi-web
```

Notice what you did **not** do: pick ports, write passwords, write a unit file or a proxy config. The script generated them, and it tells you when something you asked for conflicts with what exists.

You may already have questions: where do the files go, how does a page know it is on Production, what happens to people using the app during a switch, and what about building? We cover each below.

## What the script creates

Each instance lives in one folder:

```
/opt/bilibi/
  production/
    env               the instance's settings for the service: role, address, database, keys (root:bilibi, 0640)
    releases/1/       bilibi-web, SHA256SUMS, and the build's manifest.txt and Cargo.lock when it has them
    releases/2/
    current -> releases/2        the release in service
    backups/          database backups taken before a switch (by the installer, a later piece)
    data/             the only folder the running program may write: media/ uploads, mailbox/ outgoing mail
    site/waiting.html the page Caddy shows while the service is stopped
  dev/                the same, for Dev
  build/              home of bilibi-build; on Dev, the build tools and cache land here
```

Two system users share the work, so the program that serves pages can never rewrite itself:

| User | Runs | Owns |
| --- | --- | --- |
| `bilibi` | the service `bilibi@<instance>` | `data/` only; it can read `releases/` and `env` |
| `bilibi-build` | builds (Dev) and puts releases into service | `releases/`, `backups/`, `/opt/bilibi/build`; it may start, stop and restart `bilibi@*` and reload Caddy through sudo, nothing else |

One systemd unit template serves every instance: `bilibi@production`, `bilibi@dev`. It reads `env`, runs `current/bilibi-web run` as `bilibi`, restarts it after a crash, and stops it with Ctrl-C's signal so running jobs finish. The program may write only under `data/`.

Each instance has its own PostgreSQL role and database (`bilibi_production`, `bilibi_dev`) on the server's PostgreSQL 18 cluster, with a generated password kept only in `env`. Dev cannot read Production's data.

Caddy has one site per instance in `/etc/caddy/sites/bilibi-<instance>.caddy`, imported by `/etc/caddy/Caddyfile`. The site proxies to the instance's local port and serves the waiting page while the backend is down. For a public name Caddy fetches a certificate on its own; for a private name pass `--tls internal` and Caddy signs one itself.

## How an instance knows which one it is

The role comes from one place: **`BILIBI_ENV` in the instance's `env` file**, written at creation and fixed from then on. `development` is Dev, `staging` is Staging, `production` is Production. The program already boots on this value to decide whether 2FA step-up is enforced (`README.md`, Configuration), so there is no second source to keep in step.

| Role | `BILIBI_ENV` | Builds software | Installs from | 2FA step-up |
| --- | --- | --- | --- | --- |
| Dev | `development` | Yes | itself | Off |
| Staging | `staging` | No | Dev | On |
| Production | `production` | No | Staging, or Dev directly | On |

> [!NOTE]
> The role is deliberately not a setting. A setting can be changed from a page; a server that only installs must not become one that builds because someone edited a form. Only `BILIBI_ENV` can make an instance build, and only Dev has it.

Three operator settings, under **Instance** on the settings page (`/system/settings`), say how the instance announces itself. Their defaults follow the role, so a fresh instance needs none of them:

| Setting | Default | Meaning |
| --- | --- | --- |
| Instance name shown on every page | Dev, Staging or Production | What the banner and every confirm page say, like "Production" or "Dev (laptop)" |
| Banner colour | info (blue), warning (amber), danger (red) by role | One of the design's status colours |
| Install releases tested on | empty | The instance whose tested release this one installs: a name on this machine, like `dev`, or an address, like `https://staging.erp.example.com` |

In code, `bilibi_base::system::Instance::resolve` returns the role, label, colour and source together. **The instance banner, the Software page and the agent confirm pages all read it**, so a purchase order confirmed on Dev and the Software page on Dev cannot disagree about where they are.

## One machine or two

**Dev and Production on one machine** is the cheapest setup and the script supports it: two instances, two databases, two ports, two addresses. The script warns when you create the second one: a build on Dev competes with Production for CPU and memory, so people may find Production slow while Dev builds. For a small company that is usually fine. When it matters, run Dev elsewhere.

**Dev and Production on two machines**: run `create` once on each machine with that machine's instance. Production's "Install releases tested on" setting then holds Dev's address. Moving a release between machines is the release transport, a later piece; today you copy the `bilibi-web` file across and run `release` on the target.

Staging is optional. Set Production to install from Staging when you have one, or straight from Dev when you do not.

## Putting a release into service

`release` takes a `bilibi-web` program, or a folder holding it next to the build's `manifest.txt` and `Cargo.lock`:

```sh
sudo scripts/install-server.sh release production /home/me/bilibi-0.2.1/
```

Step by step, we:

1. Copy the program into the next numbered folder, `releases/<n>`, and write its SHA-256 checksum.
2. Stop the service. From here Caddy shows the waiting page.
3. Update the database with the new program's `migrate` and apply its production seeds. Nothing migrates on boot, so this must happen before the new program serves. If it fails, the previous release is started again and nothing has changed.
4. Point `current` at `releases/<n>` and start the service.
5. Check that the backend answers within 60 seconds. If it does not, point `current` back at the previous release and start that.

`switch` goes to any kept release the same way, after checking its checksum:

```sh
sudo scripts/install-server.sh switch production 1
```

> [!WARNING]
> Migrations are forward-only. Switching back to an older release after a newer one changed tables leaves those changes in place; the older program may not expect them. That case is the Software page's Undo, which restores the backup it took first and says what would be lost. Use `switch` for releases that share the same database shape.

`status` lists every instance, its role, address, service state and releases.

## The waiting page

While the service is stopped, Caddy answers every request with status 503 and the waiting page, marked with the header `X-Bilibi-Waiting`. The page says which instance is pausing (**PRODUCTION erp.example.com**, "Production is updating", in the role's colour), polls the same address every second or two, and reloads itself once an answer arrives without that header. People land back on what they asked for. A pause that runs long shows how the server owner can check the service.

The page is rendered once per instance from `scripts/waiting-page.html` into `site/waiting.html`, so the development launcher and the server share one page.

## What is not here yet

This script is the server layout only. These are separate pieces of the Software plan:

- **Building** a release on Dev from chosen Domains, and the Software page that drives it.
- **Backups** before a switch, the health check that undoes a bad release, and **Undo**.
- **Release transport** between machines (download from the source instance, or upload a file).
- **The first account.** `release` migrates and seeds, and `platform-provision` creates the operator company (`apps/core/company/README.md`), but no command creates the first operator account on a production database yet. The development seed does it only on a developer machine.

## Reference

Script commands:

| Command | What it does |
| --- | --- |
| `create <instance> --role dev\|staging\|production --site <host> [--port <n>] [--tls internal]` | Packages, users, folders, database, `env`, service, Caddy site. Rerun to change the site; the role and port are fixed |
| `release <instance> <bilibi-web or folder>` | Copy into `releases/<n>`, migrate, switch, check |
| `switch <instance> <n>` | Put a kept release in service |
| `status [<instance>]` | Roles, addresses, service states, releases |

Keys in `env` (the service's environment; `README.md` explains each):

| Key | Set to |
| --- | --- |
| `BILIBI_ENV` | the role: `development`, `staging` or `production` |
| `APP_ORIGIN` | `https://<site>`, the exact address people browse |
| `BIND_ADDR` | `127.0.0.1:<port>`, where Caddy forwards to; the first free port from 4300 unless you pass `--port` |
| `DATABASE_URL` | the instance's own database and role on the local cluster |
| `BILIBI_SETTINGS_KEY`, `BILIBI_USER_SIGNING_KEY`, `BILIBI_MEDIA_SIGNING_KEY` | generated 64-hex-character keys; keep them, losing `BILIBI_SETTINGS_KEY` loses every secret setting and authenticator key |
| `BILIBI_MAIL_DIR` | `data/mailbox`, until an SMTP relay is set |

Files outside `/opt/bilibi`: `/etc/systemd/system/bilibi@.service`, `/etc/sudoers.d/bilibi-build`, `/etc/caddy/sites/bilibi-<instance>.caddy`, and one `import` line in `/etc/caddy/Caddyfile`.

## Where to go next

- The design behind this layout: the Software page plan, in the Firstmate scout report `bilibi-domain-chooser` (sections 2, 3 and 6).
- What each environment variable does: [`README.md`](../README.md), Configuration.
- 2FA step-up and why Dev skips it: [`apps/base/step_up/README.md`](../apps/base/step_up/README.md).
- The instance identity in code: [`apps/base/system/README.md`](../apps/base/system/README.md).
