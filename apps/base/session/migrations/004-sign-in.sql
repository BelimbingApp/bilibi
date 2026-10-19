-- A sign-in still waiting for its holder's second factor (Core User's
-- sign-in step-up). Every guard refuses such a row except the code page's
-- (`Sessions::pending`); a check on it (`Sessions::step_up`) or a tenant
-- policy that asks for none (`Sessions::complete_sign_in`) makes it an
-- ordinary session. Its short `expires_at` is set at sign-in, so an
-- abandoned one ends on its own.
ALTER TABLE sessions ADD COLUMN second_factor_pending boolean NOT NULL DEFAULT false;
