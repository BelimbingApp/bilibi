-- ADR 01 "Agent identity at runtime": the agent's self-reported
-- harness/model-version-effort, recorded beside the actor it never replaces.
-- Self-reporting evidence only; nothing reads it for authorization.
ALTER TABLE base_audit_mutations ADD COLUMN agent_self_report text CHECK (length(agent_self_report) BETWEEN 1 AND 120);
ALTER TABLE base_audit_actions ADD COLUMN agent_self_report text CHECK (length(agent_self_report) BETWEEN 1 AND 120);
