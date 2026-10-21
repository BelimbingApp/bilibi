# shellcheck shell=bash
# Local development prerequisites; no Bilimbi module counterpart.
# shellcheck source=scripts/lib/versions.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/versions.sh"

activate_cargo() {
    # shellcheck disable=SC1091
    if [[ -f $HOME/.cargo/env ]]; then source "$HOME/.cargo/env"; fi
}

development_prerequisites() {
    local tool current minimum pg_bin
    for tool in git python3 curl ss flock openssl cc pkg-config rustc cargo psql pg_isready node npm; do
        command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool" >&2; return 1; }
    done
    python3 -c 'import tomllib' 2>/dev/null || { echo 'Python 3.11+ is required. See README.md (supported environment).' >&2; return 1; }
    # shellcheck disable=SC2154 # root is set by the sourcing script
    minimum=$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$root/Cargo.toml" | head -n1)
    current=$(rustc --version | awk '{print $2}') || return 1
    [[ $(printf '%s\n%s\n' "$minimum" "$current" | sort -V | head -n1) == "$minimum" ]] || { echo "Rust $minimum+ is required." >&2; return 1; }
    for tool in 'cargo --version' 'cargo clippy --version' 'cargo fmt --version' 'cargo watch --version' \
        'git --version' 'curl --version' 'ss -V' 'flock --version' 'openssl version' \
        'cc --version' 'psql --version' 'pg_isready --version' 'npm --version'; do
        # Each word here is a fixed command, never request input.
        $tool >/dev/null 2>&1 || { echo "Prerequisite failed: $tool" >&2; return 1; }
    done
    pkg-config --exists openssl || { echo 'OpenSSL development libraries are required.' >&2; return 1; }
    node -e "process.exit(Number(process.versions.node.split('.')[0]) < $NODE_MAJOR ? 1 : 0)" || { echo "Node.js $NODE_MAJOR+ is required." >&2; return 1; }
    if [[ -z ${DATABASE_URL:-} ]]; then
        pg_bin=${PG18_BIN:-/usr/lib/postgresql/18/bin}
        "$pg_bin/initdb" --version >/dev/null 2>&1 && "$pg_bin/pg_ctl" --version >/dev/null 2>&1 || {
            echo "PostgreSQL $POSTGRESQL_MAJOR binaries are required at $pg_bin (or set DATABASE_URL)." >&2; return 1;
        }
    fi
}
