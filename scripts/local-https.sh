#!/usr/bin/env bash
# Register the owner's HTTPS route and certificate; the API rejects other checkouts.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
host=${BILIBI_DEV_HOST:-bilibi.localhost}
port=${BILIBI_DEV_PORT:-4300}
for tool in mkcert openssl python3 ss flock; do
    command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool. Run ./scripts/setup.sh" >&2; exit 1; }
done
if [[ ! "$port" =~ ^[0-9]{4,5}$ ]] || (( 10#$port < 1024 || 10#$port > 65535 )); then
    echo 'BILIBI_DEV_PORT must be 1024..65535.' >&2
    exit 1
fi
mkdir -p "$root/.local/https"
exec 8>"/tmp/bilibi-local-https-$(id -u).lock"
flock 8
certificate="$root/.local/https/$host.pem"
key="$root/.local/https/$host-key.pem"
# Ownership is checked before the API helper creates or renews these files.
port=$(python3 "$root/scripts/local-https-api.py" \
    "$host" "$port" "$certificate" "$key")
printf '%s\n%s\n' "$host" "$port" > "$root/.local/https/instance"
echo "Local HTTPS configured through Caddy's admin API: https://$host → 127.0.0.1:$port"
