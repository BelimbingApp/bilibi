-- A session waiting for its sign-in second factor (Core User's sign-in
-- check, Base Step-up operation `core.user.sign_in`). While set, the session
-- authenticates nothing: every check refuses it, and only the sign-in pages
-- read it through `Sessions::pending`.
-- `code`: the holder owes a current authenticator or recovery code.
-- `setup`: the holder has no authenticator yet and must set one up first.
ALTER TABLE sessions ADD COLUMN second_factor text
 CHECK (second_factor IN ('code', 'setup'));
