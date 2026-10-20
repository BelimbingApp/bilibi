-- The account's mobile number for SMS one-time codes (Base SMS), in E.164
-- (`+` and 8 to 15 digits). An administrator may enter it; only its holder
-- verifies it, by entering a code sent to it. A verified number is the SMS
-- second factor being on; changing the number clears the verification.
ALTER TABLE users ADD COLUMN phone_number text
 CHECK (phone_number ~ '^\+[1-9][0-9]{7,14}$'),
 ADD COLUMN phone_verified_at timestamptz,
 ADD CONSTRAINT users_phone_verified_needs_number
 CHECK (phone_verified_at IS NULL OR phone_number IS NOT NULL);
