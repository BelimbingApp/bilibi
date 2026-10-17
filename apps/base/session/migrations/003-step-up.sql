-- Step-up on this session (Base Step-up's web proof; GitHub's "sudo mode").
-- Bound to the row, so it ends with the session: sign-out, termination,
-- expiry; an identity switch (impersonation) clears it.
-- `stepped_up_at`: when the account holder last passed a check here (the
--   absolute cap counts from it).
-- `step_up_used_at`: the last step-up action the window covered (the sliding
--   window counts from it).
-- `step_up_spent_at`: when an "ask every time" action used the last check, so
--   one check covers one such action.
ALTER TABLE sessions ADD COLUMN stepped_up_at timestamptz,
 ADD COLUMN step_up_used_at timestamptz,
 ADD COLUMN step_up_spent_at timestamptz;
