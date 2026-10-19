#!/usr/bin/env bash
# Disposable deployment proof; no Bilimbi counterpart.
# Lab-only fault injection: release 3 starts normally on the wrong port,
# so the installer's verify step fails; release 1 keeps its configured port.
set -euo pipefail
[[ $(cat /run/systemd/container) == systemd-nspawn ]]
[[ $(readlink /opt/bilibi/production/releases/current) == 1 ]]
[[ ! -e /opt/bilibi/production/releases/3 ]]
install -d /usr/local/lib /etc/systemd/system/bilibi@production.service.d
cat > /usr/local/lib/bilibi-proof-launch <<'LAUNCH'
#!/usr/bin/env bash
set -euo pipefail
if [[ $(readlink /opt/bilibi/production/releases/current) == 3 ]]; then
    export BIND_ADDR=127.0.0.1:4399
fi
exec /opt/bilibi/production/current/bilibi-web run
LAUNCH
chmod 755 /usr/local/lib/bilibi-proof-launch
cat > /etc/systemd/system/bilibi@production.service.d/proof.conf <<'UNIT'
[Service]
ExecStart=
ExecStart=/usr/local/lib/bilibi-proof-launch
UNIT
systemctl daemon-reload
echo 'Fault armed: only release 3 will use the wrong port; health verification must fail.'
