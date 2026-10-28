-- A person's own agent activity and each connection's last use
-- (`own_agent_activity`, `own_agent_last_use`) read only their incoming
-- requests made under a connection, on every status bar render.
CREATE INDEX base_integration_exchanges_agent_activity_idx
 ON base_integration_exchanges (tenant_id, user_id, connection_id, occurred_at DESC)
 WHERE direction = 'incoming' AND connection_id IS NOT NULL;
