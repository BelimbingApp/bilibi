-- Neutral sign-in admission; no Bilimbi counterpart for pending identity.
CREATE TABLE user_sign_ins (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    user_id bigint NOT NULL,
    token_hash bytea NOT NULL UNIQUE,
    csrf text NOT NULL,
    credential_hash bytea NOT NULL,
    state text NOT NULL CHECK (state IN ('pending','admitted','cancelled','expired','revoked')),
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE INDEX user_sign_ins_pending ON user_sign_ins(tenant_id,user_id) WHERE state='pending';
