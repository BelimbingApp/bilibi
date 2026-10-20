#!/usr/bin/env bash
# Disposable nspawn lab for the server proof. No Bilimbi counterpart.
# Keep all generated material outside the checkout: BILIBI_PROOF_DATA=/absolute/path.
set -euo pipefail
DATA=${BILIBI_PROOF_DATA:?set BILIBI_PROOF_DATA to an empty task folder outside the checkout}
SRC=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd -P)
NAME=fsproof
NET=10.77.61
ROOT=$DATA/lab/fresh
[[ $DATA == /* && $DATA != / && $DATA != "$SRC" && $DATA != "$SRC/"* ]] || exit 2

run() {
    local actual
    actual=$(sudo -n machinectl show "$NAME" -p RootDirectory --value)
    [[ $actual == "$ROOT" ]] || { echo "Refusing machine outside this task: $actual" >&2; exit 1; }
    sudo -n systemd-run -M "$NAME" -P --wait --quiet --collect /bin/bash -euo pipefail -c "$*"
}

case ${1:-} in
prepare)
    [[ ! -e $ROOT ]] || { echo 'Lab root already exists.' >&2; exit 1; }
    mkdir -p "$DATA/lab" "$DATA/bin" "$DATA/proof/answers" "$DATA/proof/shots"
    # An existing clean debootstrap base can be passed as the second argument.
    # Copy it; never boot or modify somebody else's base in place.
    if [[ -n ${2:-} ]]; then
        sudo -n cp -a -- "$2" "$ROOT"
    else
        sudo -n debootstrap --variant=minbase --components=main,universe \
            --include=systemd-sysv,dbus,iproute2,curl,ca-certificates,sudo,python3,gnupg,lsb-release,openssl \
            resolute "$ROOT" http://archive.ubuntu.com/ubuntu >"$DATA/lab/debootstrap.log" 2>&1
    fi
    printf '%s\n' "$NAME" | sudo -n tee "$ROOT/etc/hostname" >/dev/null
    printf '127.0.1.1 %s\n' "$NAME" | sudo -n tee -a "$ROOT/etc/hosts" >/dev/null
    sudo -n touch "$ROOT/.bilibi-server-proof"
    ;;
boot)
    [[ -f $ROOT/.bilibi-server-proof && ! -e $DATA/lab/nspawn.pid ]] || exit 1
    # The lab adds only its own tagged rules; it never changes host forwarding.
    [[ $(sysctl -n net.ipv4.ip_forward) == 1 ]] || { echo 'Host IP forwarding must already be enabled.' >&2; exit 1; }
    if ip link show "ve-$NAME" >/dev/null 2>&1 || sudo -n machinectl show "$NAME" >/dev/null 2>&1; then
        echo 'The lab machine name is already in use.' >&2; exit 1
    fi
    sudo -n iptables -t nat -A POSTROUTING -s "$NET.0/24" -m comment --comment bilibi-fresh-server-proof -j MASQUERADE
    sudo -n iptables -I FORWARD -s "$NET.0/24" -m comment --comment bilibi-fresh-server-proof -j ACCEPT
    sudo -n iptables -I FORWARD -d "$NET.0/24" -m comment --comment bilibi-fresh-server-proof -j ACCEPT
    touch "$DATA/lab/network-owned"
    nohup sudo -n systemd-nspawn -D "$ROOT" -M "$NAME" -b --network-veth --console=read-only \
        --bind-ro="$SRC:/src" --bind="$DATA:/data" >"$DATA/lab/console.log" 2>&1 &
    for _ in {1..30}; do ip link show "ve-$NAME" >/dev/null 2>&1 && break; sleep 1; done
    sudo -n ip addr add "$NET.1/24" dev "ve-$NAME"
    sudo -n ip link set "ve-$NAME" up
    for _ in {1..30}; do sudo -n machinectl show "$NAME" >/dev/null 2>&1 && break; sleep 1; done
    # sudo's launcher can exit while nspawn keeps serving. The registered
    # container leader's parent is the nspawn process we must stop.
    [[ $(sudo -n machinectl show "$NAME" -p RootDirectory --value) == "$ROOT" ]] || exit 1
    leader=$(sudo -n machinectl show "$NAME" -p Leader --value)
    pid=$(awk '/^PPid:/ {print $2}' "/proc/$leader/status")
    [[ $pid -gt 1 ]] || exit 1
    printf '%s\n' "$pid" >"$DATA/lab/nspawn.pid"
    awk '{print $22}' "/proc/$pid/stat" >"$DATA/lab/nspawn.start"
    sleep 3
    run "ip addr add $NET.2/24 dev host0; ip link set host0 up; ip route add default via $NET.1; printf 'nameserver 1.1.1.1\nnameserver 8.8.8.8\n' > /etc/resolv.conf"
    ;;
run)
    shift
    run "$*"
    ;;
stop)
    pid=$(cat "$DATA/lab/nspawn.pid")
    if [[ -e /proc/$pid/stat ]]; then
        [[ $(awk '{print $22}' "/proc/$pid/stat") == "$(cat "$DATA/lab/nspawn.start")" ]] || exit 1
        sudo -n kill -TERM "$pid"
        for _ in {1..45}; do [[ ! -e /proc/$pid ]] && break; sleep 1; done
        [[ ! -e /proc/$pid ]] || { echo 'The owned launcher has not stopped.' >&2; exit 1; }
    fi
    ;;
destroy)
    [[ -f $ROOT/.bilibi-server-proof ]] || exit 1
    [[ ! -e $DATA/lab/nspawn.pid || ! -e /proc/$(cat "$DATA/lab/nspawn.pid") ]] || exit 1
    if sudo -n findmnt -rn -o TARGET | awk -v root="$ROOT" '$0 == root || index($0, root "/") == 1 { found=1 } END { exit !found }'; then
        echo 'The lab still has mounted filesystems; refusing deletion.' >&2; exit 1
    fi
    if [[ -f $DATA/lab/network-owned ]]; then
        sudo -n iptables -t nat -D POSTROUTING -s "$NET.0/24" -m comment --comment bilibi-fresh-server-proof -j MASQUERADE
        sudo -n iptables -D FORWARD -s "$NET.0/24" -m comment --comment bilibi-fresh-server-proof -j ACCEPT
        sudo -n iptables -D FORWARD -d "$NET.0/24" -m comment --comment bilibi-fresh-server-proof -j ACCEPT
        rm "$DATA/lab/network-owned"
    fi
    sudo -n rm -r -- "$ROOT"
    for record in nspawn.pid nspawn.start; do
        [[ ! -e $DATA/lab/$record ]] || rm -- "$DATA/lab/$record"
    done
    ;;
*) echo 'Usage: lab.sh prepare [clean-base] | boot | run <command> | stop | destroy' >&2; exit 2 ;;
esac
