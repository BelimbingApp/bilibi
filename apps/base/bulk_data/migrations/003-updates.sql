-- Bulk updates (issue 133): an update sets one field on many records of one
-- record type, the records ticked on a list or every record its filters
-- match, each through the type's own update contract as the person who
-- started it. The records are resolved once, when the job starts, with the
-- version each had then (`targets`, an array of [id, version]); the owner's
-- version fence refuses a record changed since. Rows hold one result per
-- record, keyed so a retried job never records a record twice. Written only
-- through Bulk Data's audited facade and its job; every statement carries
-- the tenant. The filter's values are kept only until the records are
-- resolved, never in the audit trail.
CREATE TABLE base_bulk_data_updates (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    company_id bigint NOT NULL,
    record_type text NOT NULL CHECK (char_length(record_type) BETWEEN 1 AND 200),
    started_by bigint NOT NULL,
    field text NOT NULL CHECK (char_length(field) BETWEEN 1 AND 100),
    value jsonb NOT NULL,
    chosen text NOT NULL CHECK (chosen IN ('selected', 'filter')),
    filter jsonb CHECK (filter IS NULL OR jsonb_typeof(filter) = 'object'),
    ids jsonb CHECK (ids IS NULL OR (jsonb_typeof(ids) = 'array' AND jsonb_array_length(ids) >= 1)),
    -- With a filter, the records left out (unticked after "Select all").
    except_ids jsonb CHECK (except_ids IS NULL OR (jsonb_typeof(except_ids) = 'array' AND jsonb_array_length(except_ids) >= 1)),
    targets jsonb CHECK (targets IS NULL OR jsonb_typeof(targets) = 'array'),
    state text NOT NULL DEFAULT 'resolving'
        CHECK (state IN ('resolving', 'running', 'completed', 'cancelled', 'failed')),
    next_row integer NOT NULL DEFAULT 0 CHECK (next_row >= 0),
    error text CHECK (char_length(error) <= 500),
    job_id bigint,
    trace_id text CHECK (char_length(trace_id) <= 200),
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    updated_at timestamptz NOT NULL DEFAULT clock_timestamp(),
    finished_at timestamptz,
    CHECK ((state = 'failed') = (error IS NOT NULL)),
    CHECK ((state IN ('resolving', 'running')) = (finished_at IS NULL)),
    CHECK (state IN ('resolving', 'failed', 'cancelled') OR targets IS NOT NULL),
    CHECK (state <> 'resolving' OR (chosen = 'selected') = (ids IS NOT NULL AND filter IS NULL)),
    CHECK (state <> 'resolving' OR (chosen = 'filter') = (filter IS NOT NULL AND ids IS NULL))
);
CREATE INDEX base_bulk_data_updates_starter_idx ON base_bulk_data_updates (tenant_id, started_by, id DESC);

CREATE TABLE base_bulk_data_update_rows (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    update_id bigint NOT NULL REFERENCES base_bulk_data_updates(id),
    row_number integer NOT NULL CHECK (row_number >= 1),
    record_id text NOT NULL CHECK (char_length(record_id) BETWEEN 1 AND 100),
    outcome text NOT NULL CHECK (outcome IN ('update', 'skip', 'error')),
    message text CHECK (char_length(message) <= 2000),
    UNIQUE (update_id, row_number)
);
CREATE INDEX base_bulk_data_update_rows_outcome_idx ON base_bulk_data_update_rows (tenant_id, update_id, outcome, row_number);

-- An export of a list's filters may leave records out too: those unticked
-- after "Select all N matching" on the list.
ALTER TABLE base_bulk_data_exports
    ADD COLUMN except_ids jsonb CHECK (except_ids IS NULL OR (jsonb_typeof(except_ids) = 'array' AND jsonb_array_length(except_ids) >= 1)),
    ADD CHECK (except_ids IS NULL OR ids IS NULL);
