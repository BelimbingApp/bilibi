#!/usr/bin/env bash
# Disposable deployment evidence.
set -euo pipefail
[[ $(cat /run/systemd/container) == systemd-nspawn ]]
runuser -u postgres -- psql -d bilibi_production -P pager=off <<'SQL'
SELECT id, kind, state, result->>'release' AS release, result->>'previous_release' AS previous, result->>'migrated' AS migrated, result->>'switched_back' AS switched_back, result->>'restored' AS restored, result->>'message' AS message FROM base_software_jobs ORDER BY id;
SELECT job_id, seq, key, state, started_at AT TIME ZONE 'UTC' AS started_utc, finished_at AT TIME ZONE 'UTC' AS finished_utc, detail->>'message' AS message FROM base_software_steps ORDER BY job_id, seq;
SELECT id, event, system_principal, payload->>'operation' AS operation, payload->>'reason' AS reason FROM base_audit_actions WHERE event LIKE 'step_up.%' OR event LIKE 'software.%' ORDER BY id;
SELECT auditable_id, event, new_values->>'state' AS state, new_values->'result'->>'message' AS message FROM base_audit_mutations WHERE auditable_type='base/software.job' ORDER BY id;
SELECT count(*) AS finance_migrations FROM bilibi_migrations WHERE version LIKE 'finance-%';
SQL
/src/scripts/install-server.sh status production
