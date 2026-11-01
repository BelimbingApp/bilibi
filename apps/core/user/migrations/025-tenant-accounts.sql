-- The only conversion authorized for disposable development databases:
-- the former staff company link becomes the person's first explicit desk.
INSERT INTO user_desks(tenant_id,person_id,company_id)
SELECT DISTINCT tenant_id,person_id,organisation_id FROM users
WHERE kind='staff' AND person_id IS NOT NULL AND organisation_id IS NOT NULL
ON CONFLICT(tenant_id,person_id,company_id) DO NOTHING;
UPDATE users SET affiliation_id=NULL,organisation_id=NULL WHERE kind='staff';
-- Customer portal affiliations remain business identity, not staff workplaces.
ALTER TABLE users ADD CONSTRAINT users_staff_tenant_only CHECK(kind <> 'staff' OR (affiliation_id IS NULL AND organisation_id IS NULL));
ALTER TABLE users ADD COLUMN last_desk_id bigint;
ALTER TABLE users ADD CONSTRAINT users_last_desk_company_fk FOREIGN KEY(tenant_id,last_desk_id) REFERENCES companies(tenant_id,id);
