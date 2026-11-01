-- Personal-data erasure and retention (ADR 05), after desk/account migrations: when an account was
-- anonymised. A sweep acts on closed accounts with no mark; a name that
-- merely reads "[erased]" is not one.
ALTER TABLE users ADD COLUMN anonymised_at timestamptz;
CREATE INDEX users_closed_unanonymised ON users(tenant_id, id)
    WHERE status = 'deleted' AND anonymised_at IS NULL;
INSERT INTO base_personal_data_kinds(kind, module, fields, subject_identifier, person)
    VALUES ('user', 'core/user', ARRAY['name', 'email'], false, false),
           ('notification', 'core/user', ARRAY['title', 'body'], false, false),
           ('user.scim_user', 'core/user', ARRAY['*'], false, false),
           ('user.external_identity', 'core/user', ARRAY['subject'], false, false),
           ('user.bootstrap', 'core/user', ARRAY['admin_email'], false, false),
           ('user_two_factor_reset', 'core/user', ARRAY['name', 'email'], false, false);
