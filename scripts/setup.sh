#!/usr/bin/env bash
# Local launcher plumbing called by ./bilibi; do not run directly.
set -euo pipefail
root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
if [[ ${BILIBI_FRONT_DOOR:-} != 1 ]]; then
    echo "Starting Bilibi through ./bilibi setup"
    exec "$root/bilibi" setup
fi
mkdir -p "$root/.local"
: >"$root/.local/setup.log"
export SETUP_STATE
SETUP_STATE=$(mktemp)
trap 'rm -f "$SETUP_STATE"' EXIT

for step in "$root"/scripts/setup-steps/[0-9]*.sh; do
    SETUP_STEP=$(basename -- "$step" .sh) bash "$step" || exit 1
done

python3 "$root/scripts/install-platform-hook.py" --warn

# shellcheck source=scripts/lib/install.sh
source "$root/scripts/lib/install.sh"
setup_summary
