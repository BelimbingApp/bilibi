#!/usr/bin/env bash
# One development entrypoint; no daemon management or automatic installation.
# Install prerequisites first with ./scripts/setup.sh.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"
for tool in rustc cargo psql pg_isready caddy mkcert python3 openssl curl ss flock; do
    command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool. Run ./scripts/setup.sh" >&2; exit 1; }
done
cargo watch --version >/dev/null 2>&1 || { echo 'Missing prerequisite: cargo-watch. Run ./scripts/setup.sh' >&2; exit 1; }
echo "$(rustc --version); $(cargo --version)"
python3 scripts/discover-modules.py
./scripts/build-assets.sh
if [[ -z ${DATABASE_URL:-} ]]; then
    pg_bin=${PG18_BIN:-/usr/lib/postgresql/18/bin}
    [[ -x "$pg_bin/initdb" && -x "$pg_bin/pg_ctl" ]] || {
        echo 'Missing prerequisite: PostgreSQL 18 binaries. Run ./scripts/setup.sh (or set DATABASE_URL).' >&2
        exit 1
    }
    pg_root="$root/.local/postgres18"
    pg_data="$pg_root/data"
    mkdir -p "$pg_root"
    mkdir -p "$pg_root/socket"
    chmod 700 "$pg_root"
    if [[ ! -s "$pg_data/PG_VERSION" ]]; then
        mkdir -p "$pg_data"
        "$pg_bin/initdb" -D "$pg_data" --username=bilibi --pwfile=<(printf 'bilibi\n') \
            --auth-local=trust --auth-host=scram-sha-256
    elif [[ $(<"$pg_data/PG_VERSION") != 18 ]]; then
        echo "The private PostgreSQL data directory is not version 18: $pg_data" >&2
        exit 1
    fi
    pg_port=5438
    if ! "$pg_bin/pg_ctl" -D "$pg_data" status >/dev/null 2>&1; then
        if [[ -n $(ss -H -ltn "sport = :$pg_port") ]]; then
            echo "Port $pg_port is occupied; stop that listener or set DATABASE_URL." >&2
            exit 1
        fi
        "$pg_bin/pg_ctl" -D "$pg_data" -o "-h 127.0.0.1 -p $pg_port -k $pg_root/socket" \
            -l "$pg_root/server.log" -w start
    fi
    pg_admin_url="postgres://bilibi:bilibi@127.0.0.1:${pg_port}/postgres"
    if [[ $(psql "$pg_admin_url" -Atqc "SELECT 1 FROM pg_database WHERE datname = 'bilibi'") != 1 ]]; then
        psql "$pg_admin_url" -v ON_ERROR_STOP=1 -c 'CREATE DATABASE bilibi'
    fi
    export DATABASE_URL="postgres://bilibi:bilibi@127.0.0.1:${pg_port}/bilibi"
fi
pg_isready -d "$DATABASE_URL" >/dev/null || { echo 'PostgreSQL is not ready for DATABASE_URL.' >&2; exit 1; }
# A supplied URL must point at an existing database.
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -Atc 'select 1' >/dev/null
started=$(date +%s)
cargo build -p bilibi-web --timings
echo "Initial Rust build took $(($(date +%s)-started))s. Timing report: target/cargo-timings/cargo-timing.html"
./target/debug/bilibi-web migrate
./target/debug/bilibi-web seed --at-startup
if "$root/scripts/local-https.sh"; then
    readarray -t instance < "$root/.local/https/instance"
    scheme=https
else
    echo 'Caddy did not accept Bilibi HTTPS; check its admin API and existing certificates, then rerun. Continuing with the local HTTP backend.' >&2
    instance=(bilibi.localhost "${BILIBI_DEV_PORT:-4300}")
    while [[ -n $(ss -H -ltn "sport = :${instance[1]}") ]]; do
        ((instance[1] += 1))
        ((instance[1] <= 65535)) || { echo 'No free local backend port is available.' >&2; exit 1; }
    done
    scheme=http
fi
if [[ $scheme == https ]]; then
    export APP_ORIGIN="https://${instance[0]}"
else
    export APP_ORIGIN="http://${instance[0]}:${instance[1]}"
fi
export BIND_ADDR="127.0.0.1:${instance[1]}"
if [[ -n $(ss -H -ltn "sport = :${instance[1]}") ]]; then
    echo "This checkout already has a listener at $APP_ORIGIN. Stop its dev command before starting another." >&2
    exit 1
fi
# Colour only on a terminal without NO_COLOR, matching the app's own "listening" line.
if [[ -t 1 && -z ${NO_COLOR-} ]]; then
    printf '\e[1;32mOpen \e[4m%s\e[0m — sign in as ai@agent.my / bilibi-dev (the platform operator'"'"'s administrator).\n' "$APP_ORIGIN"
else
    echo "Open $APP_ORIGIN — sign in as ai@agent.my / bilibi-dev (the platform operator's administrator)."
fi
# dev-run.sh explains each restart from the state the previous run left here.
rm -f "$root/.local/dev-run.state"
echo 'Cargo output stays visible. Edit Rust/templates/assets to rebuild; Ctrl-C stops this app, leaving shared Caddy running.'
# The asset build rewrites app.css, writes a scratch css/.mounted-<pid>.css (and
# npm may touch node_modules) under apps/; watching them would make every rebuild
# trigger the next one. cargo-watch reads .gitignore only when .git is a
# directory, so in a git worktree these ignores are the only ones it applies.
exec cargo watch --watch apps --watch Cargo.toml --watch Cargo.lock --watch scripts \
    --ignore 'apps/base/ui/assets/app.css' --ignore 'apps/base/ui/assets/css/.mounted-*.css' \
    --ignore '**/node_modules/**' --shell './scripts/dev-run.sh'
