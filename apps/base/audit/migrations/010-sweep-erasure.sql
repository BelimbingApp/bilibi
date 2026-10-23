-- Retention sweeps (ADR 05) clear evidence the way erasure requests do: an
-- anonymised or purged record's declared personal fields become "[erased]"
-- in its mutation rows, recorded in the same append-only ledger. A ledger row
-- names exactly one of the request or the sweep that cleared it.
ALTER TABLE base_audit_erasures
    ALTER COLUMN request_id DROP NOT NULL,
    ADD COLUMN sweep_id bigint CHECK (sweep_id > 0),
    ADD CONSTRAINT base_audit_erasures_one_cause CHECK ((request_id IS NULL) <> (sweep_id IS NULL));
CREATE INDEX base_audit_erasures_sweep ON base_audit_erasures(tenant_id, sweep_id, id) WHERE sweep_id IS NOT NULL;

-- Callable only in a transaction that base/privacy marked for this sweep
-- (`bilibi.privacy_sweep`, transaction-local). Returns the rows changed.
CREATE FUNCTION base_audit_erase_sweep_values(
  p_tenant bigint, p_sweep bigint, p_type text, p_id text, p_fields text[],
  p_actor_type text, p_actor_id bigint, p_trace text
) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
  changed integer;
BEGIN
  IF p_sweep IS NULL OR current_setting('bilibi.privacy_sweep', true) IS DISTINCT FROM p_sweep::text THEN
    RAISE EXCEPTION 'audit values can only be erased inside a privacy sweep transaction'
      USING ERRCODE = '42501';
  END IF;
  IF p_tenant IS NULL OR p_tenant <= 0 OR p_type IS NULL OR p_id IS NULL
     OR p_fields IS NULL OR cardinality(p_fields) NOT BETWEEN 1 AND 64 THEN
    RAISE EXCEPTION 'invalid audit erasure' USING ERRCODE = '22023';
  END IF;
  PERFORM set_config('bilibi.audit_erasure', 'on', true);
  UPDATE base_audit_mutations
     SET old_values = base_audit_erase_json(old_values, p_fields),
         new_values = base_audit_erase_json(new_values, p_fields),
         subject_identifier = CASE WHEN subject_identifier IS NULL THEN NULL ELSE '[erased]' END
   WHERE tenant_id = p_tenant AND auditable_type = p_type AND auditable_id = p_id;
  GET DIAGNOSTICS changed = ROW_COUNT;
  PERFORM set_config('bilibi.audit_erasure', 'off', true);
  INSERT INTO base_audit_erasures(tenant_id, sweep_id, auditable_type, auditable_id, fields,
      mutation_rows, actor_type, actor_id, trace_id)
    VALUES (p_tenant, p_sweep, p_type, p_id, p_fields, changed, p_actor_type, p_actor_id, p_trace);
  RETURN changed;
END;
$$;
