# Base Software

`base/software` shows what this server runs, helps you find more modules, and installs tested updates with a database backup and undo. Build software in your local Bilibi checkout with a coding agent; servers never build it. No Bilimbi counterpart.

## Add a Domain

Ask the agent to find Finance, or open **Administration › System › Software**:

```
read base.software.open {"propose":{"domains":["finance"]}}
```

The answer includes Finance’s description, official repository, a release that accepts this server’s Bilibi version, whether the exact commits were recorded as tested together, and `local_steps`. A proposal highlights entries; it creates no job.

**Ask your developer to add the modules you need, build Bilibi on their computer, and test it before installing it here.** Follow the entry’s repository and commit, keeping any other mounted Domains and Extensions:

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

If it is already cloned, fetch and check out the chosen commit there instead. Extensions mount under `apps/extensions/<id>`. Run the composition locally with its own database and check its pages in the browser. Prepare a release using [`docs/server-install.md`](../../../docs/server-install.md), then upload it on **Release files**, or install it from an instance that has tested it.

## Compatibility

**Compatibility** states whether a module version supports the running Bilibi version and whether automated checks covered the exact code running on this server. The catalogue’s Platform requirement supplies version support; its CI record supplies the exact Platform and Domain commits checked.

- Prefer the newest release recorded as tested with this running commit, then the newest release whose requirements accept this version.
- Each entry includes `recommended_version`, a plain `compatibility` sentence, `local_steps`, and `next_step`. Each release includes `fits_running` and `tested_with_running`.
- No compatible release, no published release, or no exact tested pair is stated plainly. A development build or an unreleased commit does not inherit a released commit’s evidence.
- Domains were tested individually; separate pairs do not prove a combined composition. Check your composition locally.
- The published catalogue is read over HTTPS through `base/integration` as the requesting person and cached for five minutes. **Software › Catalogue address** can point to an operator’s catalogue. An unavailable catalogue returns `available: false` and a reason; installed status and installation remain available.
- `domains` and optional `extensions` hold repository and release information; extensions without CI evidence are labelled as not recorded as tested. `plugins` retains the existing release plug-in information.

## Data sources

| Fact | Source |
| --- | --- |
| Instance role, label, colour and installation source | `bilibi_system::Instance::resolve` |
| Installed Platform, Domains and Extensions | Host-supplied `bilibi_module_registry::Composition`, compiled into the program |
| Available software and CI evidence | Published `catalogue.json` ([versions contract](../../../docs/versions.md)) |
| Jobs, steps and enabled plug-ins | `base_software_jobs`, `base_software_steps`, `base_software_plugins` |
| What a backup restore loses | Server-wide `base_audit_mutations` since the backup |

## Access

Only a server owner with the appropriate platform capability can manage software, because installation affects every company.

| Capability | Allows |
| --- | --- |
| `admin.system.software.view` | Status, catalogue, release listings, Software page and card |
| `admin.system.software.manage` | Install, undo, go-ahead, cancel, release upload/download and pairing key |

Install, undo, go-ahead and pairing changes require the owner’s own fresh authenticator code on every commit, including Dev. The existing step-up key `base.software.change` remains `.always()` to preserve its policy and audit identity; it is **not a build operation**. Cancel requires no code. The facade rechecks authorization inside the mutation transaction. Other organisations fail closed with `denied_platform_scope`; a missing grant is `denied_missing_capability`.

## Agent contracts

Reads and reviewed writes use the shared Agent API; the page uses the same contracts through `PageContracts`.

| Contract | Input | Answer or review |
| --- | --- | --- |
| `base.software.status` | none | Instance, installed composition, plug-ins, uploads, current or last job with steps and cancel state, Undo and recent history |
| `base.software.catalogue` | none | Discovery, compatibility and local build instructions on every instance |
| `base.software.open` | optional `propose.domains`, `propose.extensions` | Status plus catalogue, matching and unknown proposals, local next step and `card.resource` |
| `base.software.releases` | none | Source listing, uploads, pairing state and releases kept here |
| `base.software.install` | `release` or uploaded `bundle`; optional `switch` | Source, composition changes, plug-in changes, checks, pause and installer steps |
| `base.software.undo` | optional `job` | Previous release, backup restore and backup time; only the newest completed job can be undone |
| `base.software.go` | optional `job` | Go-ahead for a running install copied and waiting with `switch: ask`, before any later step starts |
| `base.software.cancel` | optional `job` | Remove a queued job, or request stop before the pause; refused after the pause starts |
| `base.software.pair` | `key`, or `clear: true` | Set or clear the encrypted pairing key; never show it back |

The `base.software.change` agent operation is removed; only its step-up key stays (see Access). There is no server build operation, builder worker, compiler setting or builder service.

Install requires the source to answer and the selected release to be the one it runs now, or an uploaded bundle. Uploads use the same installer checks. A file made by a different instance than the configured source raises a warning; unsigned `bundle.json` is not proof of origin. Installing the program already in service or starting a second active job is refused. `switch: auto` proceeds after copying; `switch: ask` waits for `go` before the pause.

## Page and ChatGPT card

The Software page (`/system/software`) and MCP App (`ui://bilibi/software-v1.html`, `card/software-v1.html`) use the title **Manage domains and extensions**, with **Installed version**, **Available domains**, **Available extensions**, **Compatibility** and **Backups**. Developer commands, installation reviews and job history remain available on every instance.

- `?propose=finance,factory` highlights requested page entries; `open_software` accepts catalogue IDs in `propose` for the card.
- A view-only account sees discovery and status. The facade refuses writes without manage permission.
- Install, undo and go-ahead show the shared instance banner and a review, then commit that exact plan with the person’s authenticator code. The install headline uses the operation's summary to name an uploaded file, sibling instance or HTTPS source. A wrong code stays in the review.
- The page follows `NOTIFY bilibi_software_jobs` through `follow.rs`; the card polls status every four seconds while a job is active. Closing either leaves the job running.
- The card calls only the host’s read/plan/commit bridge, holds no credential and fetches no independent URL. It reports job outcomes to the model and can send the person’s question back to the chat.
- Plugin packages obtain the card guidance from `CARD_SKILL` and identify the connected instance.

## Jobs and upgrades

**Only install and undo jobs are queued.** One job per tenant can be queued or running. Jobs finish as `done`, `failed`, `cancelled` or `interrupted`; their result explains the outcome.

Install steps are *fetch, backup, migrate, switch, verify*, with *download* first for a remote source. Undo uses *backup, restore* (when needed), *switch, verify*. The first `backup` pauses the instance; cancellation is refused after it starts. The installer backs up before updating, checks the new program, and automatically switches back on a failed check. Undo restores the earlier backup when migrations changed the database; its review names the backup time and the page separately refreshes the count of later audited changes. The committed review records `lost_at_commit`.

Migration `base-software-003-local-builds` cancels legacy queued or running `change` jobs, skips unfinished steps, records the reason, gives never-claimed jobs the worker name `software-migration` and clears their settlement mark so the server audits and notifies the outcome. It preserves completed history and prior Undo information. A constraint prevents new active change jobs. The only builder setting, `software.build_jobs`, is removed. All existing tables and columns remain needed for install, undo, release plug-ins or historical evidence; no applied migration is edited.

## Database backups

**Software shows the backup file, time and size for installs and undos among the ten most recent jobs.** The page and assistant card read the same `backups` evidence from completed backup steps, including jobs that later failed; missing older metadata is labelled as not recorded. A recorded file path does not prove that the file is still kept.

- The installer writes a PostgreSQL dump under the instance folder at `backups/job-<n>/<database>.dump` before changing anything.
- After a successful install or undo, it keeps the **three newest backup folders** and removes older ones. There is no fixed age in days. This is the existing rule in `apps/installer/src/flow.rs` (`prune`); the page, reviews, job steps and card state it.
- Undo restores the pre-install backup only when the install changed the database structure. It then loses data entered since that backup. Otherwise, Undo switches the program back and leaves data alone. Undo takes a new safety backup first.
- If restoration fails, the service stays stopped and the job message names the file to restore by hand.

## How a worker plugs in

The separate installer (`apps/installer`) reads and writes the durable job tables with its own connection. It never creates audit or notification rows itself.

1. Listen on `bilibi_software_jobs` and poll on startup. Claim the oldest queued install or undo with a conditional update; ignore installs whose download step has not finished.
2. Mark each step running, then done, failed or skipped, with timestamps and a plain `detail.message`. Send `NOTIFY` after changes. A switch-back step may be appended after failure.
3. Lock the job before starting the pausing step. Honour `cancel_requested_at` there, or start the pause under the same lock, so cancel cannot cross it.
4. Finish with state, time and result. Result keys include `release`, `previous_release`, `migrated`, `backup`, `backup_at`, composition and `changes`, `pause_seconds`, and a plain `message`. An undo with a restore records `lost: {since, changes, by_kind}`; status adds its loss sentence.
5. Apply `plugins/enabled.json` from the release in the transaction that finishes the job and record `plugins: {before, after}`. Undo restores `before`. An absent file preserves existing plug-ins.
6. The server settles finished outcomes every 15 seconds and at startup through `MutationBoundary` as a `base/software.job` change under `software.finished`, attributed to the recorded worker. After commit it re-proves the requester’s live account through `Authentication` and sends a `software.job` notice. Missing accounts get no notice; notification failure never prevents settlement. Notice bodies are bounded to 900 characters.

Before a database update committed, a restarted install ends interrupted and restores the previous running release. After the update committed, or during restore/switch-back, the installer resumes. It carries existing software and step-up evidence unchanged across a restore; every other write since the backup is lost. See [`apps/installer/README.md`](../../installer/README.md).

## Release transport

Servers install releases built in the operator’s local Bilibi copy and checked before installation. The release has to get there. Three ways in, all ending in the installer's same copy-and-check step:

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

Each failed check is a plain sentence that says what to do next: "This release was built for Linux ARM 64-bit computers, and this server is Linux x86-64. Build it locally for the same kind of server."

`incoming/` is the one folder the program (user `bilibi`) and the installer (`bilibi-build`) both write: `scripts/install-server.sh create` and `installer` make it (`2770 bilibi:bilibi-build`), and the service unit allows the program to write there (the path has the `-` prefix, so an instance that has no `incoming/` yet still starts). A program run without `BILIBI_INSTANCE_DIR` keeps no releases, serves none, and refuses to receive one, each time with a sentence that says so.

### The Release files page

`/system/software/releases` (`release_files.rs`, menu **Administration › System › Release files**) is the small UI of transport: the instance banner first, so you always know which instance you are moving a release to or from; the releases kept here, each with a **Download** link (server owners); what the source offers; the upload of a release file, which is unpacked and checked at once and then waits as `upload-<n>` until you install it (or remove it); and the pairing key (setting or clearing it asks for the owner's authenticator code; the box is hidden from anyone who may only view). The page asks the source once when it opens and keeps the answer, so showing it again, sorting or saving does not ask again; **Refresh releases** asks once more, and so does saving the pairing key. The source limits each sender to 30 requests a minute, which the install review and the download need too. Installing stays with the Software page (the pick of uploaded files on its install card) and `base.software.install`.

## Caveats

- An unpinned program cannot prove its exact Domain versions or CI pair. Pin before building locally.
- The documented local release folder has no `release.json`; an uploaded file's review says its change list is known only after the installer reads the program. The completed job records its pinned composition and changes.
- Downloads by a person hold one bundle in memory; pulling instances stream it. Live uploads are limited to 64 MB.
- Transport calls trust system certificate roots as well as public roots and refuse redirects so pairing keys never go to another address. Install a private authority on the receiving server when needed.

## Where to go next

- [Local builds and server installation](../../../docs/server-install.md).
- [Repeatable fresh-server proof](../../../docs/server-proof/README.md).
- [Versions and CI evidence](../../../docs/versions.md).
- [Installer checks and recovery](../../installer/README.md).
- [Step-up](../step_up/README.md) and [Agent API plans](../agent_api/README.md).
