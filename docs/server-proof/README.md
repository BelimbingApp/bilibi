# Prove a fresh server

Build on your own machine, then prove installation, 2FA, Undo and automatic switch-back in a disposable systemd server.

## Prepare the lab

Use a disposable Bilibi checkout on a Linux host with `sudo -n`, `systemd-nspawn`, `machinectl`, `debootstrap`, `iptables`, Rust, Python 3 and `chrome-devtools-axi`.

- The lab owns machine `fsproof`, network `10.77.61.0/24` and its tagged firewall rules. It refuses an existing machine or interface with that name.
- Host IP forwarding must already be enabled. The script does not change it or any host service, Caddy configuration or database.
- `prod.10.77.61.2.sslip.io` and `dev.10.77.61.2.sslip.io` must resolve to the lab address. Each is also its instance's exact `APP_ORIGIN`.
- Keep generated files outside the checkout. The driver keeps the generated password, lab authenticator key and bearer in `private/`; answers are redacted.
- The browser uses its own named AXI session and profile. Certificate checking is relaxed only in that lab browser for Caddy's internal certificates.

Create the task folder:

```sh
export BILIBI_PROOF_DATA="/tmp/bilibi-server-proof-$(date +%s)"
export CHROME_DEVTOOLS_AXI_SESSION=bilibi-fresh-server-proof
mkdir -p "$BILIBI_PROOF_DATA/private"
chmod 700 "$BILIBI_PROOF_DATA/private"
openssl rand -base64 30 > "$BILIBI_PROOF_DATA/private/password"
chmod 600 "$BILIBI_PROOF_DATA/private/password"
```

Build the baseline locally, with no Domains mounted:

```sh
cargo build --release --locked -p bilibi-web -p bilibi-installer
mkdir -p "$BILIBI_PROOF_DATA/bin/platform"
cp target/release/bilibi-web "$BILIBI_PROOF_DATA/bin/platform/"
cp target/release/bilibi-installer "$BILIBI_PROOF_DATA/bin/"
docs/server-proof/lab.sh prepare
docs/server-proof/lab.sh boot
```

`prepare` bootstraps Ubuntu 26.04 with `main` and `universe`. Pass a clean debootstrap rootfs as its second argument to copy a base instead. The checkout is mounted read-only at `/src`; task material is mounted at `/data`.

## Install the fresh server

Run the product installer inside the lab:

```sh
docs/server-proof/lab.sh run '/src/scripts/install-server.sh create production --role production --site prod.10.77.61.2.sslip.io --tls internal'
docs/server-proof/lab.sh run '/src/scripts/install-server.sh release production /data/bin/platform'
docs/server-proof/lab.sh run '/src/scripts/install-server.sh create-owner production --company-name "Proof Operations" --company-code proof_operations --email owner@proof.invalid --name "Proof Owner" --password-stdin < /data/private/password'
docs/server-proof/lab.sh run '/src/scripts/install-server.sh installer /data/bin/bilibi-installer'
docs/server-proof/lab.sh run 'if command -v cargo || command -v rustc; then exit 1; fi'
docs/server-proof/drive.py login
docs/server-proof/drive.py enrol
docs/server-proof/drive.py authorize
docs/server-proof/drive.py read base.software.status
```

The driver uses the browser to enroll the lab owner's authenticator and approve an API connection. Inspect the Dashboard and Software page in that browser.

## Retire the old worker

Prove an active legacy unit is stopped, disabled and removed, while the app keeps serving:

```sh
docs/server-proof/lab.sh run '/src/docs/server-proof/legacy.sh'
```

The script installs a lab-only legacy worker unit and old sudoers contents, then runs the new `create` twice. It checks the worker process is gone, the unit and enablement link are removed, sudoers matches the product rule, and the app PID survives. The second run preserves configuration contents and the sudoers timestamp.

## Build and check Finance

Mount the chosen Domain in the local checkout and pin before building:

```sh
./bilibi add finance --kind domains --source https://github.com/BelimbingApp/bilibi-dom-finance --commit 24938fb69a4da94275ae74085ec03671a8efd51e
python3 scripts/discover-modules.py
./scripts/build-assets.sh --check
cargo metadata --format-version 1 > /dev/null
python3 scripts/discover-modules.py --pin
cargo build --release --locked -p bilibi-web
./target/release/bilibi-web status
mkdir -p "$BILIBI_PROOF_DATA/bin/finance"
cp target/release/bilibi-web .local/composition/manifest.txt .local/composition/Cargo.lock "$BILIBI_PROOF_DATA/bin/finance/"
```

Put that finished release on the lab's test instance:

```sh
docs/server-proof/lab.sh run '/src/scripts/install-server.sh create dev --role dev --site dev.10.77.61.2.sslip.io --tls internal'
docs/server-proof/lab.sh run '/src/scripts/install-server.sh release dev /data/bin/finance'
docs/server-proof/lab.sh run '/src/scripts/install-server.sh create-owner dev --company-name "Proof Operations" --company-code proof_operations --email owner@proof.invalid --name "Proof Owner" --password-stdin < /data/private/password'
LAB_ORIGIN=https://dev.10.77.61.2.sslip.io docs/server-proof/drive.py login
chrome-devtools-axi open https://dev.10.77.61.2.sslip.io/finance/payroll
```

Open Finance and use its Payroll page before packaging the release. An empty payroll list states that People is not mounted; it is not fixture data.

## Upload and install

Package the checked build locally and upload that archive on Production:

```sh
docs/server-proof/pack-setup.sh "$BILIBI_PROOF_DATA/bin/finance/bilibi-web" "$BILIBI_PROOF_DATA/proof/release.tar.gz"
docs/server-proof/drive.py upload
docs/server-proof/drive.py receive
docs/server-proof/drive.py refuse-install upload-1
docs/server-proof/drive.py review upload-1
docs/server-proof/drive.py shot install-review.png
docs/server-proof/drive.py confirm
docs/server-proof/drive.py wait 1
chrome-devtools-axi open https://prod.10.77.61.2.sslip.io/finance/payroll
docs/server-proof/drive.py shot finance-installed.png
docs/server-proof/lab.sh run '/src/docs/server-proof/rows.sh'
```

`refuse-install` checks that a commit without a code is refused. `confirm` submits the browser's reviewed plan with a fresh code from the enrolled lab authenticator. Observe the backup before migrations, the switch and verification, then use Finance's menu in Production.

The documented release folder has no `release.json`; the review warns that the change list is known only after the installer reads the program. The install result must then identify Finance and its pinned commit.

The driver works around an AXI limit: it fills the real file input through browser scripting because the native upload command's argument shape differs from the installed DevTools tool. Bilibi still streams, receives and validates the file through its ordinary page.

## Install from GitHub

Use a throwaway release in a repository you own, such as `BelimbingApp/bilibi-app`. Publish it as a pre-release with the archive attached, and delete it afterwards. GitHub allows only 60 unauthenticated calls an hour per address, and the lab shares the host's address, so refresh sparingly.

```sh
docs/server-proof/pack-setup.sh target/release/bilibi-web "$BILIBI_PROOF_DATA/assets/bilibi-setup-proof1-x86_64-unknown-linux-gnu.tar.gz"
gh-axi release create proof-github-source-1 --draft --prerelease -R BelimbingApp/bilibi-app <archive>
gh-axi release edit proof-github-source-1 --draft=false --prerelease=true -R BelimbingApp/bilibi-app
```

Name the repository and accept it on Software › Releases, choose **Show pre-releases**, then drive the same reviewed install the page offers:

```sh
docs/server-proof/drive.py github-install <asset-id> wrong-digest      # refused at the plan
docs/server-proof/drive.py github-install <asset-id> wrong-repository  # refused at the plan
docs/server-proof/drive.py github-install <asset-id> refuse            # refused without a code
docs/server-proof/drive.py github-install <asset-id>                   # fresh code, then download
docs/server-proof/drive.py cancel                                      # cancel while it downloads
docs/server-proof/drive.py wait <job>
```

To make the download last long enough to cancel or interrupt, slow the lab's veth on the host for the run: `sudo -n tc qdisc add dev ve-fsproof root tbf rate 3mbit burst 64kb latency 500ms`, and `tc qdisc del` it afterwards. Restart `bilibi@production` inside the lab during the download to see an interrupted job. `fail-check.sh <in-service-release> <failing-release>` arms the failed check for the release the GitHub install will create.

## Promote the same asset

Create a Staging instance with its own database and exact origin. Put one additional bootstrap release there so its next local release number differs from Production's. Configure the same GitHub repository independently on each instance.

1. Install one GitHub asset on Staging with its own fresh 2FA. Save its completed `status.installed_asset`, including the local release number and complete `github` object.
2. Use the installed app in the browser and check a business workflow. Record the named checks, outcome, Staging origin, job and UTC time. A failed check stops promotion.
3. Make a separate Production operation file outside the checkout. Use the same GitHub IDs, target, tag and archive digest, with `staging` shaped as documented in [Software](../../apps/base/software/README.md#staging-and-production).
4. Use `drive.py plan-only <file>` to inspect that review, then `drive.py plan <file> refuse-first` for a separately approved install with fresh 2FA. Check that the Staging result survives in the completed job.
5. Compare both installed GitHub objects after restart. Archive and program SHA-256 and GitHub IDs must match; local release numbers must differ.
6. Use `drive.py plan-only <file>` with the matching report, a report for another target of the same setup tag and commit, a report for a different setup, and a report for the same tag and another target that omits `setup_commit`. Check the four GitHub relation messages documented in Software; all plans remain reviewable. Check a failed Staging outcome with `drive.py plan-only <file> refused`. Confirm that planning left Production unchanged. Then upload the saved archive through the folded file section, check that its review says an uploaded file cannot be compared with the Staging report, and complete an upload install.
7. Request the retired listing, archive and export addresses: `/transport/releases`, `/transport/releases/1` and `/system/software/releases/1/download`. Each must answer 404. The old page bookmark may redirect to the Releases tab.

Do not put authenticator seeds, passwords, API credentials or plan codes in operation files or evidence. The driver stores its lab credentials privately and redacts plan answers.

## Undo and fail a check

Undo the completed install and confirm the baseline returns:

```sh
docs/server-proof/drive.py undo
docs/server-proof/drive.py wait 2
chrome-devtools-axi open https://prod.10.77.61.2.sslip.io/system/software
docs/server-proof/lab.sh run '/src/docs/server-proof/rows.sh'
```

The result must say release 1, restored backup, no installed Domains and no Finance migrations. The review and result record what the restore loses.

Make the next release answer on the wrong port, then install the same tested file again:

```sh
docs/server-proof/lab.sh run '/src/docs/server-proof/fail-check.sh'
docs/server-proof/drive.py upload
docs/server-proof/drive.py receive
docs/server-proof/drive.py install upload-1
docs/server-proof/drive.py wait 3
docs/server-proof/lab.sh run '/src/docs/server-proof/rows.sh'
chrome-devtools-axi open https://prod.10.77.61.2.sslip.io/system/software
docs/server-proof/drive.py shot automatic-switch-back.png
```

The hook affects only release 3. Its process starts, but the real verify step times out. The installer must restore the backup, return to healthy release 1 and record the failed check and successful switch-back. Inspect the page and audit evidence, rather than treating the expected failed job as a failed proof.

## Remove the lab

Stop the owned launcher by its recorded PID, remove only the lab's firewall rules and delete its rootfs:

```sh
chrome-devtools-axi stop
docs/server-proof/lab.sh stop
docs/server-proof/lab.sh destroy
rm -r -- "$BILIBI_PROOF_DATA/private" "$BILIBI_PROOF_DATA/chrome"
```

Before committing Platform work, move the mounted Finance checkout and `.local/composition` into task material, run discovery again and restore the Platform `Cargo.lock`. Keep the proof's pin and evidence outside the checkout; never commit a mounted composition.

## Where to go next

- [Server installation](../server-install.md).
- [Software contracts](../../apps/base/software/README.md).
- [Installer recovery](../../apps/installer/README.md).
