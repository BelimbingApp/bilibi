-- Malware scanning (issue 141, ADR 12). Every file Media stores carries its
-- scanner's verdict: `unscanned` (no scanner was installed, or the file was
-- too large for it), `pending` until the `base/media-scan` job answers, then
-- `clean`, `infected` or `failed`. Only `clean`, and `unscanned` while the
-- operator's policy allows it, is ever read back (`src/scan.rs`). Existing
-- files are `unscanned`. Each rescan raises `scan_generation`; a job records
-- its verdict only while the row still carries the generation it was queued
-- for, so an older scan still in flight can never decide a newer one.
ALTER TABLE base_media_assets
 ADD COLUMN scan_state text NOT NULL DEFAULT 'unscanned' CHECK (scan_state IN ('unscanned', 'pending', 'clean', 'infected', 'failed')),
 ADD COLUMN scan_generation bigint NOT NULL DEFAULT 0 CHECK (scan_generation >= 0),
 ADD COLUMN scanner text CHECK (length(scanner) BETWEEN 1 AND 64),
 ADD COLUMN scanner_version text CHECK (length(scanner_version) BETWEEN 1 AND 64),
 ADD COLUMN scan_signature text CHECK (length(scan_signature) BETWEEN 1 AND 255),
 ADD COLUMN scan_reason text CHECK (scan_reason IN ('too_large', 'scanner_unavailable', 'file_unavailable', 'scan_error')),
 ADD COLUMN scanned_at timestamptz,
 -- A quarantine manager's purge commits `purge_requested_at` with the
 -- `base/media-purge` job; the job removes the bytes afterwards and records
 -- `bytes_purged_at`. No byte goes before the request commits.
 ADD COLUMN purge_requested_at timestamptz,
 ADD COLUMN bytes_purged_at timestamptz,
 -- The account that stored the file, told when it is quarantined. Rows from
 -- before this migration name nobody.
 ADD COLUMN created_by_id bigint,
 ADD CONSTRAINT base_media_assets_signature CHECK (scan_signature IS NULL OR scan_state = 'infected'),
 ADD CONSTRAINT base_media_assets_reason CHECK (scan_reason IS NULL OR scan_state IN ('unscanned', 'failed')),
 ADD CONSTRAINT base_media_assets_purged CHECK (purge_requested_at IS NULL OR scan_state IN ('infected', 'failed')),
 ADD CONSTRAINT base_media_assets_purge_order CHECK (bytes_purged_at IS NULL OR purge_requested_at IS NOT NULL);
-- The quarantine page lists a tenant's files that are not clean.
CREATE INDEX base_media_assets_scan_idx ON base_media_assets (tenant_id, scan_state, id) WHERE scan_state <> 'clean';

-- A purge is final: once requested the row stays as evidence with its verdict
-- fixed and the request standing, and removed bytes never come back.
CREATE FUNCTION base_media_assets_purge_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
 IF (OLD.purge_requested_at IS NOT NULL AND (NEW.purge_requested_at IS DISTINCT FROM OLD.purge_requested_at OR NEW.scan_state IS DISTINCT FROM OLD.scan_state))
  OR (OLD.bytes_purged_at IS NOT NULL AND NEW.bytes_purged_at IS DISTINCT FROM OLD.bytes_purged_at) THEN
  RAISE EXCEPTION 'purged files keep their verdict' USING ERRCODE = 'restrict_violation';
 END IF;
 RETURN NEW;
END $$;
CREATE TRIGGER base_media_assets_purge_guard BEFORE UPDATE OF purge_requested_at, bytes_purged_at, scan_state ON base_media_assets FOR EACH ROW EXECUTE FUNCTION base_media_assets_purge_guard();
