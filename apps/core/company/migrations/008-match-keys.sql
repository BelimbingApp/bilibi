-- Company-owned match keys; extends Bilimbi Core.Company's identity lookup without extensions.
ALTER TABLE companies
    ADD COLUMN name_tokens text[] GENERATED ALWAYS AS (regexp_split_to_array(lower(name), '[^[:alnum:]]+')) STORED,
    ADD COLUMN website_host text GENERATED ALWAYS AS (regexp_replace(regexp_replace(lower(btrim(COALESCE(website,''))), '^([a-z][a-z0-9+.-]*://)?(www\.)?', ''), '[/:?#].*$', '')) STORED,
    ADD COLUMN email_domain text GENERATED ALWAYS AS (lower(split_part(email, '@', 2))) STORED,
    ADD COLUMN phone_digits text GENERATED ALWAYS AS (regexp_replace(phone, '[^0-9]', '', 'g')) STORED,
    ADD COLUMN registration_key text GENERATED ALWAYS AS (upper(regexp_replace(COALESCE(registration_number,''), '[^[:alnum:]]', '', 'g'))) STORED;
CREATE INDEX companies_name_tokens ON companies USING gin(name_tokens) WHERE deleted_at IS NULL;
CREATE INDEX companies_website_host ON companies(tenant_id,website_host) WHERE deleted_at IS NULL AND website_host <> '';
CREATE INDEX companies_email_domain ON companies(tenant_id,email_domain) WHERE deleted_at IS NULL AND email_domain <> '';
CREATE INDEX companies_phone_digits ON companies(tenant_id,phone_digits) WHERE deleted_at IS NULL AND length(phone_digits) >= 7;
CREATE INDEX companies_registration_key ON companies(tenant_id,registration_key) WHERE deleted_at IS NULL AND registration_key <> '';
