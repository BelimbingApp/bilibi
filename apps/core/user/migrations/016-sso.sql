-- Optional external identity, no Bilimbi equivalent. Credentials and tokens
-- remain outside audit values; configuration itself lives in encrypted Settings.
CREATE TABLE user_identity_providers (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 company_id bigint NOT NULL,
 public_id text NOT NULL UNIQUE,
 configured_by bigint NOT NULL,
 method text NOT NULL,
 enabled boolean NOT NULL DEFAULT false,
 -- Plain, non-secret copies of the two password-policy choices, so sign-in applies them
 -- even when the sealed configuration cannot be opened.
 enforce_sso boolean NOT NULL DEFAULT false,
 break_glass_user bigint,
 version bigint NOT NULL DEFAULT 1,
 FOREIGN KEY(tenant_id,company_id) REFERENCES companies(tenant_id,id),
 FOREIGN KEY(tenant_id,configured_by) REFERENCES users(tenant_id,id),
 UNIQUE(tenant_id,id)
);
CREATE TABLE user_external_identities (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 company_id bigint NOT NULL,
 user_id bigint NOT NULL,
 issuer text NOT NULL,
 subject text NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 FOREIGN KEY(tenant_id,provider_id) REFERENCES user_identity_providers(tenant_id,id),
 FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE,
 UNIQUE(tenant_id,company_id,issuer,subject),
 UNIQUE(tenant_id,provider_id,user_id)
);
CREATE TABLE user_identity_flows (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 provider_version bigint NOT NULL,
 state_hash bytea NOT NULL UNIQUE,
 browser_hash bytea NOT NULL,
 sealed_request jsonb NOT NULL,
 user_id bigint,
 session_csrf_hash bytea,
 purpose text NOT NULL CHECK(purpose IN ('sign_in','link','step_up')),
 return_to text NOT NULL,
 state text NOT NULL DEFAULT 'pending',
 expires_at timestamptz NOT NULL,
 FOREIGN KEY(tenant_id,provider_id) REFERENCES user_identity_providers(tenant_id,id)
);
CREATE TABLE user_external_sessions (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 user_id bigint NOT NULL,
 subject text NOT NULL,
 provider_sid text,
 session_hash bytea NOT NULL,
 logout_hint jsonb,
 FOREIGN KEY(tenant_id,provider_id) REFERENCES user_identity_providers(tenant_id,id),
 FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
ALTER TABLE user_sign_ins ADD COLUMN identity_provider_id bigint, ADD COLUMN external_session_id bigint;
CREATE TABLE user_identity_logout_replays (
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 replay_hash bytea NOT NULL,
 expires_at timestamptz NOT NULL,
 PRIMARY KEY(tenant_id,provider_id,replay_hash)
);
CREATE TABLE user_identity_proofs (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 user_id bigint NOT NULL,
 proof_hash bytea NOT NULL UNIQUE,
 operation text NOT NULL,
 target text NOT NULL,
 channel text NOT NULL,
 expires_at timestamptz NOT NULL
);
CREATE TABLE user_identity_challenges (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 user_id bigint NOT NULL,
 operation text NOT NULL,
 target text NOT NULL,
 channel text NOT NULL,
 expires_at timestamptz NOT NULL,
 started_at timestamptz NOT NULL DEFAULT now(),
 consumed boolean NOT NULL DEFAULT false
);
ALTER TABLE user_identity_flows ADD COLUMN challenge_id bigint;
ALTER TABLE authz_identity_policies ADD CONSTRAINT authz_identity_company_fk
 FOREIGN KEY(tenant_id,company_id) REFERENCES companies(tenant_id,id);
ALTER TABLE authz_identity_roles ADD CONSTRAINT authz_identity_user_fk
 FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE;
ALTER TABLE user_external_identities ADD COLUMN provisioned boolean NOT NULL DEFAULT false;
