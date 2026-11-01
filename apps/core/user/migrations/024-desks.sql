-- Explicit desks supplement the implicit desk at a staff account's affiliation.
CREATE TABLE user_desks (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    person_id bigint NOT NULL,
    company_id bigint NOT NULL,
    UNIQUE(tenant_id, person_id, company_id),
    FOREIGN KEY(tenant_id, person_id) REFERENCES persons(tenant_id,id),
    FOREIGN KEY(tenant_id, company_id) REFERENCES companies(tenant_id,id)
);
CREATE INDEX user_desks_company ON user_desks(tenant_id,company_id,person_id);
