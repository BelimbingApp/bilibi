#!/usr/bin/env bash
# Set up a Bilibi server: one or more instances (dev, staging, production) under
# /opt/bilibi, each with its own database, system service, address and release
# folders. Run as root on Ubuntu or Debian. Safe to rerun: it keeps what exists.
# The layout, and why it is this way, is in docs/server-install.md.
#
#   install-server.sh create <instance> --role dev|staging|production --site <host> [--port <n>] [--tls internal]
#   install-server.sh release <instance> <bilibi-web binary, or a folder holding it>
#   install-server.sh switch <instance> <release number>
#   install-server.sh status [<instance>]
#
# Only the layout and the release switch live here. Building a release (Dev),
# backing up before a switch, checking the new release and undoing a bad one
# belong to the Software page and its builder and installer.
set -euo pipefail
shopt -s inherit_errexit

here=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
# shellcheck source=scripts/lib/versions.sh
source "$here/lib/versions.sh"
root=/opt/bilibi
unit_name='bilibi@'
pg=$POSTGRESQL_MAJOR
log=/var/log/bilibi-install-server.log
export DEBIAN_FRONTEND=noninteractive

usage() {
    sed -n '2,/^$/p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//' >&2
    exit 2
}
fail() { printf 'install-server: %s\n' "$1" >&2; [[ -n ${2:-} ]] && printf 'Next: %s\n' "$2" >&2; exit 1; }
say() { printf '%s\n' "$*"; }
need_root() { [[ $(id -u) -eq 0 ]] || fail 'run this as root' "sudo $0 $*"; }
valid_name() { [[ $1 =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]]; }
instance_dir() { printf '%s/%s' "$root" "$1"; }
instance_exists() { [[ -f $(instance_dir "$1")/env ]]; }
# env_value <instance> <KEY>: one value from the instance's env file.
env_value() { sed -n "s/^$2=//p" "$(instance_dir "$1")/env" | head -n1; }
role_of() {
    case $(env_value "$1" BILIBI_ENV) in
        development) echo dev ;;
        staging) echo staging ;;
        *) echo production ;;
    esac
}
# These three print what exists and succeed when nothing does, so a caller's
# command substitution never trips errexit on an empty server.
instances() { [[ -d $root ]] || return 0; find "$root" -mindepth 2 -maxdepth 2 -name env -printf '%h\n' | xargs -rn1 basename | sort; }
release_numbers() { { find "$(instance_dir "$1")/releases" -mindepth 1 -maxdepth 1 -type d -printf '%f\n' 2>/dev/null || true; } | { grep -E '^[0-9]+$' || true; } | sort -n; }
current_release() { local link; link=$(readlink "$(instance_dir "$1")/current" 2>/dev/null || true); if [[ -n $link ]]; then basename "$link"; fi; }
service_state() { systemctl is-active "$unit_name$1" 2>/dev/null || true; }

# ---------------------------------------------------------------- packages
apt_installed() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q '^install ok installed$'; }
apt_candidate() { [[ -n $(apt-cache policy "$1" 2>/dev/null | awk '/Candidate:/ && $2 != "(none)" {print $2}') ]]; }
apt_updated=
# Package tool output goes to the log; the terminal gets one line per step.
apt_update() { [[ -n $apt_updated ]] || { apt-get update -qq >>"$log" 2>&1; apt_updated=1; }; }
apt_install() {
    local p missing=()
    for p in "$@"; do apt_installed "$p" || missing+=("$p"); done
    ((${#missing[@]})) || return 0
    say "installing ${missing[*]} (details in $log)"
    apt_update
    apt-get install -y -qq --no-install-recommends "${missing[@]}" >>"$log" 2>&1 ||
        fail "could not install ${missing[*]}" "see $log"
}
# apt_repo <name> <key url> <sources line with @KEYRING@>
apt_repo() {
    local ring=/usr/share/keyrings/$1.asc
    [[ -f /etc/apt/sources.list.d/$1.list ]] && return 0
    apt_install ca-certificates curl gnupg lsb-release
    install -d /usr/share/keyrings
    curl -fsSL -o "$ring" "$2" || fail "could not download the signing key for $1" "check the server's internet access"
    echo "${3//@KEYRING@/$ring}" >"/etc/apt/sources.list.d/$1.list"
    apt_updated=
    apt_update
}
install_packages() {
    apt_install ca-certificates curl openssl python3 iproute2 sudo
    if ! apt_candidate "postgresql-$pg"; then
        apt_repo pgdg https://www.postgresql.org/media/keys/ACCC4CF8.asc \
            "deb [signed-by=@KEYRING@] https://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main"
    fi
    apt_install "postgresql-$pg" "postgresql-client-$pg"
    if ! apt_candidate caddy; then
        apt_repo caddy-stable https://dl.cloudsmith.io/public/caddy/stable/gpg.key \
            'deb [signed-by=@KEYRING@] https://dl.cloudsmith.io/public/caddy/stable/deb/debian any-version main'
    fi
    apt_install caddy
}

# ---------------------------------------------------------------- users
ensure_users() {
    getent group bilibi >/dev/null || groupadd --system bilibi
    getent passwd bilibi >/dev/null ||
        useradd --system --gid bilibi --no-create-home --home-dir "$root" --shell /usr/sbin/nologin bilibi
    getent passwd bilibi-build >/dev/null ||
        useradd --system --user-group --create-home --home-dir "$root/build" --shell /bin/bash bilibi-build
    # bilibi-build reads each instance's env (the database address) to migrate.
    id -nG bilibi-build | tr ' ' '\n' | grep -qx bilibi || usermod -aG bilibi bilibi-build
    install -d -m 0750 -o bilibi-build -g bilibi-build "$root/build"
    if [[ -d /etc/sudoers.d ]]; then
        install -m 0440 "$here/server/sudoers" /etc/sudoers.d/bilibi-build
        visudo -cf /etc/sudoers.d/bilibi-build >/dev/null || fail 'the sudoers rule for bilibi-build did not validate'
    fi
}

# ---------------------------------------------------------------- database
ensure_cluster() {
    if ! pg_lsclusters -h | awk '{print $1}' | grep -qx "$pg"; then
        pg_createcluster "$pg" main >>"$log" 2>&1
    fi
    pg_ctlcluster "$pg" main start 2>/dev/null || true
    systemctl enable --now postgresql >/dev/null 2>&1 || true
    local port
    port=$(pg_lsclusters -h | awk -v v="$pg" '$1 == v {print $3; exit}')
    [[ -n $port ]] || fail "PostgreSQL $pg has no cluster" "check pg_lsclusters"
    pg_isready -q -h 127.0.0.1 -p "$port" || fail "PostgreSQL $pg is not accepting connections on port $port" 'check "systemctl status postgresql"'
    echo "$port"
}
# database_name <instance>
database_name() { printf 'bilibi_%s' "${1//-/_}"; }
# ensure_database <instance> <password>: a role and a database of its own.
# psql interpolates :variables only from standard input, never from -c.
ensure_database() {
    local name psql=(runuser -u postgres -- psql -v ON_ERROR_STOP=1 -Atq)
    name=$(database_name "$1")
    if [[ $("${psql[@]}" -c "SELECT 1 FROM pg_roles WHERE rolname = '$name'") != 1 ]]; then
        echo "CREATE ROLE :\"name\" LOGIN PASSWORD :'password'" | "${psql[@]}" -v name="$name" -v password="$2" >/dev/null
    fi
    if [[ $("${psql[@]}" -c "SELECT 1 FROM pg_database WHERE datname = '$name'") != 1 ]]; then
        echo 'CREATE DATABASE :"name" OWNER :"name"' | "${psql[@]}" -v name="$name" >/dev/null
    fi
}

# ---------------------------------------------------------------- ports
port_in_use() {
    [[ -n $(ss -H -ltn "sport = :$1") ]] && return 0
    local other
    for other in $(instances); do
        [[ $(env_value "$other" BIND_ADDR) == *":$1" ]] && return 0
    done
    return 1
}
free_port() {
    local port=4300
    while port_in_use "$port"; do ((port += 1)); ((port <= 65535)) || fail 'no free local port'; done
    echo "$port"
}

# ---------------------------------------------------------------- files
render_waiting_page() { # <instance> <role> <site> <out>
    local label colour
    case $2 in
        dev) label=Dev colour=info ;;
        staging) label=Staging colour=warning ;;
        *) label=Production colour=danger ;;
    esac
    python3 - "$here/waiting-page.html" "$4" "$label" "$colour" "$3" "$1" <<'PY'
import html, sys
source, out, label, colour, site, instance = sys.argv[1:]
page = open(source, encoding="utf-8").read()
def swap(old, new):
    global page
    if page.count(old) != 1:
        sys.exit(f"waiting-page.html no longer has exactly one {old[:40]!r}; update install-server.sh")
    page = page.replace(old, new)
swap('<title>Bilibi is starting · Business application platform</title>',
     f'<title>{html.escape(label)} is updating · Bilibi</title>')
swap('<html lang="en">', f'<html lang="en" data-role="{colour}">')
swap('<p class="instance" id="waiting-instance" hidden><span class="role" id="waiting-role"></span><span id="waiting-site"></span></p>',
     f'<p class="instance" id="waiting-instance"><span class="role" id="waiting-role">{html.escape(label.upper())}</span><span id="waiting-site">{html.escape(site)}</span></p>')
swap('<h1 id="waiting-heading">Bilibi is starting</h1>', f'<h1 id="waiting-heading">{html.escape(label)} is updating</h1>')
swap('<p id="waiting-text">The app is restarting or rebuilding. This page reloads on its own as soon as it is ready, so you will land back on what you asked for.</p>',
     f'<p id="waiting-text">Bilibi {html.escape(label)} is switching to a new release or restarting. Back in about a minute. This page reloads on its own, so you will land back on what you asked for.</p>')
swap('<p class="hint" id="waiting-hint">Taking longer than usual. If you started the app yourself, check the terminal running <code>scripts/start-app.sh</code> for a build error.</p>',
     f'<p class="hint" id="waiting-hint">Taking longer than usual. The server owner can check the service with <code>systemctl status bilibi@{html.escape(instance)}</code> and <code>journalctl -u bilibi@{html.escape(instance)}</code>.</p>')
open(out, "w", encoding="utf-8").write(page)
PY
}
write_caddy_site() { # <instance> <site> <port> <tls>
    local tls=''
    [[ $4 == internal ]] && tls='tls internal'
    install -d /etc/caddy/sites
    sed -e "s|@INSTANCE@|$1|g" -e "s|@SITE@|$2|g" -e "s|@PORT@|$3|g" -e "s|@TLS@|$tls|" \
        "$here/server/site.caddy" >"/etc/caddy/sites/bilibi-$1.caddy"
    local caddyfile=/etc/caddy/Caddyfile import='import /etc/caddy/sites/*.caddy'
    if [[ ! -f $caddyfile ]] || grep -q "Set this path to your site's directory" "$caddyfile"; then
        [[ ! -f $caddyfile || -f $caddyfile.dist ]] || cp "$caddyfile" "$caddyfile.dist"
        printf '# Managed by Bilibi scripts/install-server.sh\n%s\n' "$import" >"$caddyfile"
    elif ! grep -qF "$import" "$caddyfile"; then
        printf '\n# Added by Bilibi scripts/install-server.sh\n%s\n' "$import" >>"$caddyfile"
    fi
    caddy validate --config "$caddyfile" >/dev/null 2>&1 ||
        fail "/etc/caddy/Caddyfile does not validate" "run: caddy validate --config $caddyfile"
    systemctl enable --now caddy >/dev/null 2>&1
    systemctl reload caddy
}
install_unit() {
    local unit=/etc/systemd/system/$unit_name.service
    if ! cmp -s "$here/server/bilibi@.service" "$unit"; then
        install -m 0644 "$here/server/bilibi@.service" "$unit"
        systemctl daemon-reload
    fi
}
same_machine_warning() { # <instance> <role>
    local other
    for other in $(instances); do
        [[ $other == "$1" ]] && continue
        if [[ $2 == dev && $(role_of "$other") != dev ]] || [[ $2 != dev && $(role_of "$other") == dev ]]; then
            say "Warning: Dev shares this machine with $other ($(role_of "$other")). A build on Dev competes with it for CPU and memory, so people may find it slow while a build runs. Fine for a small server; put Dev on another machine when that matters."
            return
        fi
    done
}

# ================================================================ create
create() {
    local instance=${1:-} role='' site='' port='' tls=''
    [[ -n $instance ]] || usage
    shift
    while (($#)); do
        case $1 in
            --role) role=${2:-}; shift 2 ;;
            --site) site=${2:-}; shift 2 ;;
            --port) port=${2:-}; shift 2 ;;
            --tls) tls=${2:-}; shift 2 ;;
            *) usage ;;
        esac
    done
    valid_name "$instance" || fail "instance name must be 1 to 32 lower-case letters, digits or hyphens: $instance"
    [[ $role =~ ^(dev|staging|production)$ ]] || fail '--role must be dev, staging or production'
    [[ $site =~ ^[a-z0-9.-]+$ ]] || fail '--site must be a host name, like erp.example.com'
    [[ -z $port || $port =~ ^[0-9]+$ ]] || fail '--port must be a number'
    [[ -z $tls || $tls == internal ]] || fail '--tls takes only "internal" (a certificate from Caddy for a private name)'
    need_root create "$@"
    [[ -f $here/waiting-page.html && -f $here/server/site.caddy ]] || fail "run this from a Bilibi checkout: $here/server is missing"

    install_packages
    ensure_users
    install_unit
    local dir env created=
    dir=$(instance_dir "$instance"); env=$dir/env
    install -d -m 0755 "$root" "$dir" "$dir/site"
    install -d -m 0755 -o bilibi-build -g bilibi "$dir/releases"
    install -d -m 0750 -o bilibi-build -g bilibi-build "$dir/backups"
    install -d -m 0700 -o bilibi -g bilibi "$dir/data" "$dir/data/media" "$dir/data/mailbox"

    local pg_port password database
    pg_port=$(ensure_cluster)
    database=$(database_name "$instance")
    if [[ -f $env ]]; then
        password=$(sed -n 's|^DATABASE_URL=postgres://[^:]*:\([^@]*\)@.*|\1|p' "$env")
    else
        password=$(openssl rand -hex 24)
    fi
    [[ -n $password ]] || fail "$env has no DATABASE_URL password" "remove the file to generate a new one"
    ensure_database "$instance" "$password"
    if [[ ! -f $env ]]; then
        created=1
        [[ -n $port ]] || port=$(free_port)
        port_in_use "$port" && fail "port $port is in use" 'pick another with --port'
        local bilibi_env
        case $role in dev) bilibi_env=development ;; *) bilibi_env=$role ;; esac
        umask 027
        cat >"$env" <<EOF
# Bilibi instance "$instance". Read by bilibi@$instance.service and the release commands.
# BILIBI_ENV is the instance's role: development builds software, staging and
# production only install releases, and only development skips 2FA step-up.
BILIBI_ENV=$bilibi_env
APP_ORIGIN=https://$site
BIND_ADDR=127.0.0.1:$port
DATABASE_URL=postgres://$database:$password@127.0.0.1:$pg_port/$database
BILIBI_SETTINGS_KEY=$(openssl rand -hex 32)
BILIBI_USER_SIGNING_KEY=$(openssl rand -hex 32)
BILIBI_MEDIA_SIGNING_KEY=$(openssl rand -hex 32)
BILIBI_MAIL_DIR=$dir/data/mailbox
EOF
        umask 022
    else
        local kept_role; kept_role=$(role_of "$instance")
        [[ $kept_role == "$role" ]] || fail "$instance already exists with role $kept_role; the role is fixed at creation" "create another instance, or remove $dir and its database first"
        [[ -z $port || $(env_value "$instance" BIND_ADDR) == "127.0.0.1:$port" ]] || fail "$instance already listens on $(env_value "$instance" BIND_ADDR); the port is fixed at creation"
        port=${port:-$(env_value "$instance" BIND_ADDR | sed 's/.*://')}
        sed -i "s|^APP_ORIGIN=.*|APP_ORIGIN=https://$site|" "$env"
    fi
    chown root:bilibi "$env"; chmod 0640 "$env"

    render_waiting_page "$instance" "$role" "$site" "$dir/site/waiting.html"
    chmod 0644 "$dir/site/waiting.html"
    write_caddy_site "$instance" "$site" "$port" "$tls"
    systemctl enable "$unit_name$instance" >/dev/null 2>&1

    say "$instance: $([[ -n $created ]] && echo created || echo updated) as $role at https://$site (backend 127.0.0.1:$port), files under $dir"
    same_machine_warning "$instance" "$role"
    if [[ -z $(current_release "$instance") ]]; then
        say "Next: put a release into service with: $0 release $instance <path to bilibi-web>"
    fi
}

# ================================================================ release
with_env() { # <instance> <user> <command...>
    local instance=$1 user=$2; shift 2
    local dir; dir=$(instance_dir "$instance")
    # shellcheck disable=SC2016 # the inner shell expands $1 and $2
    runuser -u "$user" -- bash -c 'set -a; . "$1"; set +a; shift; export HOME="$2"; shift; exec "$@"' -- "$dir/env" "$dir/data" "$@"
}
# wait_healthy <instance>: the backend answers on its own port within 60 s.
wait_healthy() {
    local port; port=$(env_value "$1" BIND_ADDR | sed 's/.*://')
    local _try
    for _try in $(seq 1 60); do
        if [[ $(curl -s -o /dev/null -w '%{http_code}' --max-time 2 "http://127.0.0.1:$port/" 2>/dev/null) =~ ^[1-4][0-9][0-9]$ ]]; then
            return 0
        fi
        [[ $(service_state "$1") == failed ]] && return 1
        sleep 1
    done
    return 1
}
# put_in_service <instance> <n> <previous>: stop, point current, start, check;
# on failure point back at the previous release and start that.
put_in_service() {
    local instance=$1 n=$2 previous=$3 dir
    dir=$(instance_dir "$instance")
    systemctl stop "$unit_name$instance"
    ln -sfn "releases/$n" "$dir/current.new" && mv -T "$dir/current.new" "$dir/current"
    systemctl start "$unit_name$instance"
    if wait_healthy "$instance"; then
        say "$instance: release $n is in service ($(env_value "$instance" APP_ORIGIN))"
        return 0
    fi
    say "$instance: release $n did not answer within 60 s; see: journalctl -u $unit_name$instance" >&2
    if [[ -n $previous ]]; then
        systemctl stop "$unit_name$instance"
        ln -sfn "releases/$previous" "$dir/current.new" && mv -T "$dir/current.new" "$dir/current"
        systemctl start "$unit_name$instance"
        if wait_healthy "$instance"; then
            say "$instance: switched back to release $previous, which answers" >&2
        else
            say "$instance: release $previous does not answer either" >&2
        fi
    fi
    return 1
}
release() {
    local instance=${1:-} source=${2:-}
    [[ -n $instance && -n $source ]] || usage
    need_root release "$@"
    instance_exists "$instance" || fail "no instance named $instance" "create it first: $0 create $instance --role ... --site ..."
    local binary dir n previous
    if [[ -d $source ]]; then binary=$source/bilibi-web; else binary=$source; fi
    [[ -f $binary && -x $binary ]] || fail "$binary is not an executable bilibi-web"
    dir=$(instance_dir "$instance")
    previous=$(current_release "$instance")
    n=$(( $(release_numbers "$instance" | tail -n1) + 1 ))
    install -d -m 0755 -o bilibi-build -g bilibi "$dir/releases/$n"
    install -m 0755 -o bilibi-build -g bilibi "$binary" "$dir/releases/$n/bilibi-web"
    # The release's recipe, when the build wrote one (scripts/discover-modules.py --pin).
    local extra
    for extra in manifest.txt Cargo.lock; do
        [[ -d $source && -f $source/$extra ]] && install -m 0644 -o bilibi-build -g bilibi "$source/$extra" "$dir/releases/$n/$extra"
    done
    (cd "$dir/releases/$n" && sha256sum bilibi-web > SHA256SUMS && chown bilibi-build:bilibi SHA256SUMS)
    say "$instance: release $n copied to $dir/releases/$n"
    # The database must be up to date before the new program serves; nothing
    # migrates on boot. The service pauses from here: Caddy shows the waiting page.
    [[ -n $previous ]] && systemctl stop "$unit_name$instance"
    if ! with_env "$instance" bilibi-build "$dir/releases/$n/bilibi-web" migrate ||
        ! with_env "$instance" bilibi-build "$dir/releases/$n/bilibi-web" seed-production; then
        [[ -n $previous ]] && systemctl start "$unit_name$instance"
        fail "release $n could not update the database" "see the output above; the previous release is back in service"
    fi
    put_in_service "$instance" "$n" "$previous"
}
switch() {
    local instance=${1:-} n=${2:-}
    [[ -n $instance && $n =~ ^[0-9]+$ ]] || usage
    need_root switch "$@"
    instance_exists "$instance" || fail "no instance named $instance"
    local dir; dir=$(instance_dir "$instance")
    [[ -x $dir/releases/$n/bilibi-web ]] || fail "$instance has no release $n" "list them: $0 status $instance"
    (cd "$dir/releases/$n" && sha256sum -c --quiet SHA256SUMS) || fail "release $n does not match its checksum"
    # An older release may lack migrations the newer one applied; the database
    # keeps them (migrations are forward-only). Going back is the Software page's
    # Undo, which also restores a backup when tables changed.
    put_in_service "$instance" "$n" "$(current_release "$instance")"
}
status() {
    local instance list
    if [[ -n ${1:-} ]]; then list=$1; else list=$(instances); fi
    [[ -n $list ]] || { say "No instances under $root yet."; return; }
    for instance in $list; do
        instance_exists "$instance" || fail "no instance named $instance"
        local dir; dir=$(instance_dir "$instance")
        printf '%s: role %s, %s, service %s, current release %s\n' "$instance" "$(role_of "$instance")" \
            "$(env_value "$instance" APP_ORIGIN)" "$(service_state "$instance")" "$(current_release "$instance")"
        local r
        for r in $(release_numbers "$instance"); do
            printf '  release %s  %s\n' "$r" "$(cut -c1-12 "$dir/releases/$r/SHA256SUMS" 2>/dev/null)"
        done
    done
}

case ${1:-} in
    create) shift; create "$@" ;;
    release) shift; release "$@" ;;
    switch) shift; switch "$@" ;;
    status) shift; status "$@" ;;
    *) usage ;;
esac
