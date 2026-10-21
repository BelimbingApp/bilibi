#!/usr/bin/env bash
# One command to prepare a developer machine (Ubuntu, Debian, WSL2) for
# ./scripts/start-app.sh. No options, no questions. Idempotent: each step checks
# first and skips what is present. Runs the steps in scripts/setup-steps/ in order;
# each step also runs on its own. Details go to .local/setup.log.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
mkdir -p "$root/.local"
: >"$root/.local/setup.log"
export SETUP_STATE
SETUP_STATE=$(mktemp)
trap 'rm -f "$SETUP_STATE"' EXIT

for step in "$root"/scripts/setup-steps/[0-9]*.sh; do
    SETUP_STEP=$(basename -- "$step" .sh) bash "$step" || exit 1
done

python3 "$root/scripts/install-platform-hook.py"

# shellcheck source=scripts/lib/install.sh
source "$root/scripts/lib/install.sh"
setup_summary
