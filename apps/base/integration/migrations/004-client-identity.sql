-- Transport client evidence, not an agent self-report.
ALTER TABLE base_integration_exchanges ADD COLUMN client_identity text CHECK (length(client_identity) BETWEEN 1 AND 255);
