-- Transport client labels, separate from agent self-reports.
ALTER TABLE core_agent_connections ADD COLUMN observed_clients jsonb NOT NULL DEFAULT '{}'::jsonb;
