#!/usr/bin/env bash
# Disposable deployment proof; no Bilimbi counterpart.
set -euo pipefail
[[ $(cat /run/systemd/container) == systemd-nspawn ]]
S=/src/scripts/install-server.sh
app_pid=$(systemctl show -p MainPID --value bilibi@production)
[[ $app_pid -gt 0 ]]
cat > /etc/systemd/system/bilibi-worker@.service <<'UNIT'
[Unit]
Description=Legacy worker retirement probe
[Service]
User=bilibi-build
ExecStart=/usr/bin/sleep infinity
[Install]
WantedBy=multi-user.target
UNIT
printf 'bilibi-build ALL=(root) NOPASSWD: /usr/bin/true\n' >/etc/sudoers.d/bilibi-build
chmod 440 /etc/sudoers.d/bilibi-build
systemctl daemon-reload
systemctl enable --now bilibi-worker@production
old_pid=$(systemctl show -p MainPID --value bilibi-worker@production)
[[ $old_pid -gt 0 ]]
echo 'Legacy unit enabled and active before upgrade.'
$S create production --role production --site prod.10.77.61.2.sslip.io --tls internal
[[ ! -e /proc/$old_pid ]]
[[ ! -e /etc/systemd/system/bilibi-worker@.service ]]
[[ ! -e /etc/systemd/system/multi-user.target.wants/bilibi-worker@production.service ]]
cmp /src/scripts/server/sudoers /etc/sudoers.d/bilibi-build
visudo -cf /etc/sudoers.d/bilibi-build
hashes() { sha256sum /opt/bilibi/production/env /etc/sudoers.d/bilibi-build /etc/systemd/system/bilibi@.service /etc/systemd/system/bilibi-installer@.service; }
hashes >/tmp/before-hashes
stat -c '%Y' /etc/sudoers.d/bilibi-build >/tmp/before-sudoers-time
$S create production --role production --site prod.10.77.61.2.sslip.io --tls internal
hashes >/tmp/after-hashes
stat -c '%Y' /etc/sudoers.d/bilibi-build >/tmp/after-sudoers-time
cmp /tmp/before-hashes /tmp/after-hashes
cmp /tmp/before-sudoers-time /tmp/after-sudoers-time
[[ $(systemctl show -p MainPID --value bilibi@production) == "$app_pid" ]]
echo 'PASS: old process stopped, unit disabled and removed, sudoers refreshed; second create preserves env, unit and sudoers contents and sudoers timestamp.'
echo 'PASS: running app PID preserved across both create calls.'
