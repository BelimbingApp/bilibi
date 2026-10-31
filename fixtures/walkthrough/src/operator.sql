-- The fixture accounts' roles and Person links, and tenant 1 as the platform operator whose
-- ai@agent.my (account 6) holds the grant-all `core_admin` system role, as
-- authz reconciliation would create it.
INSERT INTO principal_roles(tenant_id,user_id,role_id) VALUES(1,1,1),(1,2,2),(1,3,3),(1,4,4),(2,5,5),(1,7,3) ON CONFLICT DO NOTHING;
SELECT setval(pg_get_serial_sequence('users','id'),GREATEST((SELECT max(id) FROM users),1));
INSERT INTO platform_operator(singleton,tenant_id) VALUES(true,1) ON CONFLICT DO NOTHING;
INSERT INTO roles(tenant_id,name,code,is_system,grant_all,description)
SELECT 1,'Core Administrator','core_admin',true,true,'Contributed system role; reconciled explicitly'
WHERE NOT EXISTS (SELECT 1 FROM roles WHERE tenant_id=1 AND code='core_admin');
INSERT INTO principal_roles(tenant_id,user_id,role_id)
SELECT 1,6,id FROM roles WHERE tenant_id=1 AND code='core_admin' ON CONFLICT DO NOTHING;
-- K9 identities: the manager and the contractor are Persons affiliated with
-- Company 001 (the tenant's primary company) and Company 002; the AI Agent
-- account signs in as an agent Person. Archiving Company 002 makes the
-- contractor's desk unavailable without freezing the account; the other
-- accounts stay unlinked on purpose.
INSERT INTO persons(id,tenant_id,code,name,kind,email) VALUES(1,1,'ada_manager','Ada Manager','human','ada@example.test'),(2,1,'kim_contractor','Kim Contractor','human','kim@example.test'),(3,1,'ai_agent','AI Agent','agent',NULL) ON CONFLICT DO NOTHING;
INSERT INTO person_affiliations(id,tenant_id,person_id,organisation_id,title,work_email) VALUES(1,1,1,1,'Operations Manager','manager@example.test'),(2,1,2,2,'Contractor','contractor@example.test') ON CONFLICT DO NOTHING;
UPDATE users SET person_id=1 WHERE tenant_id=1 AND id=1 AND person_id IS NULL AND affiliation_id IS NULL;
UPDATE users SET person_id=2 WHERE tenant_id=1 AND id=7 AND person_id IS NULL AND affiliation_id IS NULL;
UPDATE users SET person_id=3 WHERE tenant_id=1 AND id=6 AND person_id IS NULL;
SELECT setval(pg_get_serial_sequence('persons','id'),GREATEST((SELECT max(id) FROM persons),1));
SELECT setval(pg_get_serial_sequence('person_affiliations','id'),GREATEST((SELECT max(id) FROM person_affiliations),1));

INSERT INTO user_desks(tenant_id,person_id,company_id) VALUES(1,1,1),(1,2,2) ON CONFLICT(tenant_id,person_id,company_id) DO NOTHING;
