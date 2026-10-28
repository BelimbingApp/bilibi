-- Core Company contributes the scoped foreign key behind Base Session's bare
-- `sessions.company_id` (migration `base-session-004-company`): the company a
-- session chose to work in is a company of the session's own tenant, and a
-- company is never deleted from under a session that works in it. Reach is
-- proved on every authentication, never by this key.
ALTER TABLE sessions ADD CONSTRAINT sessions_scoped_company_fk
  FOREIGN KEY (tenant_id, company_id) REFERENCES companies(tenant_id, id) ON DELETE RESTRICT;
