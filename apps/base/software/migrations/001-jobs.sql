-- The software of this server: requests to change it (jobs), the steps a
-- builder or installer reports while it works on one, and the plug-ins
-- switched on. Domains are not listed here: the running program is the
-- truth about what it contains (bilibi_module_registry::composition).
--
-- A job is written by `base/software` when a server owner commits a
-- reviewed plan; a builder (Dev) or installer (any instance) claims it,
-- reports its steps and sets the final state. `apps/base/software/README.md`
-- is the contract both sides follow.
CREATE TABLE base_software_jobs (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL,
 kind varchar(16) NOT NULL,
 state varchar(16) NOT NULL DEFAULT 'queued',
 -- The validated request as the owner reviewed it: the wanted Domains and
 -- versions, plug-ins, when to switch (change); the release (install); the
 -- job undone (undo). Never a secret.
 request jsonb NOT NULL,
 -- What the owner read in the review: the change list and the estimate.
 review jsonb NOT NULL,
 -- Written by the worker at the end: release numbers, whether the database
 -- was migrated and backed up, a plain message.
 result jsonb,
 -- The job this one undoes.
 undo_of bigint REFERENCES base_software_jobs(id),
 -- `switch`: put the new program into service as soon as it is built
 -- (`auto`) or build now and wait for the owner before the pause (`ask`).
 switch varchar(8) NOT NULL DEFAULT 'auto',
 requested_by bigint NOT NULL,
 requested_at timestamptz NOT NULL DEFAULT now(),
 -- Who claimed it (a worker's own name), and when it started and ended.
 worker varchar(64),
 started_at timestamptz,
 finished_at timestamptz,
 -- A cancel asked while the job runs; the worker stops before the pause.
 cancel_requested_at timestamptz,
 cancel_requested_by bigint,
 CONSTRAINT base_software_jobs_kind_check CHECK (kind IN ('change','install','undo')),
 CONSTRAINT base_software_jobs_state_check CHECK (state IN ('queued','running','done','failed','cancelled','interrupted')),
 CONSTRAINT base_software_jobs_switch_check CHECK (switch IN ('auto','ask'))
);
CREATE INDEX base_software_jobs_tenant_idx ON base_software_jobs(tenant_id, id DESC);
-- At most one job waiting or running per server.
CREATE UNIQUE INDEX base_software_jobs_active_idx ON base_software_jobs(tenant_id) WHERE state IN ('queued','running');

-- The steps of one job in order, created with the job as `pending`; the
-- worker marks each running, done, failed or skipped. `pauses` marks the
-- first step from which the instance is stopped, so a cancel is honoured
-- only before it.
CREATE TABLE base_software_steps (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL,
 job_id bigint NOT NULL REFERENCES base_software_jobs(id) ON DELETE CASCADE,
 seq integer NOT NULL,
 key varchar(32) NOT NULL,
 label varchar(120) NOT NULL,
 pauses boolean NOT NULL DEFAULT false,
 state varchar(16) NOT NULL DEFAULT 'pending',
 -- Progress or the reason for a failure, in plain words; never a secret.
 detail jsonb,
 started_at timestamptz,
 finished_at timestamptz,
 CONSTRAINT base_software_steps_state_check CHECK (state IN ('pending','running','done','failed','skipped')),
 CONSTRAINT base_software_steps_seq_key UNIQUE (job_id, seq)
);
CREATE INDEX base_software_steps_job_idx ON base_software_steps(tenant_id, job_id, seq);

-- Plug-ins switched on for this server. Loaded at run time, so the program
-- cannot tell: the list is the truth. A completed job writes it (seam).
CREATE TABLE base_software_plugins (
 tenant_id bigint NOT NULL,
 plugin_id varchar(64) NOT NULL,
 name varchar(120) NOT NULL,
 publisher varchar(120),
 version varchar(32),
 -- `catalogue`, or the Git address it came from.
 source varchar(255) NOT NULL DEFAULT 'catalogue',
 enabled boolean NOT NULL DEFAULT true,
 job_id bigint REFERENCES base_software_jobs(id),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT base_software_plugins_pkey PRIMARY KEY (tenant_id, plugin_id)
);
