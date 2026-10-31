-- CSV exports (issue 133): an export takes one list's filtered records (or
-- the records ticked on it), with the columns the list shows, out to a CSV file, read through the record
-- type's list contract and Grid as the person who started it. The file is a
-- private artifact owned by the export (kind `export`) that expires after
-- `bulk_data.file_days`. Written only through Bulk Data's audited facade and
-- its job; every statement carries the tenant. The filter's values are kept
-- here to read the list, never in the audit trail.
CREATE TABLE base_bulk_data_exports (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    record_type text NOT NULL CHECK (char_length(record_type) BETWEEN 1 AND 200),
    started_by bigint NOT NULL,
    filter jsonb NOT NULL CHECK (jsonb_typeof(filter) = 'object'),
    ids jsonb CHECK (ids IS NULL OR (jsonb_typeof(ids) = 'array' AND jsonb_array_length(ids) >= 1)),
    columns jsonb NOT NULL CHECK (jsonb_typeof(columns) = 'array' AND jsonb_array_length(columns) BETWEEN 1 AND 64),
    state text NOT NULL DEFAULT 'running' CHECK (state IN ('running', 'completed', 'failed')),
    row_count integer CHECK (row_count >= 0),
    artifact text CHECK (char_length(artifact) <= 100),
    error text CHECK (char_length(error) <= 500),
    -- What the person could see of the columns when the file was made; a
    -- download or the export's page compares it and refuses on a change.
    access text CHECK (char_length(access) <= 4000),
    -- When the file expires (Artifacts purges it after): past it, the
    -- export is shown expired and offers no download.
    expires_at timestamptz,
    job_id bigint,
    trace_id text CHECK (char_length(trace_id) <= 200),
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz,
    CHECK ((state = 'completed') = (artifact IS NOT NULL AND row_count IS NOT NULL AND access IS NOT NULL)),
    CHECK ((state = 'failed') = (error IS NOT NULL)),
    CHECK ((state = 'running') = (finished_at IS NULL))
);
CREATE INDEX base_bulk_data_exports_starter_idx ON base_bulk_data_exports (tenant_id, started_by, id DESC);
