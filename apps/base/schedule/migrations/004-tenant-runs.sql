-- Per-tenant reach (ADR 17). One row per (occurrence, tenant) records what the
-- fan-out of an `each_tenant` occurrence did in that tenant: a queued job and
-- its lifecycle, or why none was queued. The unique key makes a retried or
-- resumed fan-out converge on one job per tenant. A row with a job is written
-- in the same audited transaction as that job, in that tenant.
--
-- `fan_out_through` is the tenant-id high-water mark the first fan-out attempt
-- read; every later attempt walks the same cohort, so a tenant created after
-- the first page joins the next occurrence, never this one.
ALTER TABLE base_schedule_occurrences ADD COLUMN fan_out_through bigint CHECK (fan_out_through >= 0);

CREATE TABLE base_schedule_tenant_runs (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    occurrence_id bigint NOT NULL REFERENCES base_schedule_occurrences (id) ON DELETE CASCADE,
    key varchar(255) NOT NULL CHECK (key ~ '^[a-z0-9][a-z0-9._/-]{0,254}$'),
    tenant_id bigint NOT NULL CHECK (tenant_id > 0),
    trigger text NOT NULL CHECK (trigger IN ('scheduled','manual')),
    -- queued: job waiting, retrying or not yet started; running: an attempt
    -- is inside the owner's code; skipped: no job (suspended tenant, or the
    -- tenant's previous run still going); not_granted: the tenant has not
    -- granted the scheduler principal the capability.
    status text NOT NULL CHECK (status IN ('queued','running','succeeded','failed','skipped','not_granted')),
    code varchar(64) CHECK (code IS NULL OR code ~ '^[a-z][a-z0-9_]{0,63}$'),
    job_id bigint CHECK (job_id > 0),
    -- Set only on a forbid-overlap definition while the run is unfinished: the
    -- partial unique index below is the one active lease per (key, tenant).
    overlap_key varchar(300),
    -- First moment an attempt passed the global start check. Once set, a retry
    -- continues the run even if the definition is disabled meanwhile.
    started_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    finished_at timestamptz,
    CHECK (status NOT IN ('queued','running') OR job_id IS NOT NULL),
    CHECK (status NOT IN ('skipped','not_granted') OR job_id IS NULL),
    CHECK ((status IN ('queued','running')) = (finished_at IS NULL)),
    CHECK (finished_at IS NULL OR overlap_key IS NULL),
    CONSTRAINT base_schedule_tenant_runs_unique UNIQUE (occurrence_id, tenant_id)
);
CREATE UNIQUE INDEX base_schedule_tenant_runs_active_overlap_unique
    ON base_schedule_tenant_runs (overlap_key) WHERE overlap_key IS NOT NULL AND finished_at IS NULL;
CREATE UNIQUE INDEX base_schedule_tenant_runs_job_unique
    ON base_schedule_tenant_runs (tenant_id, job_id) WHERE job_id IS NOT NULL;
CREATE INDEX base_schedule_tenant_runs_unfinished
    ON base_schedule_tenant_runs (created_at, id) WHERE finished_at IS NULL;
CREATE INDEX base_schedule_tenant_runs_tenant ON base_schedule_tenant_runs (tenant_id, id DESC);

-- A run row recorded inside a tenant's job names that tenant.
ALTER TABLE base_schedule_runs ADD COLUMN tenant_id bigint CHECK (tenant_id > 0);
CREATE INDEX base_schedule_runs_tenant_started ON base_schedule_runs (tenant_id, started_at DESC) WHERE tenant_id IS NOT NULL;
