-- Owner floors are frozen with each submitted revision. Existing unsnapshotted
-- rounds remain readable/recallable; decisions fail closed until resubmission.
ALTER TABLE base_workflow_approval_requests ADD COLUMN controls jsonb;
CREATE TABLE base_workflow_approval_maintenance (
    tenant_id bigint PRIMARY KEY, last_request bigint NOT NULL DEFAULT 0
);
