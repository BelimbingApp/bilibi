# Base Software

`base/software` is how a server owner changes what Bilibi is made of: which Domains it holds and at which versions. It shows what is installed, what the catalogue offers, and takes the owner's request as a job that a builder (on Dev) or an installer (on any instance) carries out. No Bilimbi counterpart: the Software page is Bilibi's own.

## One example

An assistant, acting for the server owner, asks Dev to add Finance and Factory. Over the Agent API that is three calls:

```
read   base.software.status      → "Dev, builds, runs Bilibi 0.1.1 with no Domains, no job running"
plan   base.software.change      { "domains": [{"id": "finance"}, {"id": "factory"}] }
       → review: update Bilibi 0.1.1 → 0.1.0, add Finance 0.1.0, add Factory 0.1.0,
         both tested together on 0.1.0, about 8 minutes, then a 1-minute pause
commit                           → step_up_required (every_time)
commit  + step_up_code "123456"  → job 1 queued, seven steps pending
```

Notice what happened at the end: **the commit asked for the owner's authenticator code, on Dev, where 2FA is otherwise off**. A build changes the server itself, so this one check is never relaxed by the instance or by a policy. Notice also what did *not* happen: nothing was built. The job waits for the builder, and `status` shows its steps as the builder reports them.

You may already have questions: where do the facts come from, who may do this, what exactly is a job, and how does a builder or installer pick one up? We cover each below.

## Where each fact comes from

| Fact | Source | Why that one |
| --- | --- | --- |
| Which instance this is (Dev, Staging, Production), its label, where it installs from | `bilibi_system::Instance::resolve` | One identity for the banner, the confirm pages and this module, so they cannot disagree |
| What is installed: the Platform version and each Domain's version and commit | `bilibi_module_registry::Composition`, compiled into the program and handed in by the host as `bootstrap.composition` | The program cannot lie about what it contains |
| What can be chosen: Domains, releases, plug-ins, tested sets | The published `catalogue.json`, read over HTTPS through `base/integration` | The published copy holds the sets CI recorded (`docs/versions.md`) |
| Plug-ins switched on | `base_software_plugins` | Plug-ins load at run time, so the program cannot tell |
| Jobs and their steps | `base_software_jobs`, `base_software_steps` | The history, who asked, and what the worker reported |

The catalogue address is a setting, **Software › Catalogue address**, for an adopter who publishes their own. A fetched copy is kept for five minutes.

## Who may change the software

Only a **server owner**: an administrator of the platform operator (the organisation that runs the server). One build changes every company on the server, so the two capabilities are platform capabilities, which need the operator identity *and* a grant:

| Capability | Lets you |
| --- | --- |
| `admin.system.software.view` | Read `status` and `catalogue` |
| `admin.system.software.manage` | Plan and commit `change`, `install`, `undo`, `cancel` |

An administrator of another organisation is refused with `denied_platform_scope`; a person in the operator's organisation without the grant with `denied_missing_capability`. Both land in the decision log.

**Every change, install and undo asks for the owner's own authenticator code on every commit, on every instance.** The step-up operation `base.software.change` is declared `.always()` (`apps/base/step_up/README.md`): not a tenant choice, not on the floor's terms, and not turned off by `BILIBI_ENV=development`. A code is spent once, so a second commit a minute later asks again. Cancel changes nothing on the server and asks for no code.

## The contracts

All six go through Agent API (`apps/base/agent_api/README.md`): reads are called, writes are planned, reviewed by the person, then committed. The page (a later piece) calls the same contracts through `PageContracts`.

| Contract | Kind | Input | What the review or answer says |
| --- | --- | --- | --- |
| `base.software.status` | read | none | instance; installed Platform, Domains and Extensions; plug-ins; the job waiting or running (or the last one) with its steps and whether cancel is available; whether Undo is available; recent jobs |
| `base.software.catalogue` | read | none | the catalogue, with the Platform versions each Domain release was tested on; or `available: false` and why |
| `base.software.change` | write, Dev only | `domains` (the full wanted set; a mounted Domain left out is removed), optional `platform`, `plugins`, `switch` | the change list (add, update, remove, keep), whether every version was tested together on that Platform, warnings, a rough estimate, what happens, the steps |
| `base.software.install` | write, Staging and Production | `release` (a number the source reports as tested), optional `switch` | the source, the checks the installer makes, the pause |
| `base.software.undo` | write | optional `job` | which release it goes back to, and whether the backup is restored and later entries lost |
| `base.software.cancel` | write, no 2FA | optional `job` | removed from the queue, or asked to stop before the pause; refused once the pause started |

Rules the reviews enforce, each as a field error that says what to do:

- `change` on an instance that does not build, or `install` on Dev, names the right contract for that instance.
- A Domain not in the catalogue needs a `repository` (https) and a `ref`; it is allowed and labelled with its source, and never counts as tested (captain decision 6).
- `platform` left out picks the newest release every chosen Domain accepts; a release that fits none says which Domain needs what.
- A second request while one job waits or runs is refused and names that job.
- A request that changes nothing is refused and says what already runs.
- `undo` takes only the newest finished job, and only when the worker recorded its result (below); a finished undo cannot be undone again.

`switch` is `auto` (switch as soon as the build is ready) or `ask` (build now, wait for the owner before the one-minute pause). The worker reads it; this module only records it.

## What a job is

A job is one row in `base_software_jobs`, with its ordered steps in `base_software_steps`:

| State | Meaning | Who sets it |
| --- | --- | --- |
| `queued` | Recorded, waiting for a worker | this module |
| `running` | A worker claimed it | the worker |
| `done`, `failed` | Finished; `result` says how | the worker |
| `cancelled` | Removed from the queue, or stopped before the pause | this module (queued), the worker (running) |
| `interrupted` | The worker was lost mid-way (a restart); nothing changed | the worker, when it starts again |

At most one job per server is `queued` or `running` (a unique index enforces it). The steps are created with the job as `pending`: for a change *check, fetch, build, backup, migrate, switch, verify*; for an install *fetch, backup, migrate, switch, verify*; for an undo *backup, restore (only when a backup is restored), switch, verify*. The `backup` step is marked `pauses`: from there the instance is stopped, and a cancel is no longer honoured.

Each commit writes one audited record (`base/software.job`), a `software.change`, `software.install`, `software.undo` or `software.cancel` action, the step-up rows (`step_up.*`), and one notification to the person who asked. Nothing in a job is secret: the request, the review and the result are plain data.

## How a worker plugs in

The builder (plan piece 6, Dev) and the installer (piece 5, every instance) are separate services under the `bilibi-build` user (`docs/server-install.md`). They do not go through this module's facade; they read and write the two tables with their own connection and follow this contract:

1. **Wake up.** `LISTEN bilibi_software_jobs`. Every queue and every cancel sends `NOTIFY bilibi_software_jobs, '<job id>'` inside the commit, so it arrives once the row is visible. Also poll on start-up: a notification sent while the worker was down is lost.
2. **Claim.** `UPDATE base_software_jobs SET state='running', worker=$1, started_at=now() WHERE id=$2 AND state='queued'`. Zero rows means someone else has it, or it was cancelled.
3. **Report each step.** Set `state='running', started_at=now()` when it starts, then `done` or `failed` with `finished_at` and a plain `detail` (`{"message": "Compiled 214 of 412 parts"}`), never a secret. Skip the rest after a failure (`skipped`).
4. **Honour a cancel before the pause.** Before starting a step that `pauses`, re-read `cancel_requested_at`; if set, mark the remaining steps `skipped` and the job `cancelled`.
5. **Finish.** Set `state`, `finished_at` and `result`. Undo needs these keys in `result`:

   | Key | Meaning |
   | --- | --- |
   | `release` | the release number put in service |
   | `previous_release` | the one it replaced, kept for Undo |
   | `migrated` | `true` when the database changed shape; Undo then restores the backup and says what is lost |
   | `backup` | where the backup is, when one was taken |
   | `message` | one plain sentence for the person |

   A `change` job's result should also list the Domains and versions the new program holds, so `status` can show them before the program restarts.
6. **Plug-ins.** When a job that switches plug-ins on or off finishes well, update `base_software_plugins` (`enabled`, `version`, `job_id`) in the same transaction as the final state.
7. **Tell the person.** Record the outcome as an audited action and a notification for `requested_by` (`bilibi_notifications`), as the builder piece decides.

The job's `request` holds exactly what the owner reviewed: `platform`, `domains` with a `version` (catalogue) or `repository` and `ref` (outside it), `plugins`, `switch` (change); `release` and `source` (install); `job` (undo). The worker trusts it as reviewed data and still checks the server (disk, memory, toolchain) itself, because those facts change between the review and the build.

## Caveats

- **Nothing is built yet.** This module records and shows; until the builder and installer ship, a queued job stays `queued`, and cancel is the only way out.
- **An unpinned program shows no Domain versions.** A developer build without `scripts/discover-modules.py --pin` reports `pin: unpinned`; the review warns that the running versions are not known exactly.
- **The catalogue is read as the person asking.** The fetch is recorded in the exchange log under them, and a server that cannot reach the address gets `available: false` with the reason.

## Try it

On a migrated disposable database with the walkthrough fixture, `BILIBI_ENV=development` and `BILIBI_SETTINGS_KEY` set: sign in as `ai@agent.my`, enrol the authenticator at `/settings/two-factor`, get a bearer through the device flow, then drive the calls above through `/api/v1` or `/mcp`. A commit without `step_up_code` answers `step_up_required` with `reason: every_time`; with the current code it queues the job. `SELECT * FROM base_software_jobs` and the Actions audit log show the rest.

## Where to go next

- The design and the ten pieces: the Firstmate scout report `bilibi-domain-chooser`.
- Versions, the catalogue and tested sets: [`docs/versions.md`](../../../docs/versions.md).
- The server layout the workers run in: [`docs/server-install.md`](../../../docs/server-install.md).
- Step-up and `.always()`: [`apps/base/step_up/README.md`](../step_up/README.md).
- Plans, commits and `step_up_code`: [`apps/base/agent_api/README.md`](../agent_api/README.md).
