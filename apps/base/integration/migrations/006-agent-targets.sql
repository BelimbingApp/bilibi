-- What an agent request operated on, recorded when the request is made so the
-- person's AI panel can say "Read 12 companies" or "Saved a change to Company
-- Acme" (`own_agent_activity`). A JSON list with one entry per operation:
-- {"operation", "kind", "count", "id"}. Record kinds and ids only, never names
-- or values: a name is looked up when it is shown, with the reader's own
-- access. Rows recorded before this migration have an empty list.
ALTER TABLE base_integration_exchanges
 ADD COLUMN agent_targets jsonb NOT NULL DEFAULT '[]'::jsonb
 CONSTRAINT base_integration_exchanges_agent_targets_check CHECK (jsonb_typeof(agent_targets) = 'array' AND jsonb_array_length(agent_targets) <= 20);
