-- When a worker's outcome went into the audit trail. The installer writes a
-- job's steps, state and result directly (it runs outside the program, as
-- bilibi-build), so the module records each finished job as an audited change
-- once it sees it; this is its mark. The builder records its own outcome in
-- one audited transaction and sets the mark itself.
ALTER TABLE base_software_jobs ADD COLUMN settled_at timestamptz;
