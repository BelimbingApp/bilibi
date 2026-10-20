-- SMS one-time codes (Core User two-factor). One row per code sent: only an
-- Argon2id hash is kept, never the code. A code works once, for its purpose,
-- until `expires_at` (5 minutes), and dies after 5 wrong attempts. `sent_at`
-- also bounds resending (once a minute, five an hour per account).
-- `purpose`: `verify` (proving a number before SMS is turned on) or `check`
-- (sign-in and step-up).
-- `phone_number`: the number the code went to; a code proves only that number.
CREATE TABLE user_sms_codes (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL,
    user_id bigint NOT NULL,
    purpose text NOT NULL CHECK (purpose IN ('verify', 'check')),
    phone_number text NOT NULL,
    code_hash text NOT NULL,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    sent_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz NOT NULL,
    used_at timestamptz,
    FOREIGN KEY (tenant_id, user_id) REFERENCES users (tenant_id, id) ON DELETE CASCADE
);
CREATE INDEX user_sms_codes_account ON user_sms_codes (tenant_id, user_id, sent_at DESC);
