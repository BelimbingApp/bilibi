-- The holder session id outlives the session row, so ending a session can
-- still be traced to the connections it held.
ALTER TABLE core_agent_connections DROP CONSTRAINT IF EXISTS core_agent_connections_enabled_session_id_fkey;
CREATE INDEX core_agent_connections_enabled_session_idx ON core_agent_connections(enabled_session_id) WHERE enabled_session_id IS NOT NULL;
