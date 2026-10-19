# Prove a fresh server

Build on your own machine, then prove installation, 2FA, Undo and automatic switch-back in a disposable systemd server.

## Recorded run

[The evidence summary](evidence-2026-10-19.md) records the commits, job results, screenshots and cleanup from the 2026-10-19 run.

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
git clone -- https://github.com/BelimbingApp/bilibi-dom-finance apps/domains/finance
git -C apps/domains/finance checkout --detach 24938fb69a4da94275ae74085ec03671a8efd51e
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

Open Finance and use its Payroll page before downloading the release. An empty payroll list states that People is not mounted; it is not fixture data.

## Upload and install

Download through Dev's Release files link and upload the resulting file on Production:

```sh
LAB_ORIGIN=https://dev.10.77.61.2.sslip.io docs/server-proof/drive.py download
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

The driver works around two AXI limits: it retains a downloaded Blob and exports bounded chunks because DevTools evicts large response bodies; it fills the real file input through browser scripting because the native upload command's argument shape differs from the installed DevTools tool. Bilibi still streams, receives and validates the file through its ordinary page.

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
