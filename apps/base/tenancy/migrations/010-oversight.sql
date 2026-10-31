-- Child-owned consent; hierarchy alone never authorizes a read.
CREATE TABLE base_tenancy_oversight (
    tenant_id bigint PRIMARY KEY REFERENCES tenants(id),
    parent_id bigint NOT NULL REFERENCES tenants(id),
    active boolean NOT NULL DEFAULT false,
    expires_at timestamptz,
    version bigint NOT NULL DEFAULT 1 CHECK (version > 0),
    tables jsonb NOT NULL CHECK (jsonb_typeof(tables) = 'object'),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CHECK (tenant_id <> parent_id)
);
CREATE INDEX base_tenancy_oversight_parent ON base_tenancy_oversight(parent_id, tenant_id) WHERE active;
