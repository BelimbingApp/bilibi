#!/usr/bin/env bash
# PostgreSQL server and client binaries. No cluster is created: the app runs its own
# private one under .local/postgres18, on its own port (scripts/start-app.sh).
set -euo pipefail
# shellcheck source=scripts/lib/install.sh
source "$(dirname -- "${BASH_SOURCE[0]}")/../lib/install.sh"

pg=$POSTGRESQL_MAJOR
if apt_package_installed "postgresql-$pg" && apt_package_installed "postgresql-client-$pg"; then
    present "postgresql-$pg"
    exit 0
fi
# Without this, postgresql-common would create and start a default cluster on 5432.
as_root install -d /etc/postgresql-common
need_root
echo 'create_main_cluster = false' | "${SUDO[@]}" tee /etc/postgresql-common/createcluster.conf >/dev/null
apt_update
if ! apt_candidate "postgresql-$pg"; then
    apt_repo pgdg https://www.postgresql.org/media/keys/ACCC4CF8.asc \
        "deb [signed-by=@KEYRING@] https://apt.postgresql.org/pub/repos/apt $(lsb_release -cs)-pgdg main"
fi
apt_install "postgresql-$pg" "postgresql-client-$pg" || true
installed "postgresql-$pg"
