#!/usr/bin/env bash
# Build generated UI CSS; no Node process is needed by the running Rust app.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
assets="$root/apps/base/ui/assets"
command -v npm >/dev/null || { echo 'Install Node.js >=22 and npm to build UI assets.' >&2; exit 1; }
# Refresh dependencies when the lock changes, including direct Cargo builds.
lock_stamp="$assets/node_modules/.bilibi-package-lock.json"
if [[ ! -x "$assets/node_modules/.bin/tailwindcss" ]] || ! cmp -s "$assets/package-lock.json" "$lock_stamp"; then
    npm ci --prefix "$assets" --no-audit --no-fund
    cp "$assets/package-lock.json" "$lock_stamp"
fi
node "$assets/build.cjs" "$@"
