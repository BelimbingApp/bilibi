-- Provider access tokens share ordinary agent standing and step-up windows.
-- Only a digest survives; no external JWT or claims are retained.
ALTER TABLE core_agent_connections
 ADD COLUMN provider_public text,
 ADD COLUMN provider_version bigint,
 ADD COLUMN provider_link bigint,
 ADD COLUMN provider_token_hash bytea UNIQUE,
 ADD CONSTRAINT provider_access_binding CHECK (
  (provider_public IS NULL AND provider_version IS NULL AND provider_link IS NULL AND provider_token_hash IS NULL)
  OR (provider_public IS NOT NULL AND provider_version IS NOT NULL AND provider_link IS NOT NULL AND octet_length(provider_token_hash)=32 AND provider_token_hash IS NOT NULL)
 );
