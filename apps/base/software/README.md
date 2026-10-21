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

If it is already cloned, fetch and check out the chosen commit there instead. Extensions mount under `apps/extensions/<id>`. Run the composition locally with its own database and check its pages in the browser. Prepare a release using [`docs/server-install.md`](../../../docs/server-install.md), then upload it on **Software › Releases**, or publish the setup archive on GitHub.

## Compatibility

Official modules (repositories under BelimbingApp) are checked against every Platform release before it ships, so they carry a plain `status` (`installed`, `not_installed`, `update_available`), `latest_version` and, only when no version fits, a `warning`. For modules from any other catalogue, **Compatibility** states whether a version supports the running Bilibi version and whether automated checks covered the exact code running on this server. The catalogue’s Platform requirement supplies version support; its CI record supplies the exact Platform and Domain commits checked.

- Prefer the newest release recorded as tested with this running commit, then the newest release whose requirements accept this version.
- Each entry includes `recommended_version`, a plain `compatibility` sentence, `official`, `local_steps`, and `next_step`. Each release includes `fits_running` and `tested_with_running`.
- No compatible release, no published release, or no exact tested pair is stated plainly. A development build or an unreleased commit does not inherit a released commit’s evidence.
- Domains were tested individually; separate pairs do not prove a combined composition. Check your composition locally.
- The published catalogue is read over HTTPS through `base/integration` as the requesting person and cached for five minutes. **Software › Catalogue address** can point to an operator’s catalogue. An unavailable catalogue returns `available: false` and a reason; installed status and installation remain available.
- `domains` and optional `extensions` hold repository and release information; extensions without CI evidence are labelled as not recorded as tested. `plugins` retains the existing release add-on information.

## Data sources

| Fact | Source |
| --- | --- |
| Instance role, label and colour | `bilibi_system::Instance::resolve` |
| Installed Platform, Domains and Extensions | Host-supplied `bilibi_module_registry::Composition`, compiled into the program |
| Available software and CI evidence | Published `catalogue.json` ([versions contract](../../../docs/versions.md)) |
| Jobs, steps and enabled add-ons | `base_software_jobs`, `base_software_steps`, `base_software_plugins` |
| What a backup restore loses | Server-wide `base_audit_mutations` since the backup |

## Access

Only a server owner with the appropriate platform capability can manage software, because installation affects every company.

| Capability | Allows |
| --- | --- |
| `admin.system.software.view` | Status, catalogue, release listings, Software page and card |
| `admin.system.software.manage` | Install, undo, go-ahead, cancel, release upload and GitHub settings |

Install, undo, go-ahead and GitHub settings require the owner’s own fresh 2FA code on every commit, including Dev. The existing step-up key `base.software.change` remains `.always()` to preserve its policy and audit identity; it is **not a build operation**. Cancel requires no code. The facade rechecks authorization inside the mutation transaction. Other organisations fail closed with `denied_platform_scope`; a missing grant is `denied_missing_capability`.

## Agent contracts

Reads and reviewed writes use the shared Agent API; the page uses the same contracts through `PageContracts`.

| Contract | Input | Answer or review |
| --- | --- | --- |
| `base.software.status` | none | Instance, installed composition, add-ons, uploads, current or last job with steps and cancel state, Undo and recent history |
| `base.software.catalogue` | none | Discovery, compatibility and local build instructions on every instance |
| `base.software.open` | optional `propose.domains`, `propose.extensions` | Status plus catalogue, matching and unknown proposals, local next step and `card.resource` |
| `base.software.releases` | optional `refresh`, `prerelease` | Exact GitHub assets, installed identity and uploads |
| `base.software.install` | Uploaded `bundle`, or GitHub `asset` with `repository_id`, `release_id`, `tag`, `target` and `digest`; optional `switch`, `staging` report | Source, composition changes, add-on changes, checks, pause and installer steps |
| `base.software.github` | `repository`, optional `repository_id`, `token`, `clear_token`, `refresh_minutes`, `accept` | Save the GitHub repository, the read token and how often to check; accept the repository number before installing. The token is never shown back |
| `base.software.undo` | optional `job` | Previous release, backup restore and backup time; only the newest completed job can be undone |
| `base.software.go` | optional `job` | Go-ahead for a running install copied and waiting with `switch: ask`, before any later step starts |
| `base.software.cancel` | optional `job` | Remove a queued job, or request stop before the pause; refused after the pause starts |

The `base.software.change` agent operation is removed; only its step-up key stays (see Access). There is no server build operation, builder worker, compiler setting or builder service.

Install takes an exact GitHub asset or an uploaded bundle. Installing the program already in service or starting a second active job is refused. `switch: auto` proceeds after copying; `switch: ask` waits for `go` before the pause.

## Page and ChatGPT card

The **Software** page (`/system/software`) has one menu entry and two shared tabs: **Domains and Extensions** and **Releases**. Each catalogue item is one compact card with its description, installed version (when installed) and one **Status** line. Official modules show **Installed**, **Not installed** or **Update available: X**, plus a short warning such as **Needs Bilibi X** only when no listed version fits this server. Modules from an operator’s own catalogue are not kept in step with the Platform, so they keep a **Compatibility** line: **Yes · tested with Bilibi X**, **Yes, not tested yet** or **No · needs Bilibi X**. **Developer commands** folds away the repository, exact test evidence and local build commands.

**Releases** (`?tab=releases`) shows the installed version, the current or last install (its **Install steps** stay open while the job is active, failed or interrupted, and fold when it completes), a short backup table with folded backup details, and **Get an update**. Backup file locations are folded and only available to accounts that can manage software. GitHub releases and their identities appear above a folded **Upload a release file** section, with queued files and their review on the same mounted page; switching tabs preserves pending uploads and reviews. The old `/system/software/releases` page redirects to the Releases tab.

The MCP App (`ui://bilibi/software-v1.html`, `card/software-v1.html`) keeps its **Manage domains and extensions** title and uses the same software contracts.

- `?propose=finance,factory` highlights requested page entries; `open_software` accepts catalogue IDs in `propose` for the card.
- A view-only account sees discovery and status. The facade refuses writes without manage permission.
- Install, undo and go-ahead show the shared instance banner and a review, then commit that exact plan with the person’s chosen 2FA method and code through `ProofInput`. The install headline uses the operation's summary to name an uploaded file or GitHub asset. A wrong code stays in the review.
- The page follows `NOTIFY bilibi_software_jobs` through `follow.rs`; the card polls status every four seconds while a job is active. Closing either leaves the job running.
- The card calls only the host’s read/plan/commit bridge, holds no credential and fetches no independent URL. It reports job outcomes to the model and can send the person’s question back to the chat.
- Plugin packages obtain the card guidance from `CARD_SKILL` and identify the connected instance.
- The History column picker also offers **Worker**, matching the job panel. Its optional columns use shared record fields; the default columns stay the same. Steps and long result messages stay on the panel.

## Jobs and upgrades

**Only install and undo jobs are queued.** One job per tenant can be queued or running. Jobs finish as `done`, `failed`, `cancelled` or `interrupted`; their result explains the outcome.

Install steps are *fetch, backup, migrate, switch, verify*, with *download* first for a GitHub file. Undo uses *backup, restore* (when needed), *switch, verify*. The first `backup` pauses the instance; cancellation is refused after it starts. The installer backs up before updating, checks the new program, and automatically switches back on a failed check. Undo restores the earlier backup when migrations changed the database; its review names the backup time and the page separately refreshes the count of later audited changes. The committed review records `lost_at_commit`.

Migration `base-software-003-local-builds` cancels legacy queued or running `change` jobs, skips unfinished steps, records the reason, gives never-claimed jobs the worker name `software-migration` and clears their settlement mark so the server audits and notifies the outcome. It preserves completed history and prior Undo information. A constraint prevents new active change jobs. The only builder setting, `software.build_jobs`, is removed. All existing tables and columns remain needed for install, undo, release add-ons or historical evidence; no applied migration is edited.

## Database backups

**Software shows each backup's time and size for installs and undos among the ten most recent jobs; only accounts that can manage software see the file.** The page and assistant card read the same `backups` evidence from completed backup steps, including jobs that later failed; missing older metadata is labelled as not recorded. A view-only account gets no file path from the page, `base.software.status` or `base.software.open`: the `backup` fields are dropped and job messages that name a file say "a backup file on this server" instead. A recorded file path does not prove that the file is still kept.

- The installer writes a PostgreSQL dump under the instance folder at `backups/job-<n>/<database>.dump` before changing anything.
- After a successful install or undo, it keeps the newest `KEEP_BACKUPS` backup folders (`apps/installer/src/flow.rs`, `prune`) and removes older ones. There is no fixed age in days. The backup step records the number as `keep_backups`; `status` carries the sentence once as `backup_rule`, and only the Backups section states it.
- Undo restores the pre-install backup only when the install changed the database structure. It then loses data entered since that backup. Otherwise, Undo switches the program back and leaves data alone. Undo takes a new safety backup first.
- If restoration fails, the service stays stopped and the job message names the file to restore by hand.

## How a worker plugs in

The separate installer (`apps/installer`) reads and writes the durable job tables with its own connection. It never creates audit or notification rows itself.

1. Listen on `bilibi_software_jobs` and poll on startup. Claim the oldest queued install or undo with a conditional update; ignore installs whose download step has not finished.
2. Mark each step running, then done, failed or skipped, with timestamps and a plain `detail.message`. Send `NOTIFY` after changes. A switch-back step may be appended after failure.
3. Lock the job before starting the pausing step. Honour `cancel_requested_at` there, or start the pause under the same lock, so cancel cannot cross it.
4. Finish with state, time and result. Result keys include `release`, `previous_release`, `migrated`, `backup`, `backup_at`, composition and `changes`, `pause_seconds`, and a plain `message`. An undo with a restore records `lost: {since, changes, by_kind}`; status adds its loss sentence.
5. Apply `plugins/enabled.json` from the release in the transaction that finishes the job and record `plugins: {before, after}`. Undo restores `before`. An absent file preserves existing add-ons.
6. The server settles finished outcomes every 15 seconds and at startup through `MutationBoundary` as a `base/software.job` change under `software.finished`, attributed to the recorded worker. After commit it re-proves the requester’s live account through `Authentication` and sends a `software.job` notice. Missing accounts get no notice; notification failure never prevents settlement. Notice bodies are bounded to 900 characters.

Before a database update committed, a restarted install ends interrupted and restores the previous running release. After the update committed, or during restore/switch-back, the installer resumes. It carries existing software and step-up evidence unchanged across a restore; every other write since the backup is lost. See [`apps/installer/README.md`](../../installer/README.md).

## Release archives

Install a published GitHub asset or upload a saved archive. Both paths use the same installer checks, backup, switch-back and Undo.

A gzip-compressed tar carries `bilibi-web`, `SHA256SUMS` and optionally `manifest.txt`, `Cargo.lock`, `release.json`, `bundle.json` and `plugins/`. The public [setup template](https://github.com/BelimbingApp/bilibi-setup-template) builds and packages the complete composition on GitHub. A local build can use the archive commands in [server installation](../../../docs/server-install.md). Archive parsing, safe paths and compatibility checks live in private `src/transport.rs`.

### Install from GitHub

Name one repository on **Software › Releases**, as owner/name, for example BelimbingApp/bilibi-setup-template. A public repository needs no token, but GitHub then allows only 60 calls an hour from a server's address, and each install review uses about three. A token raises that to 5,000 an hour. A private repository needs a fine-grained token that can only read that repository. The token is stored encrypted and never shown again. Changing or clearing it asks for your 2FA code, including in the settings editor.

The first time you save a repository, Bilibi asks GitHub for its number and remembers that number only when you choose **Accept this repository**. That also asks for your 2FA code. If GitHub later reports a different number for the same name, installs stop until you accept the new number. Bilibi does not replace a remembered number by itself.

The list shows setup files for this server: names that start with `bilibi-setup-` and end with this computer's target, such as `-x86_64-unknown-linux-gnu.tar.gz`. Drafts stay hidden. Pre-releases stay hidden until you choose **Show pre-releases**. The list is kept on this server and refreshed at most as often as **Check GitHub every this many minutes** (15 by default), and again when you choose **Refresh**. The list is saved in the instance's `data/` folder. If GitHub is limiting this server, or cannot be reached, the saved list stays on the page, and a failed look is not repeated before the interval has passed; **Refresh** asks again. The running program, Undo and a file already downloaded do not call GitHub.

Choose **Review** on a file. The install records the repository number, the release number, the file number, the tag, the target and the file's check code. Before the download, Bilibi reads that release from GitHub again and refuses when any of those differ. It then downloads the file through the shared outgoing calls, follows only GitHub's own file hosts, and drops the token when the host changes. The download address is not stored. The file's check code is compared before the archive is opened. The files inside are checked after they are copied, the same way as an upload. The job result keeps the GitHub identity. Undo does not call GitHub.

Uploading a saved release archive stays available when GitHub cannot be reached.

### What happens on the target

1. Review an exact GitHub asset or a queued upload. GitHub review binds repository, release and asset IDs, tag, target and archive SHA-256. It checks the selected identity again at commit.
2. The server's supervised `run_downloads` loop downloads queued GitHub installs through `base/integration` as the reauthenticated requester. It rechecks GitHub, streams into `incoming/job-<id>/`, checks the approved digest before unpacking, and wakes the installer. Cancel stops a download within about two seconds; failures finish before any pause.
3. The installer takes `incoming/job-<id>/` or `incoming/upload-<n>/`, checks checksums, processor, system libraries and disk space, refuses the program already in service, then copies and checks again. For GitHub, it compares the program with the downloader's recorded SHA-256 before executing `status`. A downloaded folder is removed after the attempt; an upload stays for retry unless installed, corrupt or discarded.

Each failed check is a plain sentence that says what to do next: "This release was built for Linux ARM 64-bit computers, and this server is Linux x86-64. Build it locally for the same kind of server."

`incoming/` is the one folder the program (user `bilibi`) and the installer (`bilibi-build`) both write: `scripts/install-server.sh create` and `installer` make it (`2770 bilibi:bilibi-build`), and the service unit allows the program to write there (the path has the `-` prefix, so an instance that has no `incoming/` yet still starts). A program run without `BILIBI_INSTANCE_DIR` cannot receive a release, each time with a sentence that says so.

### Release controls

The **Releases** tab uses `release_files.rs` within `web.rs`. Each GitHub row names its asset and full archive SHA-256, and marks the installed asset. Installed facts show the local release number separately from repository/release/asset IDs, target, tag, setup commit, archive SHA-256 and program SHA-256. `status.installed_asset` and `releases.installed_asset` return the same evidence, recovered from successful install history and the current release folder. Missing older evidence is null, never inferred from a tag. Undo returns the identity of the restored release.

Upload and queued files live inside **Upload a release file**. Uploads require manage permission. Saving or accepting GitHub settings requires a fresh 2FA code. The repository number comes from GitHub, never from a typed guess. Opening the tab uses the cached list; **Refresh**, expiry or an install review asks GitHub. Job polling does not.

### Staging and Production

Use separate authenticated connections and two separate approvals to install the same asset on Staging, then Production.

1. Read each server's `status`; identify it by canonical origin and role. A label alone is insufficient.
2. Review and install the exact GitHub asset on Staging with the owner's fresh 2FA. Wait for the durable job to finish and verify `installed_asset.github`.
3. Check relevant business workflows. Retain the Staging instance, job, exact installed asset, UTC check time, named checks and outcome. Keep failed results and stop promotion if anything fails; HTTP health alone is insufficient.
4. Re-read Production status and releases immediately before planning. Submit the same asset with a `staging` report, review the recorded checks, backup and pause, and obtain a separate approval and fresh 2FA. A Production plan must not be precommitted inside a long Staging plan.
5. Wait for Production completion and compare the full installed identity. Use cancel before the pause, `switch: ask` and separately reviewed `go` for a quiet window, or a separately reviewed Undo when needed.

The optional report has this shape:

```json
{
  "staging": {
    "instance": {"origin": "https://staging.example.com", "label": "Staging", "role": "staging"},
    "asset": "<the complete installed_asset.github object from Staging status>",
    "job": 12,
    "checked_at": "2026-10-20T12:00:00Z",
    "outcome": "passed",
    "checks": [{"name": "Create and post a purchase order", "outcome": "passed"}]
  }
}
```

Replace `asset` with the object, including repository/release/asset IDs, tag, target and archive SHA-256. Include program SHA-256 and `setup_commit` when recorded; omit missing values. The review and job retain the report. Failed or incomplete checks refuse promotion. Reports are supplied by the agent, not signed or independently verified by Production. Manual installation does not require a report.

The review describes how the selected file relates to the report:

| Comparison | Review wording |
| --- | --- |
| Same repository/release/asset IDs, tag, target and archive SHA-256 | Same file as tested on Staging |
| Same repository, tag and setup commit, with a different target | Same setup as tested on Staging, built for a different computer type |
| Same repository and tag, but the setup commit of the selected release or of the report is unavailable | Could not check whether this is the setup tested on Staging |
| Anything else from GitHub | Not the setup tested on Staging |
| An uploaded file | An uploaded file cannot be compared with the Staging report |

For this comparison, Production reads the selected tag's commit from GitHub when a report is supplied. An unavailable commit gets the "could not check" wording, never "different setup". **The comparison informs the owner and never refuses installation.** The installer retains the report without enforcing its program checksum or setup commit. Different targets contain different bytes; tags and local release numbers never establish an equal asset. Never give the agent an authenticator seed or GitHub token.

### Upgrade existing servers

Finish or cancel all active installs using instance sources before applying `base-software-004-github-and-upload`. The migration refuses while any remains, including a job paused for approval or recovery, and including the install that delivers this upgrade: deliver the upgrade itself as a GitHub asset or an uploaded archive, never from an instance source. Keep the old installer until such a job finishes. The migration removes the retired settings and prevents new active installs from any source except GitHub and upload. Completed jobs, audit evidence, release folders and Undo remain intact. Update the installer alongside the app.

## Caveats

- An unpinned program cannot prove its exact Domain versions or CI pair. Pin before building locally.
- The documented local release folder has no `release.json`; an uploaded file's review says its change list is known only after the installer reads the program. The completed job records its pinned composition and changes.
- Live uploads are limited to 64 MB. GitHub downloads stream with bounded archive checks.

## Where to go next

- [Local builds and server installation](../../../docs/server-install.md).
- [Repeatable fresh-server proof](../../../docs/server-proof/README.md).
- [Versions and CI evidence](../../../docs/versions.md).
- [Installer checks and recovery](../../installer/README.md).
- [Step-up](../step_up/README.md) and [Agent API plans](../agent_api/README.md).

Software reviews and GitHub settings use the shared `bilibi_ui::StepUp` form: an account without 2FA sees a setup link instead of the code and confirmation. Setup returns to Software; the person reviews again before confirming. Agent previews include `two_factor_enabled`, `two_factor_methods` (id, label, code length and numeric flag of each enrolled method; the card sends `method_id:code`) and an absolute `two_factor_setup_url`; the ChatGPT card uses these to show the same setup-first flow through the host’s `ui/open-link` request. Cancellation needs no 2FA.
