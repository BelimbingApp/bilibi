#!/usr/bin/env bash
# Register this checkout's HTTPS route and certificate for its own host; the
# API rejects a checkout that does not own the host. start-app.sh resolves
# BILIBI_DEV_HOST first (scripts/dev-env.py).
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
host=${BILIBI_DEV_HOST:-bilibi.localhost}
port=${BILIBI_DEV_PORT:-4300}
for tool in mkcert openssl python3 ss flock; do
    command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool. Run ./bilibi setup" >&2; exit 1; }
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
# It prints the backend port, then Caddy's HTTPS port (443 unless configured).
registration=$(python3 "$root/scripts/local-https-api.py" \
    "$host" "$port" "$certificate" "$key")
readarray -t registered <<< "$registration"
[[ ${#registered[@]} == 2 ]] || exit 1
port=${registered[0]}
https_port=${registered[1]}
printf '%s\n%s\n%s\n' "$host" "$port" "$https_port" > "$root/.local/https/instance"
address="https://$host"
[[ $https_port == 443 ]] || address+=":$https_port"
echo "Local HTTPS configured through Caddy's admin API: $address → 127.0.0.1:$port"
