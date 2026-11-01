-- OAuth connections are approved once. A connection is usable only while the
-- exact browser session that switched it on remains live.
DELETE FROM core_agent_connections;
ALTER TABLE core_agent_connections
 ADD COLUMN enabled_session_id text REFERENCES sessions(id) ON DELETE SET NULL,
 ADD COLUMN approved_client varchar(512) NOT NULL DEFAULT 'Browser',
 ALTER COLUMN expires_at SET DEFAULT 'infinity'::timestamptz;
DROP INDEX IF EXISTS core_agent_connections_tenant_user_idx;
CREATE INDEX core_agent_connections_tenant_user_idx ON core_agent_connections(tenant_id,user_id,enabled_session_id);
DELETE FROM base_settings WHERE key='agent_access.connection_days';
