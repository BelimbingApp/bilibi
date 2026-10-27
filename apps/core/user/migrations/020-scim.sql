-- SCIM extends Bilimbi User provisioning; Bilimbi has no SCIM implementation.
ALTER TABLE user_identity_providers ADD COLUMN sealed_scim_token jsonb;
CREATE UNIQUE INDEX user_scim_company_token ON user_identity_providers(tenant_id,company_id)
 WHERE sealed_scim_token IS NOT NULL;
CREATE TABLE user_scim_users (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 resource_id text NOT NULL,
 user_id bigint NOT NULL,
 person_id bigint NOT NULL,
 affiliation_id bigint NOT NULL,
 document jsonb NOT NULL,
 active boolean NOT NULL DEFAULT true,
 FOREIGN KEY(tenant_id,provider_id) REFERENCES user_identity_providers(tenant_id,id),
 FOREIGN KEY(tenant_id,user_id) REFERENCES users(tenant_id,id),
 UNIQUE(tenant_id,user_id),
 UNIQUE(tenant_id,provider_id,resource_id)
);
CREATE UNIQUE INDEX user_scim_username ON user_scim_users(tenant_id,provider_id,lower(document->>'userName'));
CREATE UNIQUE INDEX user_scim_external_id ON user_scim_users(tenant_id,provider_id,(document->>'externalId'))
 WHERE document->>'externalId' IS NOT NULL;
CREATE TABLE user_scim_groups (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 resource_id text NOT NULL,
 document jsonb NOT NULL,
 FOREIGN KEY(tenant_id,provider_id) REFERENCES user_identity_providers(tenant_id,id),
 UNIQUE(tenant_id,provider_id,resource_id)
);
CREATE UNIQUE INDEX user_scim_group_name ON user_scim_groups(tenant_id,provider_id,lower(document->>'displayName'));
CREATE TABLE user_scim_members (
 id bigserial PRIMARY KEY,
 tenant_id bigint NOT NULL,
 provider_id bigint NOT NULL,
 group_id text NOT NULL,
 member_id text NOT NULL,
 FOREIGN KEY(tenant_id,provider_id,group_id) REFERENCES user_scim_groups(tenant_id,provider_id,resource_id),
 FOREIGN KEY(tenant_id,provider_id,member_id) REFERENCES user_scim_users(tenant_id,provider_id,resource_id),
 UNIQUE(tenant_id,provider_id,group_id,member_id)
);
