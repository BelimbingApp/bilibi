-- Tenant-defined custom fields (`base/custom_fields`): one row per tenant,
-- record type and key. The key is the name a record's value is stored under
-- and never changes, nor does the type. The label and help text are the
-- tenant's to edit. A definition is archived, never deleted: records keep the
-- values stored under an archived key, and the key is never given to another
-- field. A choice field names the standalone pick-list it takes its values
-- from (`base/picklists`).
CREATE TABLE base_custom_fields (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 record_type text NOT NULL,
 key text NOT NULL,
 label text NOT NULL,
 help text,
 kind text NOT NULL CHECK (kind IN ('text', 'number', 'date', 'choice')),
 picklist text,
 position integer NOT NULL,
 archived_on date,
 version bigint NOT NULL DEFAULT 1,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT base_custom_fields_key UNIQUE (tenant_id, record_type, key),
 CONSTRAINT base_custom_fields_picklist CHECK ((kind = 'choice') = (picklist IS NOT NULL))
);
