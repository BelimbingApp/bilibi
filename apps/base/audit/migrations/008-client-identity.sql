-- Transport evidence, separate from the agent self-report; no authorization meaning.
ALTER TABLE base_audit_mutations ADD COLUMN client_identity text CHECK (length(client_identity) BETWEEN 1 AND 255);
ALTER TABLE base_audit_actions ADD COLUMN client_identity text CHECK (length(client_identity) BETWEEN 1 AND 255);
