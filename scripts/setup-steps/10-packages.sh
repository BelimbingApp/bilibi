#!/usr/bin/env bash
# Build tools and the small utilities scripts/start-app.sh needs. Python 3.11+.
set -euo pipefail
# shellcheck source=scripts/lib/install.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/install.sh"

if apt_install ca-certificates curl gnupg lsb-release build-essential pkg-config libssl-dev \
    python3 openssl iproute2 util-linux git; then
    installed 'base packages'
else
    present 'base packages'
fi
python3 -c "import sys; sys.exit(sys.version_info < tuple(map(int, '$PYTHON_MIN'.split('.'))))" ||
    fail python "Python $PYTHON_MIN or newer is required, found $(python3 -V)" "use a distribution release that ships Python $PYTHON_MIN or newer"
