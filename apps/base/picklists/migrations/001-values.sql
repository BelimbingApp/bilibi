-- Governed pick-list values (`base/picklists`): one row per tenant, list and
-- code. The code is what an owner's record stores and never changes; the
-- label, its translations (`labels`, keyed by language), hint, order and
-- default are the tenant's to edit. A value is retired, never deleted:
-- records that hold it keep it. Whether a value is locked comes from its
-- declaration, not from this table. An installation-wide list keeps its rows
-- on the platform-operator tenant.
CREATE TABLE base_picklist_values (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 list text NOT NULL,
 code text NOT NULL,
 label text NOT NULL,
 labels jsonb NOT NULL DEFAULT '{}'::jsonb,
 hint text,
 position integer NOT NULL,
 is_default boolean NOT NULL DEFAULT false,
 parent_codes text[] NOT NULL DEFAULT '{}',
 retired_on date,
 version bigint NOT NULL DEFAULT 1,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT base_picklist_values_code_key UNIQUE (tenant_id, list, code)
);
CREATE UNIQUE INDEX base_picklist_values_default_idx ON base_picklist_values(tenant_id, list) WHERE is_default;
