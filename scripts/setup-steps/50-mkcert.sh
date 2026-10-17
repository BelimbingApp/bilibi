#!/usr/bin/env bash
# mkcert, libnss3-tools and the local CA. On WSL2 the CA is also trusted by Windows,
# so a Windows browser accepts https://bilibi.localhost.
set -euo pipefail
# shellcheck source=scripts/lib/install.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/install.sh"

if apt_install mkcert libnss3-tools; then
    installed 'mkcert'
else
    present mkcert
fi

# An existing CAROOT is respected: mkcert reads it itself.
ca_root=$(mkcert -CAROOT)
ca_pem=$ca_root/rootCA.pem
trusted_here() {
    local f
    for f in /usr/local/share/ca-certificates/mkcert_development_CA_*.crt; do
        [[ -f $f ]] && cmp -s "$f" "$ca_pem" && return 0
    done
    return 1
}
if [[ -f $ca_pem ]] && trusted_here; then
    present 'mkcert CA'
else
    # mkcert escalates with sudo itself for the system store; get the credentials first.
    [[ $(id -u) -eq 0 ]] || need_root
    log mkcert -install
    installed 'mkcert CA'
fi

if is_wsl && command -v powershell.exe >/dev/null && command -v wslpath >/dev/null; then
    thumb=$(openssl x509 -in "$ca_pem" -noout -fingerprint -sha1 | cut -d= -f2 | tr -d ':')
    found=$(powershell.exe -NoProfile -Command "(Get-ChildItem Cert:\\CurrentUser\\Root | Where-Object { \$_.Thumbprint -eq '$thumb' }).Count" 2>>"$SETUP_LOG" | tr -d '\r' || true)
    if [[ ${found:-0} -ge 1 ]]; then
        present 'mkcert CA in Windows'
    else
        win_pem=$(wslpath -w "$ca_pem")
        if log powershell.exe -NoProfile -Command "Import-Certificate -FilePath '$win_pem' -CertStoreLocation Cert:\\CurrentUser\\Root"; then
            installed 'mkcert CA in Windows'
        else
            note "Windows browsers do not trust the local CA yet. Import $win_pem into Trusted Root Certification Authorities."
        fi
    fi
fi
