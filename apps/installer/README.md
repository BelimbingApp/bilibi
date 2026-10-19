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
| **Check the release and copy it into place** | Find the release: the source instance's folder on this machine (`/opt/bilibi/dev/releases/3`), the folder the program downloaded it into from a source on another machine (`incoming/job-<n>/`), or an uploaded release file (`incoming/upload-<n>/`). Check its checksum; check it was built for this kind of server (a Linux program for this processor, and a system library this server has); for a sibling, check that it still runs the release and answers; check it is not already the program in service; check there is disk for the program and a backup. Copy it into the next number here (`releases/2`), check the copy, run its `status` to prove it runs on this server, and remove it from `incoming/`. A downloaded `incoming/job-<n>/` is removed whatever the outcome; an uploaded `incoming/upload-<n>/` is removed once installed or when it fails its checks, and stays for another try otherwise. | Nothing changed. The instance never paused. The message says what to fix. |
| **Back up the database** (the pause starts) | Stop the service. Caddy shows the waiting page from here. Dump the database with PostgreSQL's own `pg_dump` into `backups/job-<n>/`. | The service is started again. Nothing changed. |
| **Update the database** | Run the new program's `migrate` and then its production seeds, as one step. `migrate` is one transaction: all of it or none of it. The step is judged on the migration ledger (the table that lists the migrations applied): we count it before and after. | If the ledger did not grow, nothing changed: the previous release is started again. If it grew (`migrate` committed and a seed then failed), the database has changed shape, so the step goes back like a failed check: restore the backup, then start the previous release. |
| **Switch to the new program** | Point `releases/current` at the new release (the instance's `current` is a fixed link to it) and start the service. | **Switch back by itself.** See below. |
| **Check that it works** | Wait up to 60 seconds for the program to answer on its own port. | **Switch back by itself.** See below. |

### Going back

When the switch or the check fails, or the database update failed after it changed the database, the installer adds a step to the job, **Switch back to release N**, and does this in it: stop, restore the backup if the database changed shape, point `current` at the previous release, start it, check that it answers. The step records what it did, so the page shows it and a restart in the middle can finish it. When the instance was never restarted during the job, nothing entered is lost, because the instance was paused the whole time.

If the backup cannot be restored, the job ends `failed` with the service **left stopped** and a message that names the dump to restore by hand. It does not try again by itself.

How a release reaches this machine from another one, with the pairing key and the release file, is the program's side: `apps/base/software/README.md`, "Release transport". An install from another machine has a **Download the release from the source** step before this one, which the program carries out; the installer leaves such a job alone until that step is done.

### "Switch as soon as it is ready" or "ask me first"

The owner chooses at review time. With "ask me first" (`switch: ask`), the installer copies and checks the release, then **waits with the instance running** until the owner says go or stops the job. The waiting shows on the backup step: "Ready. Waiting for the owner to say when to switch."

### Stopping a job

A job can be stopped until the pause starts. The installer checks for a stop request in the same transaction that starts the backup step, with the job row locked, so a stop either lands before the pause or the page refuses it as too late.

## Undo

Undo takes back the newest finished install. The steps:

| Step | What we do |
| --- | --- |
| **Back up the database** (the pause starts) | Stop the service and dump the database as it is now, so even a failed Undo can be put back. |
| **Restore the backup taken before the change** (only when that change updated the database) | Count what was entered since that backup, from the audit trail, and keep the counts in the step. Then save the job history (`base_software_*`) to a file beside the safety backup, clear the database and restore the backup with `pg_restore`, and put the history back, so Undo does not erase its own record. The audit rows of the software changes written since the backup (the 2FA check and the queue record of this Undo, the recorded outcomes of the jobs carried back) are carried too. |
| **Switch back to the previous program** | Point `current` at the previous release and start it. |
| **Check that it works** | As for an install. If the previous release does not answer, a **Switch back to release N** step returns to the release that was running, with the safety backup when the database was restored. |

**When the install updated the database, Undo loses what was entered since the backup, and says so with a count.** The review on the page states it before the owner confirms ("so far 15 changes across every company on this server: 12 to finance/ledger.entry, 3 to finance/payables.bill"), and the restore step records the final count as numbers (`lost`); the app writes the sentence from them, so there is one wording. When the install only added Domains and left the database's shape alone, Undo keeps your data as it is and says that instead.

> [!WARNING]
> Undo is not available for a job whose result was never recorded, and an Undo cannot be undone. Both are the Software page's rules (`apps/base/software/README.md`).

## Backup files

**Each install and undo records its backup file, UTC time and size in the backup step.** The Software page and assistant card show those facts and the retention rule.

Backups are PostgreSQL dumps under `<instance>/backups/job-<n>/<database>.dump`. After a successful install or undo, the installer keeps the three newest backup folders and removes older ones (`flow.rs`, `prune`). Retention is by job number, with no fixed age in days. A recorded path may therefore refer to a file already removed.

Undo restores the pre-install backup when the install changed the database structure; it takes another backup before starting. If restoration fails, the job names the dump to restore by hand and leaves the service stopped.

## If the server restarts mid-way

When the installer starts and finds a job it was running, it does not carry on blindly. It asks one question: **did the change that cannot be taken back by starting the old program already happen?**

| Where the job was | What happens | The job ends |
| --- | --- | --- |
| Before the pause (copy, or waiting for the owner to say go) | The half-copied folder is removed. The remaining steps are skipped. | `interrupted`: "The installer was interrupted before the pause. Nothing changed. Start again." |
| In the pause, the database update had not committed (the dump was cut off, or the migration ledger did not grow) | The service is stopped first, then the previous release is started and checked. | `interrupted`: the previous release is back and nothing changed. |
| In the pause, the database update had committed | The service is stopped first. The seeds run again (they are safe to repeat), the service is started again if the switch was already done, then the switch and the check carry on from the first step not done. | `done`, or `failed` with the switch back |
| In a switch back, or in an Undo's restore | The service is stopped first. The step runs again from its saved state: the history file beside the backup is read instead of the database, and a restore already finished is not repeated. A finished switch back starts the release it went back to again. | `failed` (after a switch back) or `done` (after an Undo), with the right release in service |

An Undo follows the same rules: its restore (or, when it restores nothing, its switch) is the change that cannot be taken back.

What makes a restore safe to repeat:

- **The history file.** Before clearing the database, the installer saves the job history and the shape of its tables to `backups/job-<n>/history-<step>.json`, with the dump it is restoring. A run that finds the file reads it instead of the database, which a cut-off restore left empty.
- **Marked restored.** When the dump is restored and the history carried back, the file is renamed `history-<step>.restored`. A run that finds that does nothing, so the database is never restored a second time over what was entered since.
- **A cut-off restore.** On every scan, before it looks for a job, the installer looks for `history-*.json` files under `backups/job-*/`, which exist only for a restore begun and not finished. For each one whose job is not recorded as finished in the job tables (the tables may be gone, empty, or hold only the backup's rows), it stops the service and finishes the restore from the dump named in the file. It then finds the job and settles it as above. If that restore fails, the file is set aside (`.failed`) so it is not tried again on every scan.
- **A stale file.** A file whose job is recorded as finished is deleted, not restored. The installer also deletes a job's files just before it records the job's outcome, so a crash between the two leaves no file to restore from.
- **A failed restore.** If `pg_restore` fails, the job tables are made again from the file, so the job can record that it failed. The service stays stopped, and the message names the dump to restore by hand.
- **One report.** The same database error is written to the journal once, not every five seconds.

After a reboot, systemd starts `bilibi-installer@<instance>` before `bilibi@<instance>` (`Before=` in the installer's unit, `After=` and `Wants=` in the instance's). The installer stops the instance again as its first act, but the old program may have answered for a moment before that. So a message after a restart does not claim that nothing entered is lost.

A step that was running when the restart came says so in its detail: "(the installer restarted during this step and continued)".

## How it talks to the page

The installer never goes through the app. It follows the worker contract in `apps/base/software/README.md` ("How a worker plugs in"): it `LISTEN`s on the `bilibi_software_jobs` channel, claims the oldest waiting `install` or `undo` job, marks each step `running`, `done`, `failed` or `skipped` with a plain `detail`, and writes the job's final `state` and `result`. The page and the `base.software.status` contract read those rows.

The result carries what Undo and the page need:

| Key | Meaning |
| --- | --- |
| `release`, `previous_release` | the release put in service and the one it replaced |
| `migrated` | whether the database changed shape; Undo then restores the backup |
| `backup`, `backup_at` | where the backup is and when (UTC) it was taken |
| `source`, `source_release`, `sha256`, `program` | where the release came from, its number there, its checksum and its first `status` line |
| `switched_back`, `restored`, `previous_healthy` | on a failure: what the installer did to put things back |
| `platform`, `domains`, `extensions` | what the release in service is made of, read from its own `bilibi-web status`: the Platform `{"version", "release", "commit"}` and each Domain or Extension `{"id", "version", "commit"}`; null when the program could not say |
| `changes` | what differs from the release it replaced (an install) or the release it left (an Undo): `{"platform": {"from", "to"} or null, "containers": [{"layer", "id", "change", "from", "to", "from_commit", "to_commit"}]}`, where `change` is `added`, `removed`, `updated`, `downgraded` or `moved` (same version or none, new commit; the commits are short); null when either release could not say |
| `pause_seconds` | how long the instance was paused, from the moment the backup step stopped the service to the moment the release in service answered; null when the job never paused |
| `lost` | on an Undo that restored: the counts `{"since", "changes", "by_kind"}`, never a sentence |
| `message` | one or two plain sentences for the person; they name what changed and how long the pause lasted |

An `interrupted` job's result has `migrated: false` and the release in service.

The audit trail is the app's job, not the installer's: `base/software` records each finished job as one audited change the next time the server looks (every 15 seconds, and right after it starts), attributed to the worker as a system principal. The same step sends the person who asked a notification with the result's `message`, so the outcome reaches them even if they closed the page.

## Running it

Build `bilibi-installer` on your own machine, then copy the finished program to the server ([local builds](../../docs/server-install.md#build-locally)). On the server, run:

```sh
sudo scripts/install-server.sh installer /path/to/bilibi-installer
```

That copies the program to `/opt/bilibi/build/bin/bilibi-installer` and starts `bilibi-installer@<instance>` for every instance. A later `create` starts one for the new instance too. Rerun `installer` with a newer program to update it; the services restart and a job in progress is settled as in "If the server restarts mid-way".

By hand, for one instance, with its `env` file loaded:

```sh
set -a; . /opt/bilibi/production/env; set +a
bilibi-installer /opt/bilibi/production          # serve until stopped
```

It needs `DATABASE_URL` and `BIND_ADDR` from that file, the sudo rule from `scripts/server/sudoers` to start and stop `bilibi@*`, and PostgreSQL's `pg_dump` and `pg_restore` on the path. Progress lines go to the journal: `journalctl -u bilibi-installer@production`.

## Caveats

- **The first release is not its job.** A fresh instance has no program to queue a job in; `install-server.sh release` puts the first release in service.
- **Plug-ins travel with the release.** A `plugins/` folder in the release is copied with it, file by file; links are left out. Its `enabled.json` lists the plug-ins to switch on; when the job finishes `done`, the same transaction makes `base_software_plugins` match it and records `plugins: {before, after}` in the result, and the review (the program's side) lists what is switched on or off. An Undo puts `before` back, after the restore if there is one, so a restore of the database does not bring back the newer list. A release without `enabled.json` leaves the list alone.
- **The database tools must match the cluster.** `pg_dump` and `pg_restore` come with `postgresql-client-18`, which `install-server.sh create` installs.
- **A restore keeps the software's own audit trail, and nothing else entered since.** The history file carries the `software.*` and `step_up.*` actions and the `base/software.job` changes written after the backup, so the owner's 2FA check and the Undo's own queue record survive. A carried job whose recorded outcome is not in the restored audit trail is marked unsettled, and the app records it again. Every other row written after the backup is the loss the Undo counts and states before the owner confirms.
- **A restore puts back the `public` schema only.** Bilibi keeps everything there. The job history is carried across into whatever shape the restored release gives those tables; a column the older release lacks is dropped from the carried rows. The installer reads only the job columns that exist from the first `base/software` migration on, so an older dump does not break its queries.
- **The instance folder stays root's.** The installer owns `releases/` and `backups/` only, so the link it moves is `releases/current`; an instance whose `current` still points straight at a release number is converted by `install-server.sh installer` (and `create`, `release`, `switch`). If one is found unconverted, the job ends before the pause with "Nothing changed" and says to run `installer` or `create`.
- **Old releases and backups are pruned** after each finished job: the current and previous releases and the three newest numbers stay; the three newest jobs' backups stay.

## Where to go next

- The request side, the job tables and the worker contract: [`apps/base/software/README.md`](../base/software/README.md).
- The server layout, users and services: [`docs/server-install.md`](../../docs/server-install.md).
- The design behind it: the Firstmate scout report `bilibi-domain-chooser`, section 3 "Installing, on any instance".
