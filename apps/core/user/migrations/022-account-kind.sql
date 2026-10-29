-- Issue 137, ADR 15: an account is staff or a customer of its tenant. Every
-- existing account is staff. A customer always names a Person and names an
-- affiliation (and so a company) only when it signs in for a business; a
-- private individual has no company.
ALTER TABLE users
 ADD COLUMN kind text NOT NULL DEFAULT 'staff'
  CONSTRAINT users_kind_check CHECK (kind IN ('staff', 'customer')),
 ADD CONSTRAINT users_customer_needs_person CHECK (kind = 'staff' OR person_id IS NOT NULL);

-- Staff sign in by email alone, so a staff address stays unique across the
-- installation. A customer signs in at its tenant's portal address, so its
-- address is unique among that tenant's customers only, and the same person
-- may be a customer of two tenants without either learning of the other.
DROP INDEX users_login_email_global_idx;
CREATE UNIQUE INDEX users_login_email_global_idx ON users(lower(email)) WHERE deleted_at IS NULL AND kind = 'staff';
CREATE UNIQUE INDEX users_customer_email_idx ON users(tenant_id, lower(email)) WHERE deleted_at IS NULL AND kind = 'customer';
ALTER TABLE users DROP CONSTRAINT users_tenant_id_email_key;
CREATE INDEX users_kind_idx ON users(tenant_id, kind) WHERE deleted_at IS NULL;
