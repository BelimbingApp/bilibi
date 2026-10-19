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

Notice what happened at the end: **the commit asked for the owner's authenticator code, on Dev, where 2FA is otherwise off**. A build changes the server itself, so this one check is never relaxed by the instance or by a policy. Notice also what did *not* happen: nothing was built. The job waits for the builder, and `status` shows its steps as the builder reports them. An `install` or an `undo` is picked up the same way by the installer service that runs beside every instance (`apps/installer/README.md`).

You may already have questions: where do the facts come from, who may do this, what exactly is a job, and how does a builder or installer pick one up? We cover each below.

## Where each fact comes from

| Fact | Source | Why that one |
| --- | --- | --- |
| Which instance this is (Dev, Staging, Production), its label, where it installs from | `bilibi_system::Instance::resolve` | One identity for the banner, the confirm pages and this module, so they cannot disagree |
| What is installed: the Platform version and each Domain's version and commit | `bilibi_module_registry::Composition`, compiled into the program and handed in by the host as `bootstrap.composition` | The program cannot lie about what it contains |
| What can be chosen: Domains, releases, plug-ins, tested sets | The published `catalogue.json`, read over HTTPS through `base/integration` | The published copy holds the sets CI recorded (`docs/versions.md`) |
| Plug-ins switched on | `base_software_plugins` | Plug-ins load at run time, so the program cannot tell |
| Jobs and their steps | `base_software_jobs`, `base_software_steps` | The history, who asked, and what the worker reported |
| What an Undo would lose | `base_audit_mutations` since the backup, counted across every company | One database holds every company, so the loss is server-wide |

The catalogue address is a setting, **Software › Catalogue address**, for an adopter who publishes their own. A fetched copy is kept for five minutes.

## Who may change the software

Only a **server owner**: an administrator of the platform operator (the organisation that runs the server). One build changes every company on the server, so the two capabilities are platform capabilities, which need the operator identity *and* a grant:

| Capability | Lets you |
| --- | --- |
| `admin.system.software.view` | Read `status` and `catalogue` |
| `admin.system.software.manage` | Plan and commit `change`, `install`, `undo`, `cancel` |

An administrator of another organisation is refused with `denied_platform_scope`; a person in the operator's organisation without the grant with `denied_missing_capability`. Both land in the decision log.

**Every change, install, undo and pairing key asks for the owner's own authenticator code on every commit, on every instance.** The step-up operation `base.software.change` is declared `.always()` (`apps/base/step_up/README.md`): not a tenant choice, not on the floor's terms, and not turned off by `BILIBI_ENV=development`. A code is spent once, so a second commit a minute later asks again. Cancel changes nothing on the server and asks for no code.

## The contracts

Every contract here goes through Agent API (`apps/base/agent_api/README.md`): reads are called, writes are planned, reviewed by the person, then committed. The Software page calls the same contracts through `PageContracts` (below).

| Contract | Kind | Input | What the review or answer says |
| --- | --- | --- | --- |
| `base.software.status` | read | none | instance; installed Platform, Domains and Extensions; plug-ins; the job waiting or running (or the last one) with its steps and whether cancel is available; whether Undo is available; recent jobs |
| `base.software.catalogue` | read | none | the catalogue, with the Platform versions each Domain release was tested on; or `available: false` and why |
| `base.software.releases` | read | none | what this instance can install: the releases its source runs and keeps (`source`), the release files uploaded here (`uploads`), whether a pairing key is set (`pairing`), and the releases kept here (`here`); a source that cannot be asked says why in `source.message` |
| `base.software.open` | read | optional `propose` (`domains`, `plugins`: catalogue ids to pre-tick) | what the card opens with: everything `status` says, plus the catalogue on an instance that builds, the proposal checked against it (`proposal.domains`, `.plugins`, `.unknown`; or `proposal.ignored` with the reason on an instance that installs), and `card.resource`. This is the `open_software` MCP tool |
| `base.software.change` | write, Dev only | `domains` (the full wanted set; a mounted Domain left out is removed), optional `platform`, `plugins`, `switch` | the change list (add, update, downgrade, remove, keep; plug-ins switched on or off), whether every version was tested together on that Platform, warnings, a rough estimate, what happens, the steps |
| `base.software.install` | write, Staging and Production | `release` (the number the source runs now) or `bundle` (an uploaded release file, like `upload-3`), optional `switch` | the source, what changes (from the release's own recipe), the checks the installer makes, the pause |
| `base.software.undo` | write | optional `job` | which release it goes back to, and whether the backup is restored and later entries lost. `loss` names the backup's `backup_at` (the job result's, or the `backup` step's start) and carries no count: a count changes with every write, and a confirmation is bound to the review. The Software page counts the audited changes since `backup_at` with `Software::undo_loss`, only while an Undo review is open. The committed job's review records the count at commit as `lost_at_commit`, the same server-wide counts the undo result carries |
| `base.software.go` | write, 2FA | optional `job` | refused unless the job is a running change whose build is done, or a running install whose copy is done, with no later step started and `switch` `ask`. The review names the instance, the release, the pause and the steps that follow. The commit turns `switch` to `auto` in the audited transaction (the installer's `wait_for_go` sees it), sends `NOTIFY`, and notifies the requester |
| `base.software.cancel` | write, no 2FA | optional `job` | removed from the queue, or asked to stop before the pause; refused once the pause started |
| `base.software.pair` | write, 2FA every time | `key`, or `clear: true` | the pairing key is set (never shown back) or removed; see "Release transport" |

Rules the reviews enforce, each as a field error that says what to do:

- `change` on an instance that does not build, or `install` on Dev, names the right contract for that instance.
- `install` asks the source first: the release must be the one the source runs now and the source must answer, so what you install is what was tested. A source that cannot be asked (no pairing key, a wrong key, unreachable) is refused with the reason and what to do. An uploaded file is installed as it is; `releases` lists what it holds. The review lists the plug-ins the release switches on or off, and, for an uploaded file, adds a warning when it was not made by the instance this one installs from ("This file was made on Dev, not on staging, which this server normally installs from."): the file's `bundle.json` is unsigned, so this warns and never refuses.
- A Domain not in the catalogue needs a `repository` (https) and a `ref`; it is allowed and labelled with its source, and never counts as tested (captain decision 6).
- `platform` left out keeps the running Bilibi when every chosen Domain accepts it, even when that version is not a catalogue release yet (like 0.1.1 on Dev before it is published): the request then records the running version and its commit so the builder builds from it (a program built without a pin has no commit, so the review refuses and asks for a release in `platform`), and the review warns that the set is not recorded as tested there. Only when a Domain needs another release does the review pick the newest one that fits, and then it says so in its own line: "Bilibi will also change from X to Y because Finance 0.2.0 needs Platform >=0.2.0", with the extra time and the database update. A release that fits none says which Domain needs what.
- A move to an older version, of Bilibi or of a Domain, is labelled `downgrade`, never `update`, and the review warns that the database keeps what the newer release changed.
- A Domain from outside the catalogue needs an `https://` repository with no user name, token, query or fragment in it, and a `ref` that does not start with `-`: the request is stored and shown as plain data.
- A Domain id is snake_case, appears once in the request, and is never `src` (the folder that composes the Domains); the builder re-checks this before it touches a folder.
- A second request while one job waits or runs is refused and names that job.
- A request that changes nothing is refused and says what already runs.
- `undo` takes only the newest finished job, and only when the worker recorded its result (below); a finished undo cannot be undone again.

`switch` is `auto` (switch as soon as the build is ready) or `ask` (build now, wait for the owner before the one-minute pause). The worker reads it; this module only records it. **The signal to go ahead is the job's `switch` turning `auto`**: the worker polls the row while it waits with the instance running. The Software page's **Install now** button and the `base.software.go` contract give it: they turn `switch` to `auto` for a built change or a copied install that waits.

## The Software page

**Administration › System › Software** (`/system/software`, `src/web.rs`) is the same contracts with a face: it reads `status` and `catalogue`, and every write is a plan the person reviews and then commits with their authenticator code typed into the review. Server owners see and change; a person with only `admin.system.software.view` sees what runs and the history, with no controls.

| Part | What it shows | Where it comes from |
| --- | --- | --- |
| Instance banner | Which Bilibi this is, in words and colour, on the page and again at the top of every review | `bilibi_system::Instance::banner`, the shared `base/ui` component the agent consent pages also use |
| What runs here | Bilibi version, release and commit; the Domains and their versions; plug-ins on; whether the pin is exact | `status.installed`, `status.plugins` |
| Now / Last job | The job waiting, running or last finished: its steps as the worker reports them, the running step's progress bar and time left, the result, Stop or Cancel while the contract allows it, Undo when `status.undo` says so | `status.job`, `status.undo`; the page is woken by every job write (`src/follow.rs`) |
| Domains (Dev only) | One row per catalogue Domain: tick to add or remove, pick a release (marked when tested with the running Bilibi), plug-ins under it, when to switch; then **Review changes** | `catalogue`; the review is `change`'s plan preview |
| Install a tested release (Staging, Production) | The source this instance installs from, the release number to install, a pick of the release files uploaded here (`upload-<n>: release N from <label>`; usable when no source is set), **Review the install**. There is no Build here: only Dev builds | `status.instance.installs_from`, `status.uploads`; the review is `install`'s plan preview (`{release}` or `{bundle}`) |
| History | The recent jobs with outcome and times | `status.jobs` |

- **The review is the plan.** `PageContracts::preview` stores the owner's review under a plan code bound to this browser session; **Build on Dev**, **Install on …**, **Install now on …** (the go-ahead) or **Undo on …** commits that exact plan with the typed code presented on the request (`Scope::presenting_step_up`), so the owner's facade spends it the way it spends an agent's `step_up_code`. A wrong code is an error inside the review, not a trip through the browser prompt; a person not yet enrolled is sent to enrol and comes back.
- **Nothing is chosen for the person.** `?propose=finance,factory` (an assistant's link) ticks those Domains when the page opens; the review and the code are still theirs.
- **Live progress.** Every step write by a worker announces the job on `NOTIFY bilibi_software_jobs`; one connection per web process listens (`src/follow.rs`) and refreshes each mounted page, so a build's "Compiled 214 of about 488 parts" line moves every two seconds. Without that connection the page still follows on the host's 15-second refresh.

## What a job is

A job is one row in `base_software_jobs`, with its ordered steps in `base_software_steps`:

| State | Meaning | Who sets it |
| --- | --- | --- |
| `queued` | Recorded, waiting for a worker | this module |
| `running` | A worker claimed it | the worker |
| `done`, `failed` | Finished; `result` says how | the worker |
| `cancelled` | Removed from the queue, or stopped before the pause | this module (queued), the worker (running) |
| `interrupted` | The worker was restarted before the change that cannot be taken back; nothing changed and the release that was in service is running. The page offers Start again | the worker, when it starts again |

A worker that finds a job it was running asks whether the change that cannot be taken back has happened. The installer's rules (`apps/installer/README.md`, "If the server restarts mid-way"):

- **Before the pause**, or in the pause before the database update committed (the migration ledger did not grow): the job ends `interrupted`, the half-copied folder is removed, the previous release is running.
- **After it committed**, or in a switch back or an Undo's restore: the job carries on, and the cut-off step's detail is marked `resumed`.

A build has no such point, so the builder marks a cut-off build `interrupted`.

At most one job per server is `queued` or `running` (a unique index enforces it). The steps are created with the job as `pending`: for a change *check, fetch, build, backup, migrate, switch, verify*; for an install *fetch, backup, migrate, switch, verify*, with *download* first when the source is on another machine; for an undo *backup, restore (only when a backup is restored), switch, verify*. The `backup` step is marked `pauses`: from there the instance is stopped, and a cancel is no longer honoured.

Each commit writes one audited record (`base/software.job`), a `software.change`, `software.install`, `software.undo` or `software.cancel` action, the step-up rows (`step_up.*`), and one notification to the person who asked. Nothing in a job is secret: the request, the review and the result are plain data.

## How a worker plugs in

The builder (below, Dev) runs under the `bilibi-build` user as the instance's worker service, `bilibi-worker@<instance>`, which is `bilibi-web software-worker` from the release in service (`docs/server-install.md`). The installer (`apps/installer`, every instance) is a separate service, `bilibi-installer@<instance>`, under the same user. Neither goes through this module's facade; they read and write the two tables with their own connection and follow this contract:

1. **Wake up.** `LISTEN bilibi_software_jobs`. Every queue and every cancel sends `NOTIFY bilibi_software_jobs, '<job id>'` inside the commit, so it arrives once the row is visible. Also poll on start-up: a notification sent while the worker was down is lost.
2. **Claim.** `UPDATE base_software_jobs SET state='running', worker=$1, started_at=now() WHERE id=$2 AND state='queued'`. Zero rows means someone else has it, or it was cancelled.
3. **Report each step.** Set `state='running', started_at=now()` when it starts, then `done` or `failed` with `finished_at` and a plain `detail` (`{"message": "Compiled 214 of 412 parts"}`), never a secret. Skip the rest after a failure (`skipped`). A worker may append a step the job was not made with when it goes back after a failure (the installer's `switch_back`), with the next `seq`. After each of these writes, and after each rewrite of a running step's `detail`, send `NOTIFY bilibi_software_jobs, '<job id>'` so the Software page follows live (the builder's `announce`).
4. **Honour a cancel before the pause.** Before starting a step that `pauses`, in one transaction lock the job (`SELECT cancel_requested_at FROM base_software_jobs WHERE id=$1 FOR UPDATE`). If it is set, mark the remaining steps `skipped` and the job `cancelled`. If not, mark the pausing step `running` in the same transaction, so a cancel either lands before the pause or is refused as too late.
5. **Finish.** Set `state`, `finished_at` and `result`. Undo needs these keys in `result`:

   | Key | Meaning |
   | --- | --- |
   | `release` | the release number put in service |
   | `previous_release` | the one it replaced, kept for Undo |
   | `migrated` | `true` when the database changed shape; Undo then restores the backup and says what is lost |
   | `backup`, `backup_at` | where the backup is and when (UTC, `YYYY-MM-DDTHH:MM:SSZ`) it was taken; `Software::undo_loss` counts what was entered since |
   | `platform`, `domains`, `extensions`, `changes` | what the release in service is made of and what differs from the release it replaced; the installer reads them from the release's own `status`, so Done can list what changed (`apps/installer/README.md`, "How it talks to the page") |
   | `pause_seconds` | how long the instance was paused; the message for the person names it |
   | `lost` | an undo that restored a backup writes the counts only (`since`, `changes`, `by_kind`). `status` adds the sentence as `loss` (`review::loss_sentence`) |
   | `message` | one plain sentence for the person |

   A `change` job's result should also list the Domains and versions the new program holds, so `status` can show them before the program restarts. The installer's result also names the source and its checksum (`apps/installer/README.md`, "How it talks to the page").
6. **Plug-ins.** Plug-ins travel with the release, so a server runs exactly what was tested. The builder writes the plug-ins that are to be switched on once the release is in service as `plugins/enabled.json` in the release folder (the server's `base_software_plugins` list with the reviewed change applied, a list of `{id, name, publisher, version, source}`; an empty list means none). The installer reads it when it copies the release and, in the same transaction as the final `done`, makes `base_software_plugins` match it (`enabled`, `version`, `job_id`: those listed on, every other one off) and records `plugins: {before, after}` in the result. An Undo puts `before` back the same way. A release without `plugins/enabled.json` (made before plug-ins travelled) leaves the server's list as it is.
7. **Tell the person.** Record the outcome as a retained audited action by the worker's system principal (`software-builder`: `software.build.completed`, `.failed`, `.cancelled`, `.refused`, `.interrupted`) and a notification for `requested_by`. The builder does this in one `MutationBoundary` transaction with `Context::system`, recording the job's state change as a `base/software.job` mutation and re-proving the requester's account live through `Authentication::authenticate` for the notification's scope (`src/worker.rs`, `finish`). The installer has no host services, so it does neither itself: the server audits and notifies its outcome when it settles it (below). The installer creates no audit rows of its own, with one exception: it carries existing evidence rows (`software.*` and `step_up.*` actions, `base/software.job` changes) across a database restore unchanged, with their ids and times (`apps/installer/README.md`, "Caveats"). The server settles each finished job it ran as one audited change from the job as queued to the job as the worker left it, under the action `software.finished` with the worker's name as the system principal, every 15 seconds and right after it starts (`Software::settle`, run by `bilibi-web run`; a job the builder recorded itself is marked settled in that transaction). Right after settle commits, an installer-finished job also notifies `requested_by` (kind `software.job`, the result's `message` as the body), re-proving the account live as the builder does; an account that is gone gets none. The message is cut to 900 characters for the notice, and a notification that cannot be sent is logged and never keeps the job unsettled. A job the builder finished is never announced twice: it is already settled.
8. **Leave a downloading install alone.** An install whose source is on another machine has a `download` step first, which this program carries out itself (below, "Release transport"): the job stays `queued` until the step is `done`, and the installer's "oldest waiting" query skips a job whose `download` step is not done. The step's `detail` then names the folder under `incoming/` the release waits in (`folder`), and the installer's `fetch` takes it from there.
9. **Hand a built change to the installer.** A change job is two halves. The builder owns *check, fetch, build*; when `build` is `done` its `detail` holds the release number and folder (`{"release": 3, "path": "/opt/bilibi/dev/releases/3", …}`), the builder sends one more `NOTIFY` and leaves the job `running`. The installer takes a running change job whose `build` step is `done` from its first `pending` step (`backup`) and finishes it as rule 5 says. A cancel asked in between is honoured by whichever side sees it first, under the row lock of rule 4, so the two cannot cross.

The job's `request` holds exactly what the owner reviewed: `platform`, `platform_commit` and `platform_repository` (the catalogue release's commit, or the running program's own when it is between releases), `domains` with a `version`, `commit` and `repository` (catalogue) or `repository` and `ref` (outside it), `plugins`, `switch` (change); `release` and `source` (install); `job` (undo). The worker trusts it as reviewed data and still checks the server (disk, memory, toolchain) itself, because those facts change between the review and the build.

## The builder (Dev)

The builder is the worker's building half: `bilibi-web software-worker`, run by `bilibi-worker@<instance>` as `bilibi-build`. **It builds only on Dev.** The role is `bilibi_system::Instance` (that is `BILIBI_ENV`): on Staging or Production the same program refuses a change job with the reason ("This instance is Production … only a Dev instance builds") and fails it before anything is claimed or touched. Nothing on an installing instance can turn this on.

What one change job does on Dev, step by step:

| Step | What happens | What the row says |
| --- | --- | --- |
| `check` | The Rust toolchain is present (installed into `bilibi-build`'s home on the first build, about 1.5 GB; or ahead of time with `--prepare`), `git`, `python3` and a C linker exist, 8 GB are free where the cache lives, 1 GB where releases go, and the machine has 4 GB of memory (swap counts) | "Disk 42 GB free, memory 8 GB, Rust 1.99.0, …" |
| `fetch` | One kept checkout of the Platform (`BILIBI_BUILD_DIR/src`) is updated and set to the reviewed commit; each wanted Domain is cloned or updated into `apps/domains/<id>` at its commit (an outside Domain's `ref` is tried as `origin/<ref>` first, then as a tag or commit, so a kept clone never builds an old local branch); Domains not wanted are removed; the Platform's `rust-version` is checked, and the toolchain updated when it is older | "Fetched Bilibi v0.1.0 (e3fe31709a93) and finance v0.1.0 (24938fb69a4d)" |
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

## Release transport

Staging and Production never build: they install a release that Dev, or Staging, tested. The release has to get there. Three ways in, all ending in the installer's same copy-and-check step:

| Where the instances run | How the release gets there | What you set up |
| --- | --- | --- |
| Same machine | The installer copies the folder from the sibling instance's `releases/` | "Install releases tested on" names the instance, like `dev` |
| Different machines that can reach each other | The target downloads the release file from its source over HTTPS, with a pairing key | "Install releases tested on" is the address, like `https://dev.erp.example.com`; the same pairing key on both |
| A Dev the servers cannot reach (a laptop) | You download the release file on Dev's **Release files** page and upload it on the target's | nothing |

**The install review names its source first**: "Downloads from https://dev.example.com (answered just now)" for an https source, "Copied from dev on this machine" for a sibling, "From the uploaded file upload-3" for an uploaded file; the checks that follow are the checksum, the kind of server, the source's health and disk.

**The target pulls; nothing is ever pushed.** A Production instance accepts no program from anyone: it asks its source, checks the answer, downloads, checks again, and only then does the installer touch anything.

### The release file

One release travels as one file, `bilibi-release-<n>.tar.gz`: the program (`bilibi-web`), its checksum list (`SHA256SUMS`), its pinned recipe (`manifest.txt`, `Cargo.lock`, `release.json`), the plug-ins that travel with it (`plugins/`, with `enabled.json`, the plug-ins switched on; any other file in the folder travels as it is), and `bundle.json`, which names the instance it came from (label, environment, address). The file is made on demand from the release folder (`transport.rs`), so there is nothing to keep in step.

### Pairing

A pairing key is a shared secret, set once on both instances: on the source, so it answers only to its target; on the target, so it can present it. **The same key on every instance that shares releases**, 16 to 128 characters, no spaces; make one with `openssl rand -hex 24`. Set it on the Release files page (`/system/software/releases`), in the settings editor or with `base.software.pair`; whichever way, the server owner's authenticator code is asked every time (the editor through the step-up operation `base.software.pairing_key.change`, which guards this setting, `apps/base/settings_editor/README.md`), because whoever holds the key can download the whole program. It is the secret setting **Software › Pairing key other instances use to download releases from this one** (`software.pairing_key`): encrypted like every other secret, global, server owner only, listed in the settings editor next to the other secrets, and never shown back. The source checks it on a request from a machine, which has no session for a setting to be released to, so that one read goes through this module's own system principal, `software-transport`, which may read exactly this one secret and nothing else (`SystemScope::read_own_secret`, `apps/base/authz/README.md`); the key is compared in constant time.

With the key, a source answers two addresses, both reserved by the host (`apps/web/src/transport.rs`) and both read-only:

| Address | Answer |
| --- | --- |
| `GET /transport/releases` | which instance this is, the release in service, every kept release with its checksum, size and recipe (`transport::listing`) |
| `GET /transport/releases/<n>` | the release file, streamed as it is made |

The key goes in the `X-Bilibi-Pairing-Key` header. A missing or wrong key is `403` (`not_paired`, `pairing_key_refused`), a source without an instance folder or without that release is `404`, and a sender is limited to 30 attempts a minute. Every request is one system row in the exchange log; the key never reaches it.

### What happens on the target

1. The review (`base.software.install`) asks the source: a sibling's folders directly, or the source's listing over HTTPS as the person asking (the exchange log shows it under them). The release must be the one the source runs now, and the source must answer. The review then states the change list from the release's own recipe.
2. The commit queues the job. For a source on another machine the first step is **Download the release from the source**, which this program carries out (`transfer.rs`, `run_transport`, every 5 seconds beside the server): it asks the source again, checks the disk, streams the file into `incoming/job-<id>/` with progress on the step, unpacks it, checks the checksum against the listing's and that the program was built for this kind of server, and marks the step done, waking the installer. A download that fails ends the job `failed` with "Nothing changed." and the reason; the folder is removed. Cancelling the job (while it waits or downloads) removes the folder too and marks the `download` step `skipped`, and a download in flight is stopped within about two seconds (the transfer loop checks the job while it streams) and logged as stopped because the job was cancelled, not as failed, so the next install's download is not held up.
3. The installer's **Check the release and copy it into place** step takes the folder (a sibling's `releases/<n>`, `incoming/job-<id>/` or `incoming/upload-<n>/`), and makes the same checks whichever way it came: the checksum; built for this kind of server (a Linux program for this processor, read from the program itself, and a system library this server has, from `release.json`); the source still reports it healthy (same machine); not already the program in service; disk for the program and a backup. Then it copies, checks the copy, runs the copy's `status`, and removes the folder under `incoming/`. A downloaded `incoming/job-<id>/` is removed whatever the outcome, because a new try downloads again under a new job; an uploaded `incoming/upload-<n>/` stays for another try unless it was installed, failed its checks or the owner removes it. The rest is the ordinary install (`apps/installer/README.md`).

Each failed check is a plain sentence that says what to do next: "This release was built for Linux ARM 64-bit computers, and this server is Linux x86-64. Build it on a Dev of the same kind as this server."

`incoming/` is the one folder the program (user `bilibi`) and the installer (`bilibi-build`) both write: `scripts/install-server.sh create` and `installer` make it (`2770 bilibi:bilibi-build`), and the service unit allows the program to write there (the path has the `-` prefix, so an instance that has no `incoming/` yet still starts). A program run without `BILIBI_INSTANCE_DIR` keeps no releases, serves none, and refuses to receive one, each time with a sentence that says so.

### The Release files page

`/system/software/releases` (`release_files.rs`, menu **Administration › System › Release files**) is the small UI of transport: the instance banner first, so you always know which instance you are moving a release to or from; the releases kept here, each with a **Download** link (server owners); what the source offers; the upload of a release file, which is unpacked and checked at once and then waits as `upload-<n>` until you install it (or remove it); and the pairing key (setting or clearing it asks for the owner's authenticator code; the box is hidden from anyone who may only view). The page asks the source once when it opens and keeps the answer, so showing it again, sorting or saving does not ask again; **Ask the source again** asks once more, and so does saving the pairing key. The source limits each sender to 30 requests a minute, which the install review and the download need too. Installing stays with the Software page (the pick of uploaded files on its install card) and `base.software.install`.
## The card: the Software page inside a chat

One answer: `open_software` shows the same page as a card in ChatGPT (or any MCP Apps host); the person picks, reviews and confirms inside it, with the instance named on every screen.

- **What it is.** `card/software-v1.html`, one self-contained document registered as an MCP App (`src/agent.rs`, `bilibi_agent_api::App`; the mechanism is in `apps/base/agent_api/README.md`, "MCP Apps"). `/mcp` serves it as `ui://bilibi/software-v1.html`; the `open_software` tool answers `base.software.open` and points the host at it. The agent may pass `propose` to pre-tick Domains or plug-ins the person named; nothing is chosen until the person presses Build.
- **The instance first.** The banner at the top names the instance in its colour (Dev blue, Staging amber, Production red) on every screen, the review repeats it in its heading and its button ("Build on Dev?", "Install on Production?"), and Production adds a red "This changes Production. Pick a quiet time." note. The label and colour are `status`'s `instance`, which is `Instance::resolve`, so the card, the page and the agent answers cannot disagree.
- **Dev.** A card per catalogue Domain (description, modules, installed badge, version picker, whether that version was tested on a Platform release), the plug-ins, a bar counting the changes, Review. The review is `plan` `base.software.change`: the change list, warnings, "tested together", the estimate and the pause, what happens, the steps, when to switch (`auto` or `ask`), then the authenticator code and Build. The commit is `commit` with `step_up_code`; a `step_up_required` answer asks for the code again in place.
- **Staging and Production.** No Build. "Install the version tested on <source>" takes the release number the source reports (or `bundle`, an uploaded release file), then `plan` `base.software.install`: the checks, the pause, the code, Install. Without an `installs_from` setting the card says what the owner must set.
- **The job.** After a commit the card shows the steps and the builder's progress line, reading `base.software.status` every 4 seconds while the job is queued or running, with Stop while a cancel is still honoured. While it runs, the card says "You can leave this chat". Beside Stop, a line says it cancels the job before the pause and changes nothing on the instance; Stop's tooltip and accessible description repeat it. When the job ends, the card shows the result and offers Undo when `status` allows it. The card reports the queued job and the outcome to the model with `ui/update-model-context`, so the conversation can carry on ("Release 4 is built on Dev").
- **Ask the assistant.** A question typed in the card is posted into the chat with `ui/message`, prefixed with the instance and what is ticked.
- **What it never does.** Hold a credential, fetch an address of its own, frame the live page, trust a message that did not come from the host, or decide a permission: every fact and every write goes through the host's `tools/call` bridge to `read`, `plan` and `commit`, so the owner's capability and the `.always()` code apply exactly as on the page. The bridge accepts messages only from the parent window and, after the handshake, only from the origin that answered `ui/initialize`; request ids carry a random prefix, so another frame cannot forge a reply or a tool result.
- **The plugin package** (`core/agent_plugins`) lists `open_software` in its workflow skill and writes the card's own skill, `bilibi-<instance>-software`, from `CARD_SKILL` here.

## Caveats

- **A built release is not installed yet.** The installer carries out install and undo jobs; it does not yet take a built change job over from the builder (rule 9), so cancel is the way out of that state, and `scripts/install-server.sh switch <instance> <n>` puts the release in service by hand.
- **An Undo that restores a backup keeps the software's audit trail.** The installer carries the `software.*` and `step_up.*` actions and the `base/software.job` changes written after the backup across the restore, and marks a carried job whose outcome is not in the restored audit trail as unsettled, so `settle` records it again. Every other row written after the backup is lost, and the undo review counts it.
- **An unpinned program shows no Domain versions.** A developer build without `scripts/discover-modules.py --pin` reports `pin: unpinned`; the review warns that the running versions are not known exactly.
- **The catalogue is read as the person asking.** The fetch is recorded in the exchange log under them, and a server that cannot reach the address gets `available: false` with the reason.
- **A release file is held in memory once when a person downloads it** (tens of megabytes, through the download contract); a pulling instance gets it streamed. The live upload takes a file up to 64 MB: a compressed release is well under that.
- **A private certificate authority works.** The transport's three calls (the two listings and the file download) alone trust the server's own trust store as well as the public roots (`Outgoing::trust_system_roots()`, `apps/base/integration/README.md`); every other outgoing call trusts the public roots only. A source on a private name with `--tls internal` is reachable once its authority is installed on the target with `update-ca-certificates`. The same calls use `Outgoing::no_redirects()`: a source that answers with a redirect is refused in plain words, so the pairing key never goes on to another address.

## Try it

On a migrated disposable database with the walkthrough fixture, `BILIBI_ENV=development` and `BILIBI_SETTINGS_KEY` set: sign in as `ai@agent.my`, enrol the authenticator at `/settings/two-factor`, get a bearer through the device flow, then drive the calls above through `/api/v1` or `/mcp`. A commit without `step_up_code` answers `step_up_required` with `reason: every_time`; with the current code it queues the job. `SELECT * FROM base_software_jobs` and the Actions audit log show the rest.

To watch the job being built, run the worker beside the app with the same `DATABASE_URL` and `BILIBI_ENV=development`, plus `BILIBI_INSTANCE_DIR=<a scratch folder with releases/>` and `BILIBI_BUILD_DIR=<a scratch folder on a disk with 8 GB free>`: `bilibi-web software-worker --once`. `base.software.status` then shows the steps filling in, and `releases/1` appears when the build is done.

## Where to go next

- The design and the ten pieces: the Firstmate scout report `bilibi-domain-chooser`.
- Versions, the catalogue and tested sets: [`docs/versions.md`](../../../docs/versions.md).
- The server layout the workers run in: [`docs/server-install.md`](../../../docs/server-install.md).
- Step-up and `.always()`: [`apps/base/step_up/README.md`](../step_up/README.md).
- Plans, commits and `step_up_code`: [`apps/base/agent_api/README.md`](../agent_api/README.md).
