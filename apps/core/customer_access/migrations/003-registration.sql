-- Issue 137, behaviour 7: a tenant may let people ask for a customer
-- account. Off by default. A request is verified by an emailed link, then
-- waits for staff, who approve it (which sends an invitation to the Person
-- they choose) or reject it. Nothing is written to Person or Company until
-- staff act. An unverified request expires; a verified one waits.
ALTER TABLE customer_portal_sites ADD COLUMN registration boolean NOT NULL DEFAULT false;

CREATE TABLE customer_registrations (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 255),
 email text NOT NULL CHECK (octet_length(email) BETWEEN 3 AND 254 AND email = lower(email)),
 company_name text CHECK (company_name IS NULL OR length(btrim(company_name)) BETWEEN 1 AND 160),
 token_hash bytea NOT NULL,
 expires_at timestamptz NOT NULL,
 verified_at timestamptz,
 decision text CHECK (decision IN ('approved', 'rejected')),
 decided_at timestamptz,
 decided_by bigint,
 invitation_id bigint,
 created_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT customer_registrations_decided CHECK ((decision IS NULL) = (decided_at IS NULL)),
 CONSTRAINT customer_registrations_verified_first CHECK (decision IS NULL OR verified_at IS NOT NULL),
 CONSTRAINT customer_registrations_approved_invites CHECK ((decision = 'approved') = (invitation_id IS NOT NULL))
);
CREATE UNIQUE INDEX customer_registrations_token_idx ON customer_registrations(token_hash);
CREATE UNIQUE INDEX customer_registrations_open_idx ON customer_registrations(tenant_id, email) WHERE decision IS NULL;
CREATE INDEX customer_registrations_tenant_idx ON customer_registrations(tenant_id, id DESC);
