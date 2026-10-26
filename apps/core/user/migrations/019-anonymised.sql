-- Personal-data erasure and retention (ADR 05): when an account was
-- anonymised. A sweep acts on closed accounts with no mark; a name that
-- merely reads "[erased]" is not one.
ALTER TABLE users ADD COLUMN anonymised_at timestamptz;
CREATE INDEX users_closed_unanonymised ON users(tenant_id, id)
    WHERE status = 'deleted' AND anonymised_at IS NULL;
