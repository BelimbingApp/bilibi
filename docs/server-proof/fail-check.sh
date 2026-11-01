#!/usr/bin/env bash
# Disposable deployment proof.
# Lab-only fault injection: the failing release (3 unless given) starts
# normally on the wrong port, so the installer's verify step fails; the
# release in service now (1 unless given) keeps its configured port.
# Usage: fail-check.sh [in-service-release [failing-release]]
set -euo pipefail
CURRENT=${1:-1}
FAILING=${2:-3}
[[ $(cat /run/systemd/container) == systemd-nspawn ]]
[[ $(readlink /opt/bilibi/production/releases/current) == "$CURRENT" ]]
[[ ! -e /opt/bilibi/production/releases/$FAILING ]]
install -d /usr/local/lib /etc/systemd/system/bilibi@production.service.d
cat > /usr/local/lib/bilibi-proof-launch <<LAUNCH
#!/usr/bin/env bash
set -euo pipefail
if [[ \$(readlink /opt/bilibi/production/releases/current) == $FAILING ]]; then
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
echo "Fault armed: only release $FAILING will use the wrong port; health verification must fail."
