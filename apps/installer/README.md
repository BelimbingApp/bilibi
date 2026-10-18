# Bilibi installer

`bilibi-installer` is the small program beside every Bilibi instance that puts a release into service when a server owner asks for it on the Software page, and takes it back when they press Undo. It runs as its own service under the `bilibi-build` user, so the program people use never rewrites itself. No Bilimbi counterpart: this is Bilibi's own.

## One example

Production is set to install releases tested on Dev. The owner opens the Software page on Production, chooses "Install release 3 from dev", types their 2FA code, and confirms. Over the next two minutes the page shows:

```
1. Copy the release and check it           done    Copied dev release 3 as release 2 here: Bilibi 0.1.1, ...
2. Back up the database (Production pauses from here)
                                           done    production is paused. Backed up the database (14 MB).
3. Update the database                     done    Applied 1 migration as one piece.
4. Switch to the new program               done    current points at release 2; the service is starting
5. Check that it works                     done    Release 2 answered after 2 s
Done: Release 2 is in service, installed from dev release 3. The database was updated.
```

Notice what the owner did **not** do: copy a file, stop a service, run `pg_dump`, run `migrate`, or watch a terminal. Notice also what happened to the people using Production: for about a minute every page showed "Production is updating" and then reloaded where they were.

You may already have questions: what if the new release does not work, what does Undo lose, what happens when the server restarts half way, and how does this program talk to the page? We cover each below.

## What each step does

| Step | What we do | If it fails |
| --- | --- | --- |
| **Copy the release and check it** | Read the release from the source instance's folder on this machine (`/opt/bilibi/dev/releases/3`), check its checksum, check that the source still runs it and answers, check there is disk for the program and a backup, copy it into the next number here (`releases/2`), check the copy, and run its `status` to prove it runs on this server. | Nothing changed. The instance never paused. The message says what to fix. |
| **Back up the database** (the pause starts) | Stop the service. Caddy shows the waiting page from here. Dump the database with PostgreSQL's own `pg_dump` into `backups/job-<n>/`. | The service is started again. Nothing changed. |
| **Update the database** | Run the new program's `migrate` and its production seeds. `migrate` is one transaction: all of it or none of it. We also note whether the database changed shape, because Undo needs to know. | The previous release is started again. Nothing changed. |
| **Switch to the new program** | Point `releases/current` at the new release (the instance's `current` is a fixed link to it) and start the service. | See the check below. |
| **Check that it works** | Wait up to 60 seconds for the program to answer on its own port. | **Switch back by itself**: stop, restore the backup if the database changed shape, point `current` at the previous release, start it, and check that it answers. Nothing entered is lost, because the instance was paused the whole time. |

A release is only copied from an instance on the same machine for now. Downloading one from another machine is a later piece; the step says so plainly when the source is an address.

### "Switch as soon as it is ready" or "ask me first"

The owner chooses at review time. With "ask me first" (`switch: ask`), the installer copies and checks the release, then **waits with the instance running** until the owner says go or stops the job. The waiting shows on the backup step: "Ready. Waiting for the owner to say when to switch."

### Stopping a job

A job can be stopped until the pause starts. The installer checks for a stop request in the same transaction that starts the backup step, with the job row locked, so a stop either lands before the pause or the page refuses it as too late.

## Undo

Undo takes back the newest finished install. The steps:

| Step | What we do |
| --- | --- |
| **Back up the database** (the pause starts) | Stop the service and dump the database as it is now, so even a failed Undo can be put back. |
| **Restore the backup taken before the change** (only when that change updated the database) | Count what was entered since that backup, from the audit trail. Then clear the database and restore the backup with `pg_restore`. The job history (`base_software_*`) is taken aside first and put back after, so Undo does not erase its own record. |
| **Switch back to the previous program** | Point `current` at the previous release and start it. |
| **Check that it works** | As for an install. If the previous release does not answer, we return to the release that was running, with the safety backup when the database was restored. |

**When the install updated the database, Undo loses what was entered since the backup, and says so with a count.** The review on the page states it before the owner confirms ("so far 15 changes across every company on this server: 12 to finance/ledger.entry, 3 to finance/payables.bill"), and the restore step records the final count. When the install only added Domains and left the database's shape alone, Undo keeps your data as it is and says that instead.

> [!WARNING]
> Undo is not available for a job whose result was never recorded, and an Undo cannot be undone. Both are the Software page's rules (`apps/base/software/README.md`).

## If the server restarts mid-way

Every step can run again from where it was, so **the installer resumes the job** when its service comes back (systemd starts it after a reboot):

- During the copy: the half-copied folder is removed and the copy starts over.
- During the backup: the dump is taken again.
- During the database update: `migrate` runs again. It is one transaction, so a cut-off run left nothing behind, and a run that had committed has nothing left to apply.
- During the switch or the check: `current` is pointed again, the service started, the check made.

The step that was cut off says so in its detail: "(the installer restarted during this step and continued)". The `interrupted` state in the job tables is for work that cannot resume, such as a build.

## How it talks to the page

The installer never goes through the app. It follows the worker contract in `apps/base/software/README.md` ("How a worker plugs in"): it `LISTEN`s on the `bilibi_software_jobs` channel, claims the oldest waiting `install` or `undo` job (a `change`, a build, waits for the builder), marks each step `running`, `done`, `failed` or `skipped` with a plain `detail`, and writes the job's final `state` and `result`. The page and the `base.software.status` contract read those rows.

The result carries what Undo and the page need:

| Key | Meaning |
| --- | --- |
| `release`, `previous_release` | the release put in service and the one it replaced |
| `migrated` | whether the database changed shape; Undo then restores the backup |
| `backup`, `backup_at` | where the backup is and when (UTC) it was taken |
| `source`, `source_release`, `sha256`, `program` | where the release came from, its number there, its checksum and its first `status` line |
| `switched_back`, `restored`, `previous_healthy` | on a failure: what the installer did to put things back |
| `lost` | on an Undo that restored: `{"since", "changes", "by_kind"}` |
| `message` | one or two plain sentences for the person |

The audit trail is the app's job, not the installer's: `base/software` records each finished job as one audited change the next time the server looks (every 15 seconds, and right after it starts), attributed to the worker as a system principal.

## Running it

`scripts/install-server.sh` does this on a server:

```sh
cargo build --release -p bilibi-installer
sudo scripts/install-server.sh installer target/release/bilibi-installer
```

That copies the program to `/opt/bilibi/build/bin/bilibi-installer` and starts `bilibi-installer@<instance>` for every instance. A later `create` starts one for the new instance too. Rerun `installer` with a newer program to update it; the services restart and a job in progress resumes.

By hand, for one instance, with its `env` file loaded:

```sh
set -a; . /opt/bilibi/production/env; set +a
bilibi-installer /opt/bilibi/production          # serve until stopped
bilibi-installer /opt/bilibi/production --once   # work through what is waiting, then exit
```

It needs `DATABASE_URL` and `BIND_ADDR` from that file, the sudo rule from `scripts/server/sudoers` to start and stop `bilibi@*`, and PostgreSQL's `pg_dump` and `pg_restore` on the path. Progress lines go to the journal: `journalctl -u bilibi-installer@production`.

## Caveats

- **The first release is not its job.** A fresh instance has no program to queue a job in; `install-server.sh release` puts the first release in service.
- **The database tools must match the cluster.** `pg_dump` and `pg_restore` come with `postgresql-client-18`, which `install-server.sh create` installs.
- **A restore puts back the `public` schema only.** Bilibi keeps everything there. The job history is carried across into whatever shape the restored release gives those tables; a column the older release lacks is dropped from the carried rows.
- **The instance folder stays root's.** The installer owns `releases/` and `backups/` only, so the link it moves is `releases/current`; an instance whose `current` still points straight at a release number refuses to switch and says to rerun `install-server.sh create`.
- **Old releases and backups are pruned** after each finished job: the current and previous releases and the three newest numbers stay; the three newest jobs' backups stay.

## Where to go next

- The request side, the job tables and the worker contract: [`apps/base/software/README.md`](../base/software/README.md).
- The server layout, users and services: [`docs/server-install.md`](../../docs/server-install.md).
- The design behind it: the Firstmate scout report `bilibi-domain-chooser`, section 3 "Installing, on any instance".
