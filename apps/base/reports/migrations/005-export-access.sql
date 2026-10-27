-- Prepared exports and deliveries (BelimbingApp/bilibi#128, phase 2 review).
-- `access` is what the requester could see of the report when the file was
-- made (`Reports::access`: its version and which of its columns their
-- catalog resolves); the file is released only while that still holds.
-- `job_id` is the Queue job that fills an export or runs a delivery, so a
-- job Queue ends without the owner finishing it (an actor refused before it
-- ran) shows as failed instead of preparing for ever.
ALTER TABLE base_report_exports
 ADD COLUMN access text CHECK (length(access) <= 1000),
 ADD COLUMN job_id bigint;
ALTER TABLE base_report_deliveries ADD COLUMN job_id bigint;
