-- Base Authz owns assignments; Core Company supplies the scoped owner key.
ALTER TABLE principal_roles ADD CONSTRAINT principal_roles_company_fk
 FOREIGN KEY(tenant_id,company_id) REFERENCES companies(tenant_id,id);
