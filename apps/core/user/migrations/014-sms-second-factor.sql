-- SMS as a second factor (ADR 03). The account's own phone, E.164, set by an
-- administrator; `phone_verified_at` is when the holder last proved it with a
-- code sent to it. A changed number is unverified again.
ALTER TABLE users ADD COLUMN phone_number text, ADD COLUMN phone_verified_at timestamptz,
 ADD CONSTRAINT users_phone_number_e164 CHECK (phone_number IS NULL OR phone_number ~ '^\+[1-9][0-9]{7,14}$');

-- One row per code sent. `code_hash` is an Argon2id hash (the password
-- hasher); the plain code exists only in the message. A newer code
-- supersedes older ones (`spent_at`), five wrong tries void it, and it
-- expires after five minutes. Rows also count sends for the resend throttle
-- (60 seconds apart, five an hour); rows older than a day are deleted when
-- the next code is sent.
CREATE TABLE user_sms_codes (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 purpose text NOT NULL CHECK (purpose IN ('check','verify_phone')),
 phone_number text NOT NULL,
 code_hash text NOT NULL,
 attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
 sent_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL,
 spent_at timestamptz,
 CONSTRAINT user_sms_codes_pkey PRIMARY KEY(id),
 CONSTRAINT user_sms_codes_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE INDEX user_sms_codes_owner_idx ON user_sms_codes(tenant_id,user_id,sent_at DESC);
