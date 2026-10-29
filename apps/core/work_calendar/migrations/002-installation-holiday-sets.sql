-- Installation-wide holiday sets (issue 210, ADR 13): the platform operator
-- keeps them in its own tenant, every tenant reads them and includes them in
-- its calendars like its own sets, and only the operator writes them. A code
-- names one set for each tenant: the module refuses a tenant set with an
-- installation-wide set's code and the reverse.
ALTER TABLE core_work_calendar_holiday_sets ADD COLUMN installation boolean NOT NULL DEFAULT false;
ALTER TABLE core_work_calendar_holiday_sets ADD UNIQUE (tenant_id, id, installation);
-- Codes are read across tenants: an installation-wide set by its code, and a
-- new installation-wide code against every tenant's.
CREATE INDEX core_work_calendar_holiday_sets_code_idx ON core_work_calendar_holiday_sets (code);
-- A tenant's listing reads its own sets and the installation-wide ones, so it
-- grows with those, not with every tenant's sets.
CREATE INDEX core_work_calendar_holiday_sets_installation_idx ON core_work_calendar_holiday_sets (code) WHERE installation;

-- An inclusion names the tenant keeping its set: the calendar's own, or the
-- operator's for an installation-wide set. Only an installation-wide set
-- crosses tenants.
ALTER TABLE core_work_calendar_includes ADD COLUMN set_tenant_id bigint;
ALTER TABLE core_work_calendar_includes ADD COLUMN set_installation boolean NOT NULL DEFAULT false;
UPDATE core_work_calendar_includes SET set_tenant_id = tenant_id;
ALTER TABLE core_work_calendar_includes ALTER COLUMN set_tenant_id SET NOT NULL;
ALTER TABLE core_work_calendar_includes DROP CONSTRAINT core_work_calendar_includes_tenant_id_holiday_set_id_fkey;
ALTER TABLE core_work_calendar_includes ADD FOREIGN KEY (set_tenant_id, holiday_set_id, set_installation) REFERENCES core_work_calendar_holiday_sets (tenant_id, id, installation);
ALTER TABLE core_work_calendar_includes ADD CHECK (set_tenant_id = tenant_id OR set_installation);
-- The tenants a change to an installation-wide set must be told about.
CREATE INDEX core_work_calendar_includes_keeper_idx ON core_work_calendar_includes (set_tenant_id, holiday_set_id, tenant_id);
