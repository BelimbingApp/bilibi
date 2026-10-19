-- The account's phone number for text-message two-factor, in E.164 form
-- (`+94771234567`), or NULL. An administrator sets it; the first code sent
-- to it and entered correctly records `phone_verified_at`, and a change of
-- number clears that again.
ALTER TABLE users ADD COLUMN phone_number text,
 ADD COLUMN phone_verified_at timestamptz,
 ADD CONSTRAINT users_phone_number_e164 CHECK (phone_number ~ '^\+[1-9][0-9]{6,14}$');

-- One row per text-message code sent: the Argon2id hash only, never the
-- code. A code works until `expires_at` (5 minutes), once (`used_at`), and
-- until it has counted its fifth wrong attempt; a newer code or a change of
-- number voids it (`voided_at`). The rows also count recent sends for the
-- resend throttle; rows older than a day are deleted when the account is
-- sent its next code.
CREATE TABLE user_two_factor_sms_codes (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 code_hash text NOT NULL,
 sent_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL,
 failures integer NOT NULL DEFAULT 0 CHECK (failures >= 0),
 used_at timestamptz,
 voided_at timestamptz,
 CONSTRAINT user_two_factor_sms_codes_pkey PRIMARY KEY(id),
 CONSTRAINT user_two_factor_sms_codes_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE INDEX user_two_factor_sms_codes_owner_idx ON user_two_factor_sms_codes(tenant_id,user_id,sent_at DESC);
