-- Text-message (SMS) two-factor beside the authenticator app. One phone per
-- account, E.164. An administrator may set or change it (`verified_at` is then
-- NULL); it becomes a second factor only once the holder accepted a code sent
-- to it (`verified_at`).
CREATE TABLE user_phones (
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 phone_number text NOT NULL CHECK (phone_number ~ '^\+[1-9][0-9]{7,14}$'),
 verified_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT user_phones_pkey PRIMARY KEY(tenant_id,user_id),
 CONSTRAINT user_phones_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);

-- Codes sent by text message, Argon2id hashes only. A code is for one
-- purpose (`verify_phone`: proving a number; `check`: a sign-in or step-up
-- check) and the number it went to. It works once (`used_at`), until
-- `expires_at` (five minutes), and five wrong tries end it. Sending a new
-- code ends the earlier ones. Rows stay for the resend limits.
CREATE TABLE user_sms_codes (
 id bigint GENERATED ALWAYS AS IDENTITY,
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 purpose text NOT NULL CHECK (purpose IN ('verify_phone','check')),
 phone_number text NOT NULL CHECK (phone_number ~ '^\+[1-9][0-9]{7,14}$'),
 code_hash text NOT NULL,
 sent_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL,
 failures integer NOT NULL DEFAULT 0 CHECK (failures BETWEEN 0 AND 5),
 used_at timestamptz,
 CONSTRAINT user_sms_codes_pkey PRIMARY KEY(id),
 CONSTRAINT user_sms_codes_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE INDEX user_sms_codes_owner_idx ON user_sms_codes(tenant_id,user_id,sent_at DESC);

-- A sign-in that passed the password and still owes a code: the browser
-- holds a random token (only its SHA-256 is stored) for ten minutes. No
-- session exists until the code is accepted.
CREATE TABLE user_pending_sign_ins (
 token_hash text NOT NULL,
 tenant_id bigint NOT NULL, user_id bigint NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 expires_at timestamptz NOT NULL,
 CONSTRAINT user_pending_sign_ins_pkey PRIMARY KEY(token_hash),
 CONSTRAINT user_pending_sign_ins_user_fkey FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id) ON DELETE CASCADE
);
CREATE INDEX user_pending_sign_ins_expiry_idx ON user_pending_sign_ins(expires_at);
