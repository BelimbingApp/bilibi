-- Record redirects (`base/records`): after an owner merges two records, a
-- reference to the merged-away id points at the survivor. The owner writes
-- the row in its own merge transaction; every holder repoints on read, so no
-- holder's row is rewritten. Chains are collapsed on write: `to_id` is never
-- itself redirected.
CREATE TABLE base_records_redirects (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 kind text NOT NULL,
 from_id text NOT NULL,
 to_id text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT base_records_redirects_from_key UNIQUE (tenant_id, kind, from_id),
 CONSTRAINT base_records_redirects_differ CHECK (from_id <> to_id)
);
CREATE INDEX base_records_redirects_to_idx ON base_records_redirects(tenant_id, kind, to_id);
