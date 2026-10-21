-- Servers no longer build software. Retain old jobs as history, and free
-- their active slot even when a builder stopped after producing a release.
UPDATE base_software_steps AS step
SET state = 'skipped', finished_at = now(),
    detail = coalesce(step.detail, '{}'::jsonb) || jsonb_build_object('message', 'Server builds were removed. Add Domains in your local Bilibi copy and build there; then install the tested release here.')
FROM base_software_jobs AS job
WHERE step.tenant_id = job.tenant_id AND step.job_id = job.id
  AND job.kind = 'change' AND job.state IN ('queued', 'running')
  AND step.state IN ('pending', 'running');
UPDATE base_software_jobs
SET state = 'cancelled', finished_at = now(), settled_at = NULL,
    worker = coalesce(worker, 'software-migration'), started_at = coalesce(started_at, now()),
    result = coalesce(result, '{}'::jsonb) || jsonb_build_object('message', 'Server builds were removed. Add Domains in your local Bilibi copy and build there; then install the tested release here.')
WHERE kind = 'change' AND state IN ('queued', 'running');
-- Keep terminal legacy history for audit and Undo; no new change may wait.
ALTER TABLE base_software_jobs ADD CONSTRAINT base_software_jobs_no_build_check
CHECK (kind <> 'change' OR state NOT IN ('queued', 'running'));
-- The compiler setting belonged only to the removed builder. Shared job,
-- step and plug-in tables and all install/undo columns remain in use.
DELETE FROM base_settings WHERE key = 'software.build_jobs';
