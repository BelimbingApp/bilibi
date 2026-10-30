-- Global search (Base Search): the contact's name as a stored text-search
-- vector, which changes in the same transaction as the row, and prefix
-- indexes for the restrictable email and phone a reader may also match on.
-- The `simple` configuration does not stem: names, codes and emails in mixed
-- languages are matched as written.
ALTER TABLE persons ADD COLUMN search_vector tsvector
    GENERATED ALWAYS AS (setweight(to_tsvector('simple', name || ' ' || code), 'A')) STORED;
CREATE INDEX persons_search ON persons USING gin (search_vector);
CREATE INDEX persons_email_prefix ON persons (tenant_id, lower(email) text_pattern_ops) WHERE email IS NOT NULL;
CREATE INDEX persons_phone_prefix ON persons (tenant_id, lower(phone) text_pattern_ops) WHERE phone IS NOT NULL;
