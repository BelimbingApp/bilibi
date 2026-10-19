INSERT INTO tenants(id,name) VALUES(1,'Bilimbi Labs'),(2,'Other Workspace') ON CONFLICT DO NOTHING;
INSERT INTO roles(id,tenant_id,name,grant_all) VALUES(1,1,'manager',false),(2,1,'viewer',false),(3,1,'staff',false),(4,1,'admin',true),(5,2,'manager',false) ON CONFLICT DO NOTHING;
INSERT INTO role_capabilities(tenant_id,role_id,capability)
SELECT tenant_id,id,capability FROM roles CROSS JOIN unnest(ARRAY['companies.view']) capability WHERE id IN (1,2,3,4,5) ON CONFLICT DO NOTHING;
INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,1,'companies.update'),(1,3,'companies.update'),(2,5,'companies.update') ON CONFLICT DO NOTHING;
INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id)
SELECT 1,role_id,'companies',field_id FROM unnest(ARRAY[2,3]::bigint[]) role_id CROSS JOIN unnest(ARRAY['tax_id','revenue']) field_id ON CONFLICT DO NOTHING;
INSERT INTO field_restrictions(tenant_id,role_id,table_id,field_id) VALUES(1,2,'companies','email'),(1,2,'companies','phone') ON CONFLICT DO NOTHING;
INSERT INTO role_capabilities(tenant_id,role_id,capability)
SELECT tenant_id,id,capability FROM roles CROSS JOIN unnest(ARRAY['admin.company.view','admin.company.list']) capability WHERE id IN (1,2,3,5) ON CONFLICT DO NOTHING;
INSERT INTO role_capabilities(tenant_id,role_id,capability)
SELECT tenant_id,id,capability FROM roles CROSS JOIN unnest(ARRAY['admin.company.create','admin.company.update','admin.company.delete','admin.company.tenant-wide.manage']) capability WHERE id IN (1,5) ON CONFLICT DO NOTHING;
INSERT INTO role_capabilities(tenant_id,role_id,capability) VALUES(1,3,'admin.company.update') ON CONFLICT DO NOTHING;
INSERT INTO companies(id,tenant_id,name,code,status,email,phone,tax_id,revenue)
SELECT n,1,'Company '||lpad(n::text,3,'0'),'company_'||lpad(n::text,3,'0'),CASE WHEN n%3=0 THEN 'pending' ELSE 'active' END,'contact'||n||'@example.test','+60 12 345 '||n,'TAX-'||n,125000+n FROM generate_series(1,62) n ON CONFLICT DO NOTHING;
INSERT INTO companies(id,tenant_id,name,code,status,email,phone,tax_id,revenue) VALUES(100,2,'Private other-tenant company','private_other_tenant_company','active','private@example.test','private phone','PRIVATE-TAX',987654321) ON CONFLICT DO NOTHING;
-- K1 organisation facts: reference catalogs, the first company's legal
-- identity and departments, and each tenant's primary company.
INSERT INTO company_legal_entity_types(code,name,description) VALUES('sdn_bhd','Sdn. Bhd.','Private limited company (Malaysia)'),('bhd','Bhd.','Public limited company (Malaysia)'),('pte_ltd','Pte. Ltd.','Private limited company (Singapore)') ON CONFLICT DO NOTHING;
INSERT INTO company_department_types(id,code,name,category) VALUES(1,'ENG','Engineering','operational'),(2,'OPS','Operations','operational'),(3,'FIN','Finance','administrative'),(4,'SALES','Sales','revenue') ON CONFLICT DO NOTHING;
INSERT INTO company_departments(tenant_id,company_id,department_type_id,status) VALUES(1,1,1,'active'),(1,1,2,'active') ON CONFLICT DO NOTHING;
UPDATE companies SET legal_name='Company 001 Sdn. Bhd.',registration_number='202401000001',legal_entity_type_id=(SELECT id FROM company_legal_entity_types WHERE code='sdn_bhd'),website='https://company-001.example.test' WHERE tenant_id=1 AND id=1 AND legal_name IS NULL;
INSERT INTO tenant_primary_companies(tenant_id,company_id) VALUES(1,1),(2,100) ON CONFLICT DO NOTHING;
SELECT setval(pg_get_serial_sequence('company_department_types','id'),GREATEST((SELECT max(id) FROM company_department_types),1));
SELECT setval(pg_get_serial_sequence('tenants','id'),GREATEST((SELECT max(id) FROM tenants),1));
SELECT setval(pg_get_serial_sequence('roles','id'),GREATEST((SELECT max(id) FROM roles),1));
SELECT setval(pg_get_serial_sequence('companies','id'),GREATEST((SELECT max(id) FROM companies),1));
