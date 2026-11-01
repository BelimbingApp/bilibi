-- Removes the rows the retired demo seed installed (Bilibi's earliest slices:
-- the example.test accounts, their roles, the generated companies, the second
-- tenant and the demo reference rows), from a database that `bilibi-web seed`
-- seeded before the demo was removed. Every row is matched by the exact
-- identity the demo seed gave it; a row that something else still references
-- is kept and reported instead. `ai@agent.my`, the platform-operator tenant
-- and its primary company are never deleted. Idempotent: a clean database
-- reports nothing. Each change is one line in `pg_temp.retired_demo`.
CREATE TEMP TABLE retired_demo(n serial, line text NOT NULL) ON COMMIT DROP;
DO $retire$
DECLARE
    acct record;
    r record;
    operator bigint := (SELECT tenant_id FROM platform_operator WHERE singleton);
    held bigint;
BEGIN
    -- Only a database the demo seed populated: its accounts, roles or second
    -- tenant are still there. Each run removes those markers, so a cleaned or
    -- fresh database never matches anything below again.
    IF NOT EXISTS (SELECT 1 FROM users u JOIN user_identities i ON i.id=u.identity_id WHERE (u.id,u.tenant_id,i.email) IN ((1,1,'manager@example.test'),(2,1,'viewer@example.test'),(3,1,'staff@example.test'),(4,1,'admin@example.test'),(5,2,'manager@example.test'),(5,2,'manager@other.example.test'),(7,1,'contractor@example.test')))
       AND NOT EXISTS (SELECT 1 FROM roles WHERE (id,tenant_id,name) IN ((1,1,'manager'),(2,1,'viewer'),(3,1,'staff'),(4,1,'admin'),(5,2,'manager')) AND (code IS NULL OR code IN ('', name)))
       AND NOT EXISTS (SELECT 1 FROM tenants WHERE id=2 AND name='Other Workspace') THEN
        RETURN;
    END IF;
    -- The demo accounts (tenant 2's manager under either address: K9 renamed
    -- it for global sign-in) and what only they own.
    FOR acct IN
        SELECT u.id, u.tenant_id, i.email FROM users u
        JOIN user_identities i ON i.id=u.identity_id
        JOIN (VALUES (1,1,'manager@example.test'),(2,1,'viewer@example.test'),(3,1,'staff@example.test'),
                     (4,1,'admin@example.test'),(5,2,'manager@example.test'),(5,2,'manager@other.example.test'),
                     (7,1,'contractor@example.test')) d(id,tenant_id,email)
          ON d.id=u.id AND d.tenant_id=u.tenant_id AND d.email=i.email
        ORDER BY u.id
    LOOP
        DELETE FROM sessions WHERE tenant_id=acct.tenant_id AND user_id=acct.id;
        DELETE FROM principal_roles WHERE tenant_id=acct.tenant_id AND user_id=acct.id;
        DELETE FROM principal_capabilities WHERE tenant_id=acct.tenant_id AND user_id=acct.id;
        DELETE FROM notifications WHERE tenant_id=acct.tenant_id AND user_id=acct.id;
        DELETE FROM user_database_queries WHERE tenant_id=acct.tenant_id AND user_id=acct.id;
        DELETE FROM base_settings WHERE tenant_id=acct.tenant_id AND scope_type='user' AND scope_id=acct.id;
        -- Pins, agent connections and reset tokens cascade with the account.
        DELETE FROM users WHERE tenant_id=acct.tenant_id AND id=acct.id;
        INSERT INTO retired_demo(line) VALUES (format('removed account %s %s (tenant %s), with its sessions, roles, notifications and preferences', acct.id, acct.email, acct.tenant_id));
    END LOOP;

    -- The five demo roles (no code, or the code the retired vocabulary gave
    -- them), with their grants and field restrictions.
    FOR r IN
        SELECT ro.id, ro.tenant_id, ro.name FROM roles ro
        JOIN (VALUES (1,1,'manager'),(2,1,'viewer'),(3,1,'staff'),(4,1,'admin'),(5,2,'manager')) d(id,tenant_id,name)
          ON d.id=ro.id AND d.tenant_id=ro.tenant_id AND d.name=ro.name
        WHERE ro.code IS NULL OR ro.code IN ('', ro.name)
        ORDER BY ro.id
    LOOP
        FOR acct IN SELECT u.id, i.email FROM principal_roles p JOIN users u ON u.tenant_id=p.tenant_id AND u.id=p.user_id JOIN user_identities i ON i.id=u.identity_id
                    WHERE p.tenant_id=r.tenant_id AND p.role_id=r.id ORDER BY u.id LOOP
            INSERT INTO retired_demo(line) VALUES (format('revoked demo role %s from account %s %s', r.name, acct.id, acct.email));
        END LOOP;
        DELETE FROM principal_roles WHERE tenant_id=r.tenant_id AND role_id=r.id;
        DELETE FROM role_capabilities WHERE tenant_id=r.tenant_id AND role_id=r.id;
        DELETE FROM field_restrictions WHERE tenant_id=r.tenant_id AND role_id=r.id;
        DELETE FROM roles WHERE tenant_id=r.tenant_id AND id=r.id;
        INSERT INTO retired_demo(line) VALUES (format('removed demo role %s %s (tenant %s), with its capabilities and field restrictions', r.id, r.name, r.tenant_id));
    END LOOP;

    -- System roles reconciled from the retired demo vocabulary in any tenant;
    -- one that an account still holds is an operator's grant and stays.
    FOR r IN SELECT id, tenant_id, code FROM roles WHERE is_system AND code IN ('manager','viewer','staff','admin') ORDER BY tenant_id, id LOOP
        SELECT count(*) INTO held FROM principal_roles WHERE tenant_id=r.tenant_id AND role_id=r.id;
        IF held > 0 THEN
            INSERT INTO retired_demo(line) VALUES (format('kept role %s %s (tenant %s): %s account(s) hold it', r.id, r.code, r.tenant_id, held));
        ELSE
            DELETE FROM role_capabilities WHERE tenant_id=r.tenant_id AND role_id=r.id;
            DELETE FROM field_restrictions WHERE tenant_id=r.tenant_id AND role_id=r.id;
            DELETE FROM roles WHERE id=r.id;
            INSERT INTO retired_demo(line) VALUES (format('removed retired system role %s %s (tenant %s)', r.id, r.code, r.tenant_id));
        END IF;
    END LOOP;

    -- The demo Persons and their affiliations (K9), unless something else
    -- references them. The agent Person linked to ai@agent.my stays.
    FOR r IN
        SELECT a.id, a.title FROM person_affiliations a
        JOIN (VALUES (1,1,1,'Operations Manager','manager@example.test'),(2,2,2,'Contractor','contractor@example.test')) d(id,person_id,organisation_id,title,work_email)
          ON d.id=a.id AND d.person_id=a.person_id AND d.organisation_id=a.organisation_id AND d.title=a.title AND d.work_email=a.work_email
        WHERE a.tenant_id=1 ORDER BY a.id
    LOOP
        BEGIN
            DELETE FROM person_affiliations WHERE tenant_id=1 AND id=r.id;
            INSERT INTO retired_demo(line) VALUES (format('removed demo affiliation %s %s', r.id, r.title));
        EXCEPTION WHEN foreign_key_violation THEN
            INSERT INTO retired_demo(line) VALUES (format('kept demo affiliation %s %s: other records reference it', r.id, r.title));
        END;
    END LOOP;
    FOR r IN
        SELECT p.id, p.name FROM persons p
        JOIN (VALUES (1,'Ada Manager','ada@example.test'),(2,'Kim Contractor','kim@example.test')) d(id,name,email)
          ON d.id=p.id AND d.name=p.name AND d.email=p.email
        WHERE p.tenant_id=1 ORDER BY p.id
    LOOP
        BEGIN
            DELETE FROM persons WHERE tenant_id=1 AND id=r.id;
            INSERT INTO retired_demo(line) VALUES (format('removed demo person %s %s', r.id, r.name));
        EXCEPTION WHEN foreign_key_violation THEN
            INSERT INTO retired_demo(line) VALUES (format('kept demo person %s %s: other records reference it', r.id, r.name));
        END;
    END LOOP;
    IF EXISTS (SELECT 1 FROM users u JOIN user_identities i ON i.id=u.identity_id JOIN persons p ON p.tenant_id=u.tenant_id AND p.id=u.person_id WHERE i.email='ai@agent.my' AND p.name='AI Agent') THEN
        INSERT INTO retired_demo(line) VALUES ('kept the agent Person "AI Agent" linked to ai@agent.my');
    END IF;

    -- The generated companies 2-62 of tenant 1 and the second tenant's
    -- company 100, unless a primary-company assignment, a child company, a
    -- department, an attachment, an artifact or a company setting uses one.
    FOR r IN
        SELECT c.id, c.tenant_id, c.name FROM companies c
        WHERE (c.tenant_id=1 AND c.id BETWEEN 2 AND 62 AND c.code='company_'||lpad(c.id::text,3,'0') AND c.name='Company '||lpad(c.id::text,3,'0'))
           OR (c.tenant_id=2 AND c.id=100 AND c.code='private_other_tenant_company' AND operator IS DISTINCT FROM 2)
        ORDER BY c.id
    LOOP
        IF EXISTS (SELECT 1 FROM tenant_primary_companies WHERE company_id=r.id AND tenant_id IS DISTINCT FROM 2)
           OR EXISTS (SELECT 1 FROM base_media_attachments WHERE tenant_id=r.tenant_id AND subject_id=r.id::text AND subject_type ILIKE '%compan%')
           OR EXISTS (SELECT 1 FROM base_artifacts WHERE tenant_id=r.tenant_id AND company_id=r.id)
           OR EXISTS (SELECT 1 FROM base_settings WHERE tenant_id=r.tenant_id AND scope_type='company' AND scope_id=r.id) THEN
            INSERT INTO retired_demo(line) VALUES (format('kept demo company %s %s: it is in use', r.id, r.name));
            CONTINUE;
        END IF;
        BEGIN
            DELETE FROM tenant_primary_companies WHERE tenant_id=2 AND company_id=r.id;
            DELETE FROM companies WHERE tenant_id=r.tenant_id AND id=r.id;
            INSERT INTO retired_demo(line) VALUES (format('removed demo company %s %s (tenant %s)', r.id, r.name, r.tenant_id));
        EXCEPTION WHEN foreign_key_violation THEN
            INSERT INTO retired_demo(line) VALUES (format('kept demo company %s %s: other records reference it', r.id, r.name));
        END;
    END LOOP;

    -- The second tenant, when nothing but its own generated rows remain.
    IF operator IS DISTINCT FROM 2 AND EXISTS (SELECT 1 FROM tenants WHERE id=2 AND name='Other Workspace') THEN
        BEGIN
            DELETE FROM sessions WHERE tenant_id=2;
            DELETE FROM authz_decision_log WHERE tenant_id=2;
            DELETE FROM authz_system_grants WHERE tenant_id=2;
            DELETE FROM role_capabilities WHERE tenant_id=2 AND role_id IN (SELECT id FROM roles WHERE tenant_id=2 AND is_system);
            DELETE FROM field_restrictions WHERE tenant_id=2 AND role_id IN (SELECT id FROM roles WHERE tenant_id=2 AND is_system);
            DELETE FROM roles WHERE tenant_id=2 AND is_system;
            DELETE FROM base_settings WHERE tenant_id=2;
            -- The relationship-role defaults Company's migration gave every tenant.
            DELETE FROM company_relationship_roles WHERE tenant_id=2;
            DELETE FROM tenants WHERE id=2;
            INSERT INTO retired_demo(line) VALUES ('removed demo tenant 2 Other Workspace, with its decision log, system roles and default relationship roles');
        EXCEPTION WHEN foreign_key_violation THEN
            INSERT INTO retired_demo(line) VALUES ('kept tenant 2 Other Workspace: it holds other records');
        END;
    END IF;

    -- The operator's primary company keeps its identity; only the facts the
    -- demo seed wrote and nobody changed since are reset to a fresh install's.
    IF operator IS NOT NULL THEN
        UPDATE companies c SET
            name=CASE WHEN c.name='Company 001' THEN 'Bilibi Development' ELSE c.name END,
            code=CASE WHEN c.code='company_001' AND NOT EXISTS (SELECT 1 FROM companies o WHERE o.tenant_id=c.tenant_id AND o.code='bilibi_dev') THEN 'bilibi_dev' ELSE c.code END,
            legal_name=CASE WHEN c.legal_name='Company 001 Sdn. Bhd.' THEN 'Bilibi Development' ELSE c.legal_name END,
            email=CASE WHEN c.email='contact1@example.test' THEN '' ELSE c.email END,
            phone=CASE WHEN c.phone='+60 12 345 1' THEN '' ELSE c.phone END,
            tax_id=CASE WHEN c.tax_id='TAX-1' THEN '' ELSE c.tax_id END,
            revenue=CASE WHEN c.revenue=125001 THEN 0 ELSE c.revenue END,
            registration_number=CASE WHEN c.registration_number='202401000001' THEN NULL ELSE c.registration_number END,
            legal_entity_type_id=CASE WHEN c.registration_number='202401000001' AND c.legal_entity_type_id=(SELECT id FROM company_legal_entity_types WHERE code='sdn_bhd') THEN NULL ELSE c.legal_entity_type_id END,
            website=CASE WHEN c.website='https://company-001.example.test' THEN NULL ELSE c.website END,
            version=c.version+1, updated_at=now()
        FROM tenant_primary_companies p
        WHERE p.tenant_id=operator AND c.tenant_id=p.tenant_id AND c.id=p.company_id AND c.id=1
          AND (c.name='Company 001' OR c.code='company_001' OR c.legal_name='Company 001 Sdn. Bhd.' OR c.email='contact1@example.test'
               OR c.phone='+60 12 345 1' OR c.tax_id='TAX-1' OR c.revenue=125001 OR c.registration_number='202401000001'
               OR c.website='https://company-001.example.test');
        IF FOUND THEN
            INSERT INTO retired_demo(line) VALUES ('reset the demo name, code, legal facts and contact details of the operator''s primary company 1 where they were unchanged');
        END IF;
    END IF;

    -- Demo reference rows nothing uses: the legal entity types and the
    -- Finance and Sales department types. Engineering and Operations stay,
    -- as a fresh install's development sample has them.
    FOR r IN
        DELETE FROM company_legal_entity_types t
        USING (VALUES ('sdn_bhd','Sdn. Bhd.'),('bhd','Bhd.'),('pte_ltd','Pte. Ltd.')) d(code,name)
        WHERE t.code=d.code AND t.name=d.name AND NOT EXISTS (SELECT 1 FROM companies c WHERE c.legal_entity_type_id=t.id)
        RETURNING t.id, t.name
    LOOP
        INSERT INTO retired_demo(line) VALUES (format('removed demo legal entity type %s %s', r.id, r.name));
    END LOOP;
    FOR r IN
        DELETE FROM company_department_types t
        USING (VALUES ('FIN','Finance'),('SALES','Sales')) d(code,name)
        WHERE t.code=d.code AND t.name=d.name AND NOT EXISTS (SELECT 1 FROM company_departments x WHERE x.department_type_id=t.id)
        RETURNING t.id, t.name
    LOOP
        INSERT INTO retired_demo(line) VALUES (format('removed demo department type %s %s', r.id, r.name));
    END LOOP;
END
$retire$;
