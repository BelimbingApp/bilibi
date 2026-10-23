-- Prepared exports (BelimbingApp/bilibi#128, phase 2): a result too large to
-- download at once is prepared on the Queue as the person who asked, stored
-- as a private expiring artifact of their company, and announced by the
-- bell. A row is the request and its outcome; only its requester reads it.
-- `report_name` keeps the name it was asked under, so a later rename or
-- delete of the report does not change what the list says.
CREATE TABLE base_report_exports (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 requester_id bigint NOT NULL,
 company_id bigint NOT NULL,
 report_id bigint NOT NULL,
 report_name text NOT NULL CHECK (length(report_name) BETWEEN 1 AND 120),
 format text NOT NULL CHECK (format IN ('csv','pdf')),
 answers jsonb NOT NULL DEFAULT '{}' CHECK (jsonb_typeof(answers) = 'object' AND octet_length(answers::text) <= 8192),
 state text NOT NULL DEFAULT 'queued' CHECK (state IN ('queued','ready','failed')),
 artifact_id text,
 filename text,
 rows integer CHECK (rows >= 0),
 message text CHECK (length(message) <= 500),
 expires_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 finished_at timestamptz,
 CONSTRAINT base_report_exports_pkey PRIMARY KEY (id),
 CONSTRAINT base_report_exports_tenant_id_key UNIQUE (tenant_id, id),
 CONSTRAINT base_report_exports_ready CHECK (state <> 'ready' OR (artifact_id IS NOT NULL AND filename IS NOT NULL))
);
CREATE INDEX base_report_exports_requester_idx ON base_report_exports (tenant_id, requester_id, created_at DESC);
