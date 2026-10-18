-- When a worker's outcome went into the audit trail. The installer writes a
-- job's steps, state and result directly (it runs outside the program, as
-- bilibi-build), so the module records each finished job as an audited change
-- once it sees it; this is its mark. The builder records its own outcome in
-- one audited transaction and sets the mark itself.
ALTER TABLE base_software_jobs ADD COLUMN settled_at timestamptz;
-- Every job finished before this migration was finished by the builder inside
-- an audited transaction, with its notification, so none of them is settled
-- again.
UPDATE base_software_jobs SET settled_at = finished_at WHERE finished_at IS NOT NULL;
