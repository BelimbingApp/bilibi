-- A Matrix report (BelimbingApp/bilibi#128, phase 2): one grouping down, one
-- across, from the same grouped read as a Summary.
ALTER TABLE base_reports DROP CONSTRAINT base_reports_kind_check;
ALTER TABLE base_reports ADD CONSTRAINT base_reports_kind_check CHECK (kind IN ('rows','summary','matrix'));
