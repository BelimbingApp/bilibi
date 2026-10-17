-- Authenticator-app two-factor (TOTP, RFC 6238) per account. `secret` is the
-- AES-256-GCM envelope Settings seals with the installation key
-- (`Settings::seal_owned`), never plaintext. A row with no `confirmed_at` is
-- a pending enrollment: it proves nothing until a current code confirms it.
-- `last_step` is the last accepted 30-second step, so a code is never
-- accepted twice.
CREATE TABLE user_two_factor (
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 secret jsonb NOT NULL,
 confirmed_at timestamptz,
 last_step bigint,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT user_two_factor_pkey PRIMARY KEY(tenant_id,user_id),
 CONSTRAINT user_two_factor_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);

-- One-time recovery codes, Argon2id hashes only (the password hasher). A
-- regeneration replaces the whole set; a used code keeps its row with
-- `used_at` so the page can count what is left.
CREATE TABLE user_two_factor_recovery_codes (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 code_hash text NOT NULL,
 used_at timestamptz,
 CONSTRAINT user_two_factor_recovery_codes_pkey PRIMARY KEY(id),
 CONSTRAINT user_two_factor_recovery_codes_owner_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES user_two_factor(tenant_id,user_id) ON DELETE CASCADE
);
CREATE INDEX user_two_factor_recovery_codes_owner_idx ON user_two_factor_recovery_codes(tenant_id,user_id);

-- Failed checks per account (password re-entry, authenticator or recovery
-- code), counted in a window; reaching the limit locks every check of that
-- account until `locked_until`. Kept apart from `user_two_factor` because the
-- password check before enrollment is throttled too.
CREATE TABLE user_two_factor_attempts (
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 failures integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
 window_started_at timestamptz NOT NULL DEFAULT now(),
 locked_until timestamptz,
 CONSTRAINT user_two_factor_attempts_pkey PRIMARY KEY(tenant_id,user_id),
 CONSTRAINT user_two_factor_attempts_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
