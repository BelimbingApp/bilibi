-- The connection's configuration and client secret, sealed with the installation key
-- and bound to its tenant and record. Sign-in reads it with no administrator's
-- session, so offboarding whoever saved it cannot lock a company out.
ALTER TABLE user_identity_providers ADD COLUMN sealed_runtime jsonb;
