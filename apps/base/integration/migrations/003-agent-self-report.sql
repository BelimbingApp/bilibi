-- ADR 01 "Agent identity at runtime": the self-reported
-- harness/model-version-effort an agent sent with the request. A value that
-- failed the shape check is dropped and only the fact that it was malformed
-- is kept. Self-reporting evidence; never read for authorization.
ALTER TABLE base_integration_exchanges
 ADD COLUMN agent_self_report text CHECK (length(agent_self_report) BETWEEN 1 AND 120),
 ADD COLUMN agent_self_report_malformed boolean NOT NULL DEFAULT false,
 ADD CONSTRAINT base_integration_exchanges_self_report_check CHECK (NOT (agent_self_report_malformed AND agent_self_report IS NOT NULL));
