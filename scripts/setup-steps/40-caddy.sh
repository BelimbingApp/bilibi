#!/usr/bin/env bash
# Caddy, with its admin API on localhost:2019 (scripts/local-https.sh registers the route there).
set -euo pipefail
# shellcheck source=scripts/lib/install.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/install.sh"

if apt_package_installed caddy || command -v caddy >/dev/null; then
    present caddy
else
    apt_update
    if ! apt_candidate caddy; then
        apt_repo caddy-stable https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
            'deb [signed-by=@KEYRING@] https://dl.cloudsmith.io/public/caddy/stable/deb/debian any-version main'
    fi
    apt_install caddy || true
    installed caddy
fi

# scripts/local-https.sh adds Bilibi's route to a running Caddy through the admin API. It
# needs an HTTPS server on :443 with the TLS app present; the packaged Caddyfile
# serves a demo page on :80 only. Replace the packaged file (keeping a copy), never a
# file the user edited.
caddyfile=/etc/caddy/Caddyfile
marker='# Managed by Bilibi scripts/setup.sh'
config_changed=
if [[ ! -f $caddyfile ]] || grep -q "Set this path to your site's directory" "$caddyfile"; then
    need_root
    [[ -f $caddyfile ]] && log "${SUDO[@]}" cp -n "$caddyfile" "$caddyfile.dist"
    as_root install -d /etc/caddy
    printf '%s\n{\n\tauto_https disable_redirects\n}\n:443 {\n\ttls internal\n}\n' "$marker" |
        "${SUDO[@]}" tee "$caddyfile" >/dev/null
    config_changed=1
elif ! grep -q "$marker" "$caddyfile" && ! grep -q ':443' "$caddyfile"; then
    note "$caddyfile is yours and has no :443 server, so local HTTPS will not work; ./bilibi start falls back to HTTP."
fi

admin_up() { curl -fsS --max-time 2 http://localhost:2019/config/ >/dev/null 2>&1; }
if admin_up && [[ -z $config_changed ]]; then exit 0; fi

if [[ -d /run/systemd/system ]] && command -v systemctl >/dev/null; then
    as_root systemctl enable caddy
    as_root systemctl restart caddy
    for _ in 1 2 3 4 5 6 7 8 9 10; do admin_up && exit 0; sleep 1; done
    fail caddy 'the service started but its admin API does not answer on localhost:2019' \
        'check "systemctl status caddy" and that /etc/caddy/Caddyfile does not disable the admin API'
fi
if admin_up; then
    note "Caddy is running with an old config (no systemd here). Restart it with: caddy run --config $caddyfile"
elif is_wsl; then
    note "Caddy is not running (this WSL has no systemd). Enable systemd once (\"[boot]\" and \"systemd=true\" in /etc/wsl.conf, then \"wsl --shutdown\" from Windows) and rerun ./scripts/setup.sh, or run in another terminal: caddy run --config $caddyfile"
else
    note "Caddy is not running (no systemd here). Start it in another terminal with: caddy run --config $caddyfile"
fi
