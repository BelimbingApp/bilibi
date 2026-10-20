-- Retire instance sources only after their jobs have finished or been cancelled.
-- Never cancel a paused install in a migration: its installer owns recovery.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM base_software_jobs
               WHERE kind = 'install' AND state IN ('queued', 'running')
                 AND coalesce(request->'source'->>'kind', '') NOT IN ('github', 'upload')) THEN
        RAISE EXCEPTION 'Finish or cancel active instance-source installs before upgrading. History and Undo will be preserved.';
    END IF;
END $$;
ALTER TABLE base_software_jobs ADD CONSTRAINT base_software_jobs_asset_source_check
CHECK (kind <> 'install' OR state NOT IN ('queued', 'running')
       OR coalesce(request->'source'->>'kind', '') IN ('github', 'upload'));
DELETE FROM base_settings WHERE key IN ('software.pairing_key', 'instance.installs_from');
