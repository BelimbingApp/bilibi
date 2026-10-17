-- When this session last passed an authenticator check (step-up, GitHub's
-- "sudo mode"). Bound to the row, so it ends with the session: sign-out,
-- termination, expiry. Null until the first check; an identity switch
-- (impersonation) clears it.
ALTER TABLE sessions ADD COLUMN stepped_up_at timestamptz;
