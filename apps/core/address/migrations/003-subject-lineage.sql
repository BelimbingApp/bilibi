-- No Bilimbi counterpart: privacy lineage survives the live owner's unlink.
-- This is not an address attachment and grants no owner access.
CREATE TABLE address_person_subjects (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id),
    address_id bigint NOT NULL,
    person_id bigint NOT NULL,
    UNIQUE (tenant_id, person_id, address_id),
    FOREIGN KEY (tenant_id, address_id) REFERENCES addresses(tenant_id, id),
    FOREIGN KEY (tenant_id, person_id) REFERENCES persons(tenant_id, id)
);
