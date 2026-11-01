-- Issue 147, part 2: a tenant may prepare an account before the invited
-- address has proved or created its platform sign-in.
ALTER TABLE users ALTER COLUMN identity_id DROP NOT NULL;

CREATE TABLE user_invitations (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 user_id bigint NOT NULL,
 email text NOT NULL CHECK (octet_length(email) BETWEEN 3 AND 254 AND email=lower(email)),
 token_hash bytea NOT NULL,
 invited_by bigint NOT NULL,
 expires_at timestamptz NOT NULL,
 accepted_at timestamptz,
 revoked_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id),
 FOREIGN KEY(tenant_id,invited_by) REFERENCES users(tenant_id,id),
 CONSTRAINT user_invitations_closed_once CHECK (accepted_at IS NULL OR revoked_at IS NULL)
);
CREATE UNIQUE INDEX user_invitations_token_idx ON user_invitations(token_hash);
CREATE UNIQUE INDEX user_invitations_open_email_idx ON user_invitations(tenant_id,email)
 WHERE accepted_at IS NULL AND revoked_at IS NULL;
CREATE INDEX user_invitations_tenant_idx ON user_invitations(tenant_id,id DESC);

ALTER TABLE users ADD CONSTRAINT users_identity_state_check CHECK (
 (status='invited' AND identity_id IS NULL AND deleted_at IS NULL) OR
 (status='deleted' AND identity_id IS NULL AND deleted_at IS NOT NULL) OR
 (status<>'invited' AND identity_id IS NOT NULL)
);
