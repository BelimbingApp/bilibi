-- Company and UTC calendar dates qualify the assignment, never the role's
-- administrative owner. Live reads use clock_timestamp(), including in a
-- transaction that began before the engagement expired.
ALTER TABLE principal_roles
 ADD COLUMN company_id bigint,
 ADD COLUMN valid_from date,
 ADD COLUMN valid_until date,
 ADD COLUMN purpose text NOT NULL DEFAULT '',
 ADD CONSTRAINT principal_roles_company_positive CHECK(company_id IS NULL OR company_id > 0),
 ADD CONSTRAINT principal_roles_dates CHECK(valid_from IS NULL OR valid_until IS NULL OR valid_until >= valid_from),
 ADD CONSTRAINT principal_roles_purpose CHECK(length(purpose) <= 1000);
ALTER TABLE principal_roles DROP CONSTRAINT principal_roles_pkey;
ALTER TABLE principal_roles ADD CONSTRAINT principal_roles_scope UNIQUE NULLS NOT DISTINCT(tenant_id,user_id,role_id,company_id);

ALTER TABLE roles ADD COLUMN requires_engagement boolean NOT NULL DEFAULT false;
CREATE OR REPLACE VIEW effective_principal_roles AS
 SELECT p.tenant_id,p.user_id,p.role_id,p.company_id FROM principal_roles p
 JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id
 WHERE (NOT r.requires_engagement OR (p.company_id IS NOT NULL AND p.valid_from IS NOT NULL AND p.valid_until IS NOT NULL AND length(trim(p.purpose)) > 0)) AND (valid_from IS NULL OR valid_from <= (clock_timestamp() AT TIME ZONE 'UTC')::date)
   AND (valid_until IS NULL OR valid_until >= (clock_timestamp() AT TIME ZONE 'UTC')::date)
 UNION SELECT p.tenant_id,p.user_id,p.role_id,NULL::bigint FROM authz_identity_roles p
 JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE NOT r.requires_engagement;

-- NULL target means tenant work: a company grant never authorizes it.
-- NULL account supports bounded holder enumeration on the same policy.
CREATE FUNCTION authz_roles_for(target_tenant bigint, target_user bigint, target_company bigint)
RETURNS TABLE(tenant_id bigint,user_id bigint,role_id bigint,company_id bigint)
LANGUAGE sql VOLATILE AS $$
 SELECT DISTINCT p.tenant_id,p.user_id,p.role_id,p.company_id
 FROM effective_principal_roles p
 WHERE p.tenant_id=target_tenant
 AND (target_user IS NULL OR p.user_id=target_user)
 AND (p.company_id IS NULL OR p.company_id=target_company)
$$;
