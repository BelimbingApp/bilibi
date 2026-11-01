-- An engagement may be indefinite. Its dates are opt-in limits, while its
-- company and purpose remain required. Keep the applied 014 definition frozen.
CREATE OR REPLACE VIEW effective_principal_roles AS
 SELECT p.tenant_id,p.user_id,p.role_id,p.company_id FROM principal_roles p
 JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id
 WHERE (NOT r.requires_engagement OR (p.company_id IS NOT NULL AND length(trim(p.purpose)) > 0))
   AND (valid_from IS NULL OR valid_from <= (clock_timestamp() AT TIME ZONE 'UTC')::date)
   AND (valid_until IS NULL OR valid_until >= (clock_timestamp() AT TIME ZONE 'UTC')::date)
 UNION SELECT p.tenant_id,p.user_id,p.role_id,NULL::bigint FROM authz_identity_roles p
 JOIN roles r ON r.tenant_id=p.tenant_id AND r.id=p.role_id WHERE NOT r.requires_engagement;
