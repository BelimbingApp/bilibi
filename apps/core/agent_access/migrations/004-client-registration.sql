-- Public, pre-authentication OAuth lifecycle receipts, like device receipts.
-- No person or tenant is asserted until consent creates a grant.
CREATE TABLE core_agent_oauth_clients (
 client_id text PRIMARY KEY,
 metadata jsonb NOT NULL,
 created_at timestamptz NOT NULL DEFAULT now(),
 last_used_at timestamptz NOT NULL DEFAULT now(),
 metadata_expires_at timestamptz
);
CREATE INDEX core_agent_oauth_clients_unused_idx ON core_agent_oauth_clients(last_used_at);
CREATE INDEX core_agent_connections_client_idx ON core_agent_connections(client_id);
-- Existing grants carry their client binding and remain valid until expiry.
-- New authorizations must register; no legacy client allowlist survives.
DELETE FROM base_settings WHERE key='agent_access.oauth_clients';
ALTER TABLE core_agent_connections ALTER COLUMN client_id TYPE text;
