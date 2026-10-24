-- Global search (Base Search): the company's name, code and legal name as a
-- stored text-search vector, which changes in the same transaction as the row.
-- The name and code rank above the legal name. The `simple` configuration
-- does not stem: names and codes in mixed languages are matched as written.
ALTER TABLE companies ADD COLUMN search_vector tsvector
    GENERATED ALWAYS AS (
        setweight(to_tsvector('simple', name), 'A')
        || setweight(to_tsvector('simple', coalesce(code, '')), 'A')
        || setweight(to_tsvector('simple', coalesce(legal_name, '')), 'B')
    ) STORED;
CREATE INDEX companies_search ON companies USING gin (search_vector);
