#!/usr/bin/env bash
# Used on the first launch and every watched rebuild. Cargo's own diagnostics
# remain visible, plus explicit wall-clock timing including failed compilations.
#
# On a change cargo-watch kills the whole process group of the running copy of
# this script (SIGTERM, then SIGKILL before it starts the next copy), so the old
# copy cannot say why. Each copy records what it is doing in $state, and the next
# copy reads it to explain the restart. A run that ends on its own (a failed
# step, or the app stopping) leaves cargo-watch idle until the next change, so it
# says how to recover. start-app.sh clears $state on launch.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
cd "$root"
state="$root/.local/dev-run.state"
mkdir -p "$root/.local"
case $(cat "$state" 2>/dev/null || true) in
    serve) echo 'Files changed (for example after git pull): Bilibi stopped and is rebuilding. It starts again by itself when the build finishes.' ;;
    build) echo 'Files changed during the build: starting the build again.' ;;
    ended) echo 'Files changed: building and starting Bilibi again.' ;;
esac
echo build > "$state"
retry='Fix the error and save a file to try again, or press Ctrl-C and run ./scripts/start-app.sh again.'
# Ctrl-C: cargo-watch passes SIGINT on and exits; stop quietly.
trap 'exit 130' INT
not_running() {
    echo ended > "$state"
    echo "Bilibi is not running: $1"
}
# Run one step; if it fails, say which and how to recover.
step() {
    local what=$1 result=0
    shift
    "$@" || result=$?
    if [[ $result != 0 ]]; then
        not_running "$what failed (see above). $retry"
        exit "$result"
    fi
}
step 'module discovery' python3 scripts/discover-modules.py
step 'the CSS build' ./scripts/build-assets.sh
started=$(date +%s)
result=0
cargo build -p bilibi-web --timings || result=$?
echo "Rust build took $(($(date +%s)-started))s (exit $result). Timing report: target/cargo-timings/cargo-timing.html"
if [[ $result != 0 ]]; then
    not_running "the Rust build failed (see above). $retry"
    exit "$result"
fi
# A rebuild can carry a module's new migration; apply it before serving, as
# start-app.sh does on launch, so the running app never queries a schema the
# database lacks. The seed stays launch-only.
step 'the database migration' "$root/target/debug/bilibi-web" migrate
# A stop by name (pkill -f "target/debug/bilibi-web run", pkill bilibi-web,
# killall bilibi-web, kill $(pgrep -x bilibi-web)) reaches every process whose
# name or command line matches, so an agent stopping its own test server that
# way also stopped the developer's app. Serve a copy named bilibi-dev from
# .local/dev instead: neither its process name nor its command line matches.
# A hard link is instant and a rebuild writes a new file, so the link never
# serves a stale build; fall back to a copy where linking fails. Remove the old
# link first: after a change that rebuilt nothing it already is the same file.
dev_bin="$root/.local/dev/bilibi-dev"
mkdir -p "$root/.local/dev"
rm -f "$dev_bin"
ln "$root/target/debug/bilibi-web" "$dev_bin" 2>/dev/null \
    || cp "$root/target/debug/bilibi-web" "$dev_bin"
echo serve > "$state"
result=0
"$dev_bin" run || result=$?
# Reached only when the app stopped and this script was not killed with it.
if [[ $result == 143 ]]; then
    not_running 'another process stopped it (SIGTERM), not a file change. To start it again, save any file or run `touch Cargo.toml`, or press Ctrl-C and run ./scripts/start-app.sh again.'
else
    not_running "the app stopped (exit $result, see above). $retry"
fi
exit "$result"
