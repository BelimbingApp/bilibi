-- ADR 01 "Agent identity at runtime": the distinct self-reported
-- harness/model-version-effort values the connection's agent sent, each with
-- the UTC time it was first seen, newest 20 kept (like seen_agent_ids). What
-- the agent says it is, shown labelled self-reporting; never read for
-- authorization.
ALTER TABLE core_agent_connections ADD COLUMN self_reported_agents jsonb NOT NULL DEFAULT '{}'::jsonb;
