-- Operator-built reports (BelimbingApp/bilibi#128, ADR 07). A report is a
-- saved question over one Grid table, never saved data: `definition` holds
-- column paths, filters, groupings, measures, sort and chart, validated by the
-- facade. `owner_id` is the account that wrote it; only it changes the report.
-- `shared_with_everyone` shares the definition with the whole tenant; the
-- share rows below limit it to roles instead. Each reader still sees only the
-- rows and fields their own grants allow.
CREATE TABLE base_reports (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 owner_id bigint NOT NULL,
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 120),
 description text NOT NULL DEFAULT '' CHECK (length(description) <= 1000),
 record_type text NOT NULL CHECK (record_type ~ '^[a-z][a-z0-9_]{0,63}$'),
 kind text NOT NULL CHECK (kind IN ('rows','summary')),
 definition jsonb NOT NULL CHECK (jsonb_typeof(definition) = 'object' AND octet_length(definition::text) <= 32768),
 shared_with_everyone boolean NOT NULL DEFAULT false,
 version bigint NOT NULL DEFAULT 1 CHECK (version >= 1),
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT base_reports_pkey PRIMARY KEY (id),
 CONSTRAINT base_reports_tenant_id_key UNIQUE (tenant_id, id)
);
CREATE INDEX base_reports_owner_idx ON base_reports (tenant_id, owner_id);
CREATE INDEX base_reports_everyone_idx ON base_reports (tenant_id) WHERE shared_with_everyone;

-- The roles one report is shared with. A role removed from the tenant drops
-- its shares with it.
CREATE TABLE base_report_shares (
 tenant_id bigint NOT NULL,
 report_id bigint NOT NULL,
 role_id bigint NOT NULL,
 CONSTRAINT base_report_shares_pkey PRIMARY KEY (tenant_id, report_id, role_id),
 CONSTRAINT base_report_shares_report_fkey FOREIGN KEY (tenant_id, report_id) REFERENCES base_reports (tenant_id, id) ON DELETE CASCADE,
 CONSTRAINT base_report_shares_role_fkey FOREIGN KEY (tenant_id, role_id) REFERENCES roles (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX base_report_shares_role_idx ON base_report_shares (tenant_id, role_id);
