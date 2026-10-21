-- A session a correct password created while its second factor is still
-- owed (sign-in step-up, Core User's `core.user.sign_in`). It proves nothing:
-- `Sessions::resolve` refuses it everywhere, so no page, socket, API call or
-- download accepts it. Only `Sessions::pending` reads it, for the code page,
-- and `Sessions::admit` clears the flag once the code was right. Core User
-- writes every new session pending and admits it when no check applies.
ALTER TABLE sessions ADD COLUMN second_factor_pending boolean NOT NULL DEFAULT false;
