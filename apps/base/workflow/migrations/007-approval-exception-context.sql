-- Control evidence retains the deciding edge without depending on Core.
-- Nullable historical fields mean unknown provenance, never invented consent.
ALTER TABLE base_workflow_control_exceptions ADD COLUMN impersonator bigint,
    ADD COLUMN agent_connection bigint, ADD COLUMN trace_id text;
ALTER TABLE base_workflow_control_reviews ADD COLUMN agent_connection bigint,
    ADD COLUMN trace_id text;
