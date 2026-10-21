-- One counter of failed credential checks per account and kind, so sign-in
-- and two-factor share one table and one window/lock rule
-- (`src/credential_attempts.rs`) while keeping separate counts: guessing at
-- the sign-in form cannot block the signed-in owner's own two-factor checks.
-- `step_up`: the existing two-factor checks (password re-entry, authenticator
--   code), unchanged in behaviour.
-- `sign_in`: wrong passwords on the sign-in form.
-- `locks`: locks in a row for this kind; sign-in's lock grows with it and a
--   successful check forgets it.
ALTER TABLE user_two_factor_attempts RENAME TO user_credential_attempts;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_pkey TO user_credential_attempts_pkey;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_user_fkey TO user_credential_attempts_user_fkey;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_failures_check TO user_credential_attempts_failures_check;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_tenant_id_not_null TO user_credential_attempts_tenant_id_not_null;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_user_id_not_null TO user_credential_attempts_user_id_not_null;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_failures_not_null TO user_credential_attempts_failures_not_null;
ALTER TABLE user_credential_attempts RENAME CONSTRAINT user_two_factor_attempts_window_started_at_not_null TO user_credential_attempts_window_started_at_not_null;
ALTER TABLE user_credential_attempts
 ADD COLUMN kind text NOT NULL DEFAULT 'step_up'
  CONSTRAINT user_credential_attempts_kind_check CHECK (kind IN ('sign_in', 'step_up')),
 ADD COLUMN locks integer NOT NULL DEFAULT 0
  CONSTRAINT user_credential_attempts_locks_check CHECK (locks >= 0);
ALTER TABLE user_credential_attempts DROP CONSTRAINT user_credential_attempts_pkey,
 ADD CONSTRAINT user_credential_attempts_pkey PRIMARY KEY (tenant_id, user_id, kind);
