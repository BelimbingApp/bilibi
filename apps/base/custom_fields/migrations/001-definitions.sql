CREATE TABLE base_custom_field_definitions (
  id bigserial PRIMARY KEY,
  tenant_id bigint NOT NULL REFERENCES tenants(id),
  record_type text NOT NULL,
  field_key text NOT NULL CHECK (field_key ~ '^[a-z][a-z0-9_]{0,62}$'),
  label text NOT NULL CHECK (length(label) BETWEEN 1 AND 120),
  field_type text NOT NULL CHECK (field_type IN ('text','long_text','integer','decimal','date','date_time','boolean','choice','multi_choice','reference')),
  required boolean NOT NULL DEFAULT false,
  help_text text,
  position integer NOT NULL DEFAULT 0,
  options jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(options)='array'),
  reference_table text,
  text_limit integer,
  format text,
  archived_at timestamptz,
  version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, record_type, field_key)
);
CREATE INDEX base_custom_field_definitions_live
  ON base_custom_field_definitions(tenant_id, record_type, position, field_key)
  WHERE archived_at IS NULL;
