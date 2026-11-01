ALTER TABLE user_identity_proofs
 ADD COLUMN provider_id bigint NOT NULL,
 ADD COLUMN provider_version bigint NOT NULL CHECK(provider_version > 0),
 ADD CONSTRAINT user_identity_proofs_provider
 FOREIGN KEY(tenant_id, provider_id) REFERENCES user_identity_providers(tenant_id, id);
