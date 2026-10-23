ALTER TABLE companies
  ADD COLUMN custom jsonb NOT NULL DEFAULT '{}'::jsonb
    CHECK (jsonb_typeof(custom) = 'object');
CREATE INDEX companies_custom_gin ON companies USING gin (custom jsonb_path_ops);
