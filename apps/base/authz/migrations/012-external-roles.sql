-- Provider policies and grants are owned here, never written by an identity method.
CREATE TABLE authz_identity_policies (
 tenant_id bigint NOT NULL REFERENCES tenants(id),
 source text NOT NULL,
 company_id bigint NOT NULL,
 role_ids bigint[] NOT NULL,
 PRIMARY KEY(tenant_id,source)
);
CREATE TABLE authz_identity_roles (
 tenant_id bigint NOT NULL,
 source text NOT NULL,
 user_id bigint NOT NULL,
 role_id bigint NOT NULL,
 PRIMARY KEY(tenant_id,source,user_id,role_id),
 FOREIGN KEY(tenant_id,source) REFERENCES authz_identity_policies(tenant_id,source),
 FOREIGN KEY(tenant_id,role_id) REFERENCES roles(tenant_id,id) ON DELETE CASCADE
);
CREATE VIEW effective_principal_roles AS
 SELECT tenant_id,user_id,role_id FROM principal_roles
 UNION SELECT tenant_id,user_id,role_id FROM authz_identity_roles;
