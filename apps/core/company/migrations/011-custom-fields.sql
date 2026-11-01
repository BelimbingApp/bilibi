-- Tenant-defined custom fields (`base/custom_fields`): the values a company
-- holds, keyed by the stable key of each definition. Company owns the column
-- and checks every value against the tenant's definitions inside its own
-- write, so the tenant predicate, the soft delete and the version check
-- cover the values and one audited change records them with the row.
ALTER TABLE companies
 ADD COLUMN custom_fields jsonb NOT NULL DEFAULT '{}'::jsonb,
 ADD CONSTRAINT companies_custom_fields_object CHECK (jsonb_typeof(custom_fields) = 'object');
CREATE INDEX companies_custom_fields_idx ON companies USING gin (custom_fields jsonb_path_ops);
