# Base Software

`base/software` is how a server owner changes what Bilibi is made of: which Domains it holds and at which versions. It shows what is installed, what the catalogue offers, and takes the owner's request as a job that a builder (on Dev) or an installer (on any instance) carries out. No Bilimbi counterpart: the Software page is Bilibi's own.

## One example

An assistant, acting for the server owner, asks Dev to add Finance and Factory. Over the Agent API that is three calls:

```
read   base.software.status      → "Dev, builds, runs Bilibi 0.1.1 with no Domains, no job running"
plan   base.software.change      { "domains": [{"id": "finance"}, {"id": "factory"}] }
       → review: keep Bilibi 0.1.1, add Finance 0.1.0, add Factory 0.1.0,
         not recorded as tested together on 0.1.1, about 6 minutes, then a 1-minute pause
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
| `base.software.change` | write, Dev only | `domains` (the full wanted set; a mounted Domain left out is removed), optional `platform`, `plugins`, `switch` | the change list (add, update, downgrade, remove, keep; plug-ins switched on or off), whether every version was tested together on that Platform, warnings, a rough estimate, what happens, the steps |
| `base.software.install` | write, Staging and Production | `release` (a number the source reports as tested), optional `switch` | the source, the checks the installer makes, the pause |
| `base.software.undo` | write | optional `job` | which release it goes back to, and whether the backup is restored and later entries lost |
| `base.software.cancel` | write, no 2FA | optional `job` | removed from the queue, or asked to stop before the pause; refused once the pause started |

Rules the reviews enforce, each as a field error that says what to do:

- `change` on an instance that does not build, or `install` on Dev, names the right contract for that instance.
- A Domain not in the catalogue needs a `repository` (https) and a `ref`; it is allowed and labelled with its source, and never counts as tested (captain decision 6).
- `platform` left out keeps the running Bilibi when every chosen Domain accepts it, even when that version is not a catalogue release yet (like 0.1.1 on Dev before it is published): the request then records the running version and its commit so the builder builds from it (a program built without a pin has no commit, so the review refuses and asks for a release in `platform`), and the review warns that the set is not recorded as tested there. Only when a Domain needs another release does the review pick the newest one that fits, and then it says so in its own line: "Bilibi will also change from X to Y because Finance 0.2.0 needs Platform >=0.2.0", with the extra time and the database update. A release that fits none says which Domain needs what.
- A move to an older version, of Bilibi or of a Domain, is labelled `downgrade`, never `update`, and the review warns that the database keeps what the newer release changed.
- A Domain from outside the catalogue needs an `https://` repository with no user name, token, query or fragment in it, and a `ref` that does not start with `-`: the request is stored and shown as plain data.
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

The builder (below, Dev) and the installer (plan piece 5, every instance) run under the `bilibi-build` user as the instance's worker service, `bilibi-worker@<instance>`, which is `bilibi-web software-worker` from the release in service (`docs/server-install.md`). They do not go through this module's facade; they read and write the two tables with their own connection and follow this contract:

1. **Wake up.** `LISTEN bilibi_software_jobs`. Every queue and every cancel sends `NOTIFY bilibi_software_jobs, '<job id>'` inside the commit, so it arrives once the row is visible. Also poll on start-up: a notification sent while the worker was down is lost.
2. **Claim.** `UPDATE base_software_jobs SET state='running', worker=$1, started_at=now() WHERE id=$2 AND state='queued'`. Zero rows means someone else has it, or it was cancelled.
3. **Report each step.** Set `state='running', started_at=now()` when it starts, then `done` or `failed` with `finished_at` and a plain `detail` (`{"message": "Compiled 214 of 412 parts"}`), never a secret. Skip the rest after a failure (`skipped`).
4. **Honour a cancel before the pause.** Before starting a step that `pauses`, in one transaction lock the job (`SELECT cancel_requested_at FROM base_software_jobs WHERE id=$1 FOR UPDATE`). If it is set, mark the remaining steps `skipped` and the job `cancelled`. If not, mark the pausing step `running` in the same transaction, so a cancel either lands before the pause or is refused as too late.
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
7. **Tell the person.** Record the outcome as a retained audited action by the worker's system principal (`software-builder`: `software.build.completed`, `.failed`, `.cancelled`, `.refused`, `.interrupted`) and a notification for `requested_by`. The builder does this in one `MutationBoundary` transaction with `Context::system`, recording the job's state change as a `base/software.job` mutation and re-proving the requester's account live through `Authentication::authenticate` for the notification's scope (`src/worker.rs`, `finish`); the installer should do the same.
8. **Hand a built change to the installer.** A change job is two halves. The builder owns *check, fetch, build*; when `build` is `done` its `detail` holds the release number and folder (`{"release": 3, "path": "/opt/bilibi/dev/releases/3", …}`), the builder sends one more `NOTIFY` and leaves the job `running`. The installer takes a running change job whose `build` step is `done` from its first `pending` step (`backup`) and finishes it as rule 5 says. A cancel asked in between is honoured by whichever side sees it first, under the row lock of rule 4, so the two cannot cross.

The job's `request` holds exactly what the owner reviewed: `platform`, `platform_commit` and `platform_repository` (the catalogue release's commit, or the running program's own when it is between releases), `domains` with a `version`, `commit` and `repository` (catalogue) or `repository` and `ref` (outside it), `plugins`, `switch` (change); `release` and `source` (install); `job` (undo). The worker trusts it as reviewed data and still checks the server (disk, memory, toolchain) itself, because those facts change between the review and the build.

## The builder (Dev)

The builder is the worker's building half: `bilibi-web software-worker`, run by `bilibi-worker@<instance>` as `bilibi-build`. **It builds only on Dev.** The role is `bilibi_system::Instance` (that is `BILIBI_ENV`): on Staging or Production the same program refuses a change job with the reason ("This instance is Production … only a Dev instance builds") and fails it before anything is claimed or touched. Nothing on an installing instance can turn this on.

What one change job does on Dev, step by step:

| Step | What happens | What the row says |
| --- | --- | --- |
| `check` | The Rust toolchain is present (installed into `bilibi-build`'s home on the first build, about 1.5 GB; or ahead of time with `--prepare`), `git`, `python3` and a C linker exist, 8 GB are free where the cache lives, 1 GB where releases go, and the machine has 4 GB of memory (swap counts) | "Disk 42 GB free, memory 8 GB, Rust 1.99.0, …" |
| `fetch` | One kept checkout of the Platform (`BILIBI_BUILD_DIR/src`) is updated and set to the reviewed commit; each wanted Domain is cloned or updated into `apps/domains/<id>` at its commit; Domains not wanted are removed; the Platform's `rust-version` is checked, and the toolchain updated when it is older | "Fetched Bilibi v0.1.0 (e3fe31709a93) and finance v0.1.0 (24938fb69a4d)" |
| `build` | `scripts/discover-modules.py`, `cargo metadata`, `discover-modules.py --pin`, then `cargo build --release --locked -p bilibi-web` into the kept cache, at the lowest CPU priority; the program, `manifest.txt`, `Cargo.lock`, `SHA256SUMS` and a `release.json` are copied into the next `releases/<n>`, and the new program's own `status` is run to prove it starts | "Compiled 214 of about 488 parts, about 7 minutes left", then "Built release 3 in 9 min 12 s" |

- **Progress with a time estimate.** While `cargo` compiles, the `build` step's `detail` is rewritten every two seconds with `compiled`, `total`, `fresh` (parts taken from the cache), `seconds_left` and a plain `message`. The estimate uses the pace of the build so far once a fifth of the parts are done, before that the pace of the last build here (`BILIBI_BUILD_DIR/history.json`), and before any build a default of about a second and a half per part.
- **The cache makes later builds faster.** The Platform checkout, the Domain checkouts and Cargo's target folder are kept under `BILIBI_BUILD_DIR`, so a second build of the same set compiles only what changed and reports most parts as `fresh`.
- **Cancel.** The builder looks for `cancel_requested_at` every two seconds; a cancel stops the running command and its compilers (the whole process group), marks the current step `skipped`, the rest `skipped` and the job `cancelled`, and says "Stopped before anything changed. Dev still runs …". Nothing in the running instance was touched, because the builder never touches it.
- **Failure.** A step that fails names the cause in plain words (a Domain commit that is not in its repository, a compile error, a compiler killed for memory with exit 137, a missing package with the `apt-get` line to run) and keeps the tail of the tool's output in `detail.details`; the full output is in `BILIBI_BUILD_DIR/logs/job-<id>.log`. The job's `result.message` starts with "Nothing changed."
- **Stop and restart.** `systemctl stop bilibi-worker@dev` (Ctrl-C) during a build stops the compilers and marks the job `interrupted` at once ("Nothing changed. Start the change again; the parts already compiled are kept"); a crash leaves the same mark when the worker starts again. A job already handed to the installer is left alone.
- **Settings.** *Software › Compilers at a time* (`software.build_jobs`, default 2; 0 = use every processor) caps the compiler processes of a build, so a Dev that shares its machine with Production stays responsive. Builds also run at the lowest CPU priority.
- **Clean environment.** Every program the builder starts (Git, Python, Cargo and so a Domain's build scripts, `sha256sum`, rustup, the new `bilibi-web status`) runs with an empty environment plus `PATH` (toolchain first), `HOME`, `LANG`, `CARGO_TARGET_DIR`, `CARGO_TERM_COLOR`, `GIT_TERMINAL_PROMPT` and, when set, `CARGO_BUILD_JOBS`. The instance's `DATABASE_URL` and `BILIBI_*` keys never reach them.
- **Paths.** `BILIBI_INSTANCE_DIR` (the instance folder, whose `releases/` receives the result) and `BILIBI_BUILD_DIR` (the builder's folder; default `/opt/bilibi/build`) come from the instance's `env` file; the toolchain lives in the worker user's `$HOME` (`~/.cargo`, `~/.rustup`), so an operator's own Cargo configuration there applies.

`bilibi-web software-worker --once` handles the job waiting now and exits; `--prepare` installs the toolchain now (Dev only) and exits.

## Caveats

- **A built release is not installed yet.** The builder leaves a change job `running` at its `backup` step with the release in `releases/<n>`; until the installer ships, cancel is the way out of that state, and `scripts/install-server.sh switch <instance> <n>` puts the release in service by hand.
- **An install or undo job waits.** Only the builder exists today; a queued `install` or `undo` job stays `queued` until the installer ships, and cancel is the way out.
- **An unpinned program shows no Domain versions.** A developer build without `scripts/discover-modules.py --pin` reports `pin: unpinned`; the review warns that the running versions are not known exactly.
- **The catalogue is read as the person asking.** The fetch is recorded in the exchange log under them, and a server that cannot reach the address gets `available: false` with the reason.

## Try it

On a migrated disposable database with the walkthrough fixture, `BILIBI_ENV=development` and `BILIBI_SETTINGS_KEY` set: sign in as `ai@agent.my`, enrol the authenticator at `/settings/two-factor`, get a bearer through the device flow, then drive the calls above through `/api/v1` or `/mcp`. A commit without `step_up_code` answers `step_up_required` with `reason: every_time`; with the current code it queues the job. `SELECT * FROM base_software_jobs` and the Actions audit log show the rest.

To watch the job being built, run the worker beside the app with the same `DATABASE_URL` and `BILIBI_ENV=development`, plus `BILIBI_INSTANCE_DIR=<a scratch folder with releases/>` and `BILIBI_BUILD_DIR=<a scratch folder on a disk with 8 GB free>`: `bilibi-web software-worker --once`. `base.software.status` then shows the steps filling in, and `releases/1` appears when the build is done.

## Where to go next

- The design and the ten pieces: the Firstmate scout report `bilibi-domain-chooser`.
- Versions, the catalogue and tested sets: [`docs/versions.md`](../../../docs/versions.md).
- The server layout the workers run in: [`docs/server-install.md`](../../../docs/server-install.md).
- Step-up and `.always()`: [`apps/base/step_up/README.md`](../step_up/README.md).
- Plans, commits and `step_up_code`: [`apps/base/agent_api/README.md`](../agent_api/README.md).
