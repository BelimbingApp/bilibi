#!/usr/bin/env bash
# The rebuild watcher scripts/start-app.sh runs.
set -euo pipefail
# shellcheck source=scripts/lib/install.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/install.sh"

load_cargo_env
if [[ $(cargo watch --version 2>/dev/null | awk '{print $2}') == "$CARGO_WATCH_VERSION" ]]; then
    present cargo-watch
else
    log cargo install cargo-watch --version "$CARGO_WATCH_VERSION" --locked
    installed cargo-watch
fi
