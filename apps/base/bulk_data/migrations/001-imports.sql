-- CSV imports (issue 133): a run brings one file in for one record type
-- through that type's write contracts. Rows hold one result per data row and
-- pass (the dry run, then the real run), keyed so a retried job never
-- records a row twice. Mappings are saved by name for a record type and
-- shared within the company. Written only through Bulk Data's audited
-- facade and contracts; every statement carries the tenant.
CREATE TABLE base_bulk_data_runs (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    record_type text NOT NULL CHECK (char_length(record_type) BETWEEN 1 AND 200),
    started_by bigint NOT NULL,
    file_name text NOT NULL CHECK (char_length(file_name) BETWEEN 1 AND 255),
    source_artifact text CHECK (char_length(source_artifact) <= 100),
    header jsonb NOT NULL CHECK (jsonb_typeof(header) = 'array'),
    row_count integer NOT NULL CHECK (row_count >= 0),
    mapping jsonb CHECK (mapping IS NULL OR jsonb_typeof(mapping) = 'object'),
    state text NOT NULL DEFAULT 'draft'
        CHECK (state IN ('draft', 'previewing', 'previewed', 'running', 'completed', 'cancelled')),
    pass text CHECK (pass IN ('preview', 'run')),
    next_row integer NOT NULL DEFAULT 0 CHECK (next_row >= 0 AND next_row <= row_count),
    job_id bigint,
    trace_id text CHECK (char_length(trace_id) <= 200),
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz,
    CHECK ((state IN ('previewing', 'previewed') AND pass = 'preview')
        OR (state IN ('running', 'completed') AND pass = 'run')
        OR state IN ('draft', 'cancelled'))
);
CREATE INDEX base_bulk_data_runs_starter_idx ON base_bulk_data_runs (tenant_id, started_by, id DESC);

CREATE TABLE base_bulk_data_rows (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    run_id bigint NOT NULL REFERENCES base_bulk_data_runs(id),
    pass text NOT NULL CHECK (pass IN ('preview', 'run')),
    row_number integer NOT NULL CHECK (row_number >= 1),
    outcome text NOT NULL CHECK (outcome IN ('create', 'update', 'skip', 'error')),
    record_id text CHECK (char_length(record_id) <= 100),
    source_key text CHECK (char_length(source_key) <= 255),
    message text CHECK (char_length(message) <= 2000),
    UNIQUE (run_id, pass, row_number)
);
CREATE INDEX base_bulk_data_rows_outcome_idx ON base_bulk_data_rows (tenant_id, run_id, pass, outcome, row_number);
CREATE INDEX base_bulk_data_rows_source_idx ON base_bulk_data_rows (tenant_id, run_id, source_key)
    WHERE pass = 'run' AND record_id IS NOT NULL;

CREATE TABLE base_bulk_data_mappings (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    record_type text NOT NULL CHECK (char_length(record_type) BETWEEN 1 AND 200),
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 120),
    columns jsonb NOT NULL CHECK (jsonb_typeof(columns) = 'array'),
    mapping jsonb NOT NULL CHECK (jsonb_typeof(mapping) = 'object'),
    created_by bigint NOT NULL,
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
CREATE UNIQUE INDEX base_bulk_data_mappings_name_idx
    ON base_bulk_data_mappings (tenant_id, company_id, record_type, lower(name));
