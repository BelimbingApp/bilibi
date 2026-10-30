# shellcheck shell=bash
# Shared by scripts/setup.sh, scripts/setup-steps/* and .agents/setup. Source it;
# it runs nothing. Output is silent: tool output goes to .local/setup.log and one
# line is printed at the end, or what failed, why and what to do next.

SETUP_ROOT=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
SETUP_LOG="$SETUP_ROOT/.local/setup.log"
mkdir -p "$SETUP_ROOT/.local"
# shellcheck source=scripts/lib/versions.sh
source "$SETUP_ROOT/scripts/lib/versions.sh"

export DEBIAN_FRONTEND=noninteractive
SETUP_STEP=${SETUP_STEP:-$(basename -- "${BASH_SOURCE[1]:-setup}" .sh)}

# Results go to a state file so a parent run can print one summary line.
if [[ -z ${SETUP_STATE:-} ]]; then
    SETUP_STATE=$(mktemp)
    SETUP_STANDALONE=1
fi

installed() { printf 'installed %s\n' "$1" >>"$SETUP_STATE"; }
present() { printf 'present %s\n' "$1" >>"$SETUP_STATE"; }
note() { printf 'note %s\n' "$1" >>"$SETUP_STATE"; }

join() { local IFS=,; local s="$*"; echo "${s//,/, }"; }

setup_summary() {
    local kind name text out=() have=() notes=() parts=()
    while read -r kind name; do
        case $kind in
            installed) out+=("$name") ;;
            present) have+=("$name") ;;
            note) notes+=("$name") ;;
        esac
    done <"$SETUP_STATE"
    if ((${#out[@]})); then parts+=("installed: $(join "${out[@]}")"); fi
    if ((${#have[@]})); then parts+=("already present: $(join "${have[@]}")"); fi
    text='setup: ok'
    if ((${#parts[@]})); then
        local IFS=';'
        text+=" (${parts[*]//;/; })"
        unset IFS
    fi
    echo "$text"
    # Notes are exceptions the user has to act on, so they are printed.
    for text in "${notes[@]}"; do echo "setup: $text"; done
}

# fail <what failed> <why> <next action>
fail() {
    trap - EXIT
    printf 'setup: %s failed: %s\nNext: %s\nLog: %s\n' "$1" "$2" "$3" "$SETUP_LOG" >&2
    exit 1
}

setup_unexpected_error() {
    local status=$? command=$BASH_COMMAND
    trap - ERR EXIT
    printf 'setup: %s failed: "%s" exited %s\nNext: fix the cause in the log, then rerun ./bilibi setup (it skips what is done)\nLog: %s\n' \
        "$SETUP_STEP" "$command" "$status" "$SETUP_LOG" >&2
    exit 1
}
set -Eeuo pipefail
trap setup_unexpected_error ERR
if [[ -n ${SETUP_STANDALONE:-} ]]; then
    trap 'setup_summary; rm -f "$SETUP_STATE"' EXIT
fi

# log <command...>: run quietly, output to the log. Failure trips the ERR trap.
log() {
    printf '\n$ %s\n' "$*" >>"$SETUP_LOG"
    "$@" >>"$SETUP_LOG" 2>&1
}

# Platform checks live here and nowhere else.
setup_os_id() { (. /etc/os-release && echo "${ID:-}"); }
setup_os_like() { (. /etc/os-release && echo "${ID:-} ${ID_LIKE:-}"); }
is_wsl() { grep -qiE 'microsoft|wsl' /proc/version 2>/dev/null; }

require_supported_system() {
    [[ $(uname -s) == Linux ]] || fail platform "$(uname -s) is not supported" 'use Ubuntu, Debian or WSL2 with Ubuntu or Debian'
    if [[ ! -r /etc/os-release ]] || ! command -v apt-get >/dev/null; then
        fail platform 'no apt-based distribution found' 'use Ubuntu, Debian or WSL2 with Ubuntu or Debian'
    fi
    case " $(setup_os_like) " in
        *' ubuntu '* | *' debian '*) ;;
        *) fail platform "$(setup_os_id) is not supported" 'use Ubuntu, Debian or WSL2 with Ubuntu or Debian' ;;
    esac
}
require_supported_system

# need_root: lazily called before the first privileged command, so a rerun that
# has nothing to install never asks for a password.
SUDO=()
need_root() {
    [[ $(id -u) -eq 0 ]] && return 0
    command -v sudo >/dev/null || fail sudo 'sudo is not installed and you are not root' 'install sudo, or rerun as root'
    if [[ -t 0 ]]; then SUDO=(sudo); else SUDO=(sudo -n); fi
    "${SUDO[@]}" -v >>"$SETUP_LOG" 2>&1 || fail sudo 'sudo needs a password it cannot get' 'run ./bilibi setup from a terminal, or rerun as root'
}
as_root() { need_root; log "${SUDO[@]}" "$@"; }

apt_package_installed() { dpkg-query -W -f='${Status}' "$1" 2>/dev/null | grep -q '^install ok installed$'; }
# apt_candidate <package>: true when a configured repository offers it.
apt_candidate() { [[ -n $(apt-cache policy "$1" 2>/dev/null | awk '/Candidate:/ && $2 != "(none)" {print $2}') ]]; }

APT_UPDATED=
apt_update() {
    [[ -n $APT_UPDATED ]] && return 0
    as_root apt-get update -qq
    APT_UPDATED=1
}
# apt_install <package...>: installs what is missing; returns 1 if all were present.
apt_install() {
    local p missing=()
    for p in "$@"; do apt_package_installed "$p" || missing+=("$p"); done
    ((${#missing[@]})) || return 1
    apt_update
    as_root apt-get install -y -qq --no-install-recommends "${missing[@]}"
}

# Third-party apt repository: apt_repo <name> <key url> <sources line>
apt_repo() {
    local name=$1 key=$2 line=$3 ring=/usr/share/keyrings/$1.asc
    [[ -f /etc/apt/sources.list.d/$name.list ]] && return 0
    apt_install ca-certificates curl gnupg || true
    as_root install -d /usr/share/keyrings
    need_root
    log "${SUDO[@]}" curl -fsSL -o "$ring" "$key"
    echo "${line//@KEYRING@/$ring}" | "${SUDO[@]}" tee "/etc/apt/sources.list.d/$name.list" >/dev/null
    APT_UPDATED=
    apt_update
}

load_cargo_env() {
    # shellcheck disable=SC1091
    [[ -f $HOME/.cargo/env ]] && source "$HOME/.cargo/env"
    return 0
}

version_ge() { [[ $(printf '%s\n%s\n' "$1" "$2" | sort -V | head -n1) == "$2" ]]; }
