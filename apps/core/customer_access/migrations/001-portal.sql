-- Core Customer Access (issue 137, ADR 16): each tenant's portal site and the
-- invitations that turn a Person into a customer account.
--
-- A site's address is the segment a customer's portal links carry
-- (`/portal/sign-in/{address}`). It names the tenant before anyone is signed
-- in, so it is unique across the installation. One site per tenant.
CREATE TABLE customer_portal_sites (
 tenant_id bigint PRIMARY KEY REFERENCES tenants(id),
 address text NOT NULL CHECK (address ~ '^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$'),
 title text NOT NULL CHECK (length(btrim(title)) BETWEEN 1 AND 120),
 version bigint NOT NULL DEFAULT 1,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX customer_portal_sites_address_idx ON customer_portal_sites(address);

-- An invitation names the Person the customer will sign in as and, for a
-- business customer, the affiliation (and so the company) it signs in
-- through; `name` is the account name the staff member proposed. Its token is
-- stored hashed and redeemed once. At most one invitation per address is
-- open; inviting the address again replaces it.
CREATE TABLE customer_invitations (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 email text NOT NULL CHECK (octet_length(email) BETWEEN 3 AND 254 AND email = lower(email)),
 name text NOT NULL CHECK (length(btrim(name)) BETWEEN 1 AND 255),
 person_id bigint NOT NULL,
 affiliation_id bigint,
 organisation_id bigint,
 token_hash bytea NOT NULL,
 invited_by bigint NOT NULL,
 expires_at timestamptz NOT NULL,
 accepted_at timestamptz,
 user_id bigint,
 revoked_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT customer_invitations_company CHECK ((affiliation_id IS NULL) = (organisation_id IS NULL)),
 CONSTRAINT customer_invitations_closed_once CHECK (accepted_at IS NULL OR revoked_at IS NULL),
 CONSTRAINT customer_invitations_accepted_user CHECK ((accepted_at IS NULL) = (user_id IS NULL))
);
CREATE UNIQUE INDEX customer_invitations_token_idx ON customer_invitations(token_hash);
CREATE UNIQUE INDEX customer_invitations_open_idx ON customer_invitations(tenant_id, email) WHERE accepted_at IS NULL AND revoked_at IS NULL;
CREATE INDEX customer_invitations_tenant_idx ON customer_invitations(tenant_id, id DESC);
