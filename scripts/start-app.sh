#!/usr/bin/env bash
# Local launcher plumbing, with no Bilimbi module counterpart.
# One development entrypoint; no daemon management or automatic installation.
# Install prerequisites first with ./scripts/setup.sh.
set -euo pipefail
# Supply the initial local role to the install seed. A saved role takes precedence.
export BILIBI_ENV=${BILIBI_ENV-development}
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"
# A full local copy needs these keys. When a variable is already set, leave it
# and its file alone. Otherwise read the file, or create it once: 64 hex
# characters from 32 random bytes, mode 0600, under gitignored .local/dev/.
# Never replace an existing key file. A new settings key cannot open secret
# settings or authenticator keys sealed with the old one.
ensure_dev_key() {
    local var=$1 file=$2 value tmp
    if [[ -n ${!var:-} ]]; then
        return 0
    fi
    mkdir -p -- "$root/.local/dev"
    chmod 700 "$root/.local/dev"
    if [[ -e $file && ! -f $file ]]; then
        echo "$file exists and is not a regular file. It was left unchanged." >&2
        exit 1
    fi
    if [[ -f $file ]]; then
        value=$(tr -d '[:space:]' < "$file")
        chmod 600 "$file" || true
    else
        value=$(python3 -c 'import secrets; print(secrets.token_hex(32), end="")')
        tmp=$(mktemp "$root/.local/dev/.new-key.XXXXXX")
        chmod 600 "$tmp"
        if ! printf '%s\n' "$value" > "$tmp"; then
            rm -f -- "$tmp"
            exit 1
        fi
        # ln fails if the file appeared; keep that file and do not rewrite it.
        if ln -- "$tmp" "$file" 2>/dev/null; then
            rm -f -- "$tmp"
        else
            rm -f -- "$tmp"
            if [[ ! -f $file ]]; then
                echo "Could not create $file." >&2
                exit 1
            fi
            value=$(tr -d '[:space:]' < "$file")
        fi
    fi
    if [[ ! $value =~ ^[0-9a-fA-F]{64}$ ]]; then
        echo "$file must contain 64 hex characters (32 random bytes). It was left unchanged." >&2
        exit 1
    fi
    printf -v "$var" '%s' "$value"
    # shellcheck disable=SC2163 # exports the variable named by $var
    export "$var"
}
ensure_dev_key BILIBI_SETTINGS_KEY "$root/.local/dev/settings.key"
ensure_dev_key BILIBI_USER_SIGNING_KEY "$root/.local/dev/user-signing.key"
ensure_dev_key BILIBI_MEDIA_SIGNING_KEY "$root/.local/dev/media-signing.key"
# Acquire the checkout owner and bring up status before any build can fail.
if [[ ${1:-} != --owned ]]; then
    exec python3 scripts/dev-process.py watch "$root/scripts/start-app.sh" --owned
fi
for tool in rustc cargo psql pg_isready python3 curl ss flock; do
    command -v "$tool" >/dev/null || { echo "Missing prerequisite: $tool. Run ./scripts/setup.sh" >&2; exit 1; }
done
cargo watch --version >/dev/null 2>&1 || { echo 'Missing prerequisite: cargo-watch. Run ./scripts/setup.sh' >&2; exit 1; }
echo "$(rustc --version); $(cargo --version)"
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
host=${BILIBI_DEV_HOST:-bilibi.localhost}
# Plan before calling local-https.sh: other checkouts never register with Caddy.
# The registrar rechecks ownership under its lock in case two first launches race.
plan=$(python3 "$root/scripts/local-https-api.py" --plan "$host" "${BILIBI_DEV_PORT:-4300}")
readarray -t origin_plan <<< "$plan"
scheme=${origin_plan[0]}
instance=("${origin_plan[1]}" "${origin_plan[2]}")
if [[ $scheme == https ]]; then
    if "$root/scripts/local-https.sh"; then
        readarray -t instance < "$root/.local/https/instance"
    else
        echo 'Local HTTPS is unavailable for this checkout. Continuing with its loopback HTTP address.' >&2
        plan=$(python3 "$root/scripts/local-https-api.py" --http "$host" "${BILIBI_DEV_PORT:-4300}")
        readarray -t origin_plan <<< "$plan"
        scheme=${origin_plan[0]}
        instance=("${origin_plan[1]}" "${origin_plan[2]}")
    fi
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
# Colour the address on a terminal without NO_COLOR, as the app's own "listening" line does.
if [[ -t 1 && -z ${NO_COLOR-} ]]; then
    highlight() { printf '\e[1;32m%s\e[4m%s\e[0m' "$1" "$2"; }
else
    highlight() { printf '%s%s' "$1" "$2"; }
fi
echo "$(highlight 'Open ' "$APP_ORIGIN") — sign in as ai@agent.my / bilibi-dev (the platform operator's administrator)."
# Open the sign-in page in the browser once the app answers: on WSL through
# Windows, on a Linux desktop through xdg-open. Without either, the printed
# address is enough. BILIBI_OPEN_BROWSER=0 turns this off. The waiter stops
# when this launcher does; exec below keeps the launcher's process ID.
opener=()
if [[ ${BILIBI_OPEN_BROWSER:-1} != 0 ]] && command -v curl >/dev/null; then
    if [[ -n ${WSL_DISTRO_NAME-} ]] && command -v wslview >/dev/null; then
        opener=(wslview)
    elif [[ -n ${WSL_DISTRO_NAME-} && -x /mnt/c/Windows/explorer.exe ]]; then
        opener=(/mnt/c/Windows/explorer.exe)
    elif [[ -n ${DISPLAY-}${WAYLAND_DISPLAY-} ]] && command -v xdg-open >/dev/null; then
        opener=(xdg-open)
    fi
fi
if (( ${#opener[@]} )); then
    echo 'Your browser opens the sign-in page when the app is ready. BILIBI_OPEN_BROWSER=0 turns this off.'
    launcher=$$
    (
        # -f: Caddy answers 502 until the app is up.
        until curl -skf -o /dev/null --max-time 2 "$APP_ORIGIN/login"; do
            kill -0 "$launcher" 2>/dev/null || exit 0
            sleep 2
        done
        exec "${opener[@]}" "$APP_ORIGIN/login"
    ) >/dev/null 2>&1 &
fi
# The optional local asset build writes ignored app.css (and npm may touch
# node_modules, Python its __pycache__) under apps/ and scripts/;
# watching them would make every rebuild trigger the next one, so they are
# ignored here. .gitignore is not used: it ignores the mounted Domain and
# Extension checkouts under apps/, whose edits must rebuild too. (cargo-watch
# only ever read it when .git is a directory, so a git worktree already ran
# with these ignores alone.)
# Keep the app owned by the launcher, outside cargo-watch's rebuild process
# group. The owner stops only its own child on replacement or Ctrl-C.
# Ignore prose, while retaining all Rust, SQL, templates, assets and include files.
exec cargo watch --no-vcs-ignores --watch apps --watch Cargo.toml --watch Cargo.lock --watch scripts \
    --ignore 'apps/base/ui/assets/app.css' --ignore 'apps/base/ui/assets/css/.mounted-*.css' \
    --ignore '**/node_modules/**' --ignore '**/__pycache__/**' --ignore '*.md' --shell './scripts/dev-run.sh'
