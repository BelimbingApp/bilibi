-- Issue 147, part 1: credentials belong to one platform sign-in, while users
-- remain tenant-owned accounts. Development databases are disposable until
-- 2027-01-01, so this clean break deliberately refuses rows rather than
-- converting existing accounts.
DO $$
BEGIN
 IF EXISTS (SELECT 1 FROM users) THEN
  RAISE EXCEPTION 'core-user-028-sign-in-identities requires a fresh database';
 END IF;
END $$;

DROP INDEX users_login_email_global_idx;
DROP INDEX users_customer_email_idx;

CREATE TABLE user_identities (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 kind text NOT NULL CONSTRAINT user_identities_kind_check CHECK (kind IN ('staff','customer')),
 tenant_id bigint REFERENCES tenants(id),
 email text NOT NULL,
 password_hash text NOT NULL,
 password_version bigint NOT NULL DEFAULT 1,
 email_verified_at timestamptz,
 status text NOT NULL DEFAULT 'active' CONSTRAINT user_identities_status_check CHECK (status IN ('active','deleted')),
 home_tenant_id bigint NOT NULL REFERENCES tenants(id),
 home_user_id bigint NOT NULL,
 last_tenant_id bigint REFERENCES tenants(id),
 deleted_at timestamptz,
 created_at timestamptz NOT NULL DEFAULT now(),
 updated_at timestamptz NOT NULL DEFAULT now(),
 CONSTRAINT user_identities_audience_check CHECK (
  (kind='staff' AND tenant_id IS NULL) OR
  (kind='customer' AND tenant_id=home_tenant_id)
 ),
 CONSTRAINT user_identities_deleted_check CHECK ((status='deleted')=(deleted_at IS NOT NULL)),
 UNIQUE(home_tenant_id,home_user_id)
);
CREATE UNIQUE INDEX user_identities_staff_email_idx ON user_identities(lower(email)) WHERE deleted_at IS NULL AND kind='staff';
CREATE UNIQUE INDEX user_identities_customer_email_idx ON user_identities(tenant_id,lower(email)) WHERE deleted_at IS NULL AND kind='customer';

ALTER TABLE users
 ADD COLUMN identity_id bigint,
 ALTER COLUMN email DROP NOT NULL,
 ALTER COLUMN password_hash DROP NOT NULL,
 ADD CONSTRAINT users_identity_fkey FOREIGN KEY(identity_id) REFERENCES user_identities(id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE users ALTER COLUMN identity_id SET NOT NULL;
ALTER TABLE user_identities
 ADD CONSTRAINT user_identities_home_account_fkey FOREIGN KEY(home_tenant_id,home_user_id) REFERENCES users(tenant_id,id) DEFERRABLE INITIALLY DEFERRED;
ALTER TABLE users DROP COLUMN email, DROP COLUMN password_hash, DROP COLUMN email_verified_at;
CREATE UNIQUE INDEX users_live_identity_key ON users(tenant_id,identity_id) WHERE deleted_at IS NULL;

ALTER TABLE password_reset_tokens DROP CONSTRAINT password_reset_tokens_pkey;
ALTER TABLE password_reset_tokens DROP CONSTRAINT password_reset_tokens_tenant_id_user_id_fkey;
ALTER TABLE password_reset_tokens DROP COLUMN tenant_id, DROP COLUMN user_id;
ALTER TABLE password_reset_tokens ADD COLUMN identity_id bigint NOT NULL REFERENCES user_identities(id) ON DELETE CASCADE;
ALTER TABLE password_reset_tokens ADD PRIMARY KEY(identity_id);

ALTER TABLE user_credential_attempts DROP CONSTRAINT user_credential_attempts_pkey;
ALTER TABLE user_credential_attempts DROP CONSTRAINT user_credential_attempts_user_fkey;
ALTER TABLE user_credential_attempts DROP COLUMN tenant_id, DROP COLUMN user_id;
ALTER TABLE user_credential_attempts ADD COLUMN identity_id bigint NOT NULL REFERENCES user_identities(id) ON DELETE CASCADE;
ALTER TABLE user_credential_attempts ADD PRIMARY KEY(identity_id,kind);

ALTER TABLE user_two_factor DROP CONSTRAINT user_two_factor_pkey;
ALTER TABLE user_two_factor DROP CONSTRAINT user_two_factor_user_fkey;
ALTER TABLE user_two_factor DROP COLUMN tenant_id, DROP COLUMN user_id;
ALTER TABLE user_two_factor ADD COLUMN identity_id bigint NOT NULL REFERENCES user_identities(id) ON DELETE CASCADE;
ALTER TABLE user_two_factor ADD PRIMARY KEY(identity_id);

ALTER TABLE user_sign_ins ADD COLUMN identity_id bigint REFERENCES user_identities(id) ON DELETE CASCADE;
ALTER TABLE user_sign_ins ALTER COLUMN identity_id SET NOT NULL;
CREATE INDEX user_sign_ins_identity_pending ON user_sign_ins(identity_id) WHERE state='pending';

INSERT INTO base_personal_data_kinds(kind,module,fields,subject_identifier,person)
 VALUES('user_identity','core/user',ARRAY['email'],false,false);

