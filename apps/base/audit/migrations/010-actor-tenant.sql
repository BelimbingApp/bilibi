ALTER TABLE base_audit_actions ADD COLUMN actor_tenant_id bigint;
ALTER TABLE base_audit_actions ADD CONSTRAINT base_audit_actor_tenant CHECK
    (actor_tenant_id IS NULL OR (actor_type IN ('user','agent') AND actor_id>0 AND actor_tenant_id<>tenant_id));
