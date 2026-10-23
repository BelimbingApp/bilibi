-- Audit erasure (ADR 05): the one narrow exception to append-only evidence.
-- Inside a privacy erasure transaction, base_audit_erase_values replaces the
-- declared personal fields of one subject's mutation rows with "[erased]" and
-- records the erasure in an append-only ledger. Who, when, which record and
-- which fields remain; only values go. Every other update, every delete and
-- every truncate stays refused.

-- Mutation rows may change only through base_audit_erase_values, which sets the
-- transaction-local flag below, and only in old_values, new_values and
-- subject_identifier. Insert behaviour is unchanged from 006.
CREATE OR REPLACE FUNCTION base_audit_evidence_v1() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  -- Nested: PL/pgSQL may evaluate every operand, and only mutation rows have
  -- these columns; one IF would fail every write to the other evidence tables.
  IF TG_OP = 'UPDATE' AND TG_TABLE_NAME = 'base_audit_mutations' THEN
    IF current_setting('bilibi.audit_erasure', true) = 'on'
       AND (NEW.id, NEW.company_id, NEW.tenant_id, NEW.actor_type, NEW.actor_id, NEW.actor_role,
            NEW.impersonator_id, NEW.system_principal, NEW.ip_address, NEW.url, NEW.user_agent,
            NEW.auditable_type, NEW.auditable_id, NEW.subject_name, NEW.subject_id, NEW.source,
            NEW.event, NEW.trace_id, NEW.occurred_at, NEW.recorded_at,
            NEW.agent_self_report, NEW.client_identity)
           IS NOT DISTINCT FROM
           (OLD.id, OLD.company_id, OLD.tenant_id, OLD.actor_type, OLD.actor_id, OLD.actor_role,
            OLD.impersonator_id, OLD.system_principal, OLD.ip_address, OLD.url, OLD.user_agent,
            OLD.auditable_type, OLD.auditable_id, OLD.subject_name, OLD.subject_id, OLD.source,
            OLD.event, OLD.trace_id, OLD.occurred_at, OLD.recorded_at,
            OLD.agent_self_report, OLD.client_identity) THEN
      RETURN NEW;
    END IF;
  END IF;
  IF TG_OP <> 'INSERT' THEN
    RAISE EXCEPTION 'audit evidence is append-only' USING ERRCODE = '42501';
  END IF;
  NEW.recorded_at := clock_timestamp();
  -- Nested: PL/pgSQL may evaluate both operands, and diagnostics have no event time.
  IF TG_TABLE_NAME <> 'base_audit_capture_failures' THEN
    IF NEW.occurred_at IS NULL THEN
      NEW.occurred_at := date_trunc('second', NEW.recorded_at);
    END IF;
  END IF;
  RETURN NEW;
END;
$$;

-- Replaces each named top-level key that holds a value; null and missing keys
-- stay as they were, so the evidence still says which fields changed.
CREATE FUNCTION base_audit_erase_json(v jsonb, fields text[]) RETURNS jsonb
LANGUAGE sql IMMUTABLE AS $$
  SELECT CASE WHEN jsonb_typeof(v) = 'object' THEN
    COALESCE((SELECT jsonb_object_agg(key,
        CASE WHEN key = ANY(fields) AND value <> 'null'::jsonb THEN '"[erased]"'::jsonb ELSE value END)
      FROM jsonb_each(v)), '{}'::jsonb)
  ELSE v END
$$;

CREATE TABLE base_audit_erasures (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL,
 request_id bigint NOT NULL CHECK (request_id > 0),
 auditable_type varchar(255) NOT NULL,
 auditable_id varchar(128) NOT NULL,
 fields text[] NOT NULL CHECK (cardinality(fields) BETWEEN 1 AND 64),
 mutation_rows integer NOT NULL CHECK (mutation_rows >= 0),
 actor_type varchar(40) NOT NULL,
 actor_id bigint NOT NULL,
 trace_id text,
 occurred_at timestamptz,
 recorded_at timestamptz(6),
 CHECK (actor_type IN ('user','agent','guest','console','scheduler','queue','system','unknown'))
);
CREATE INDEX base_audit_erasures_request ON base_audit_erasures(tenant_id, request_id, id);
CREATE TRIGGER base_audit_erasures_evidence_guard BEFORE INSERT OR UPDATE OR DELETE ON base_audit_erasures FOR EACH ROW EXECUTE FUNCTION base_audit_evidence_v1();
CREATE TRIGGER base_audit_erasures_truncate_guard BEFORE TRUNCATE ON base_audit_erasures FOR EACH STATEMENT EXECUTE FUNCTION base_audit_evidence_v1();

-- Callable only in a transaction that base/privacy marked for this request
-- (`bilibi.privacy_erasure`, transaction-local). Returns the rows changed.
CREATE FUNCTION base_audit_erase_values(
  p_tenant bigint, p_request bigint, p_type text, p_id text, p_fields text[],
  p_actor_type text, p_actor_id bigint, p_trace text
) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
  changed integer;
BEGIN
  IF p_request IS NULL OR current_setting('bilibi.privacy_erasure', true) IS DISTINCT FROM p_request::text THEN
    RAISE EXCEPTION 'audit values can only be erased inside a privacy erasure transaction'
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
  INSERT INTO base_audit_erasures(tenant_id, request_id, auditable_type, auditable_id, fields,
      mutation_rows, actor_type, actor_id, trace_id)
    VALUES (p_tenant, p_request, p_type, p_id, p_fields, changed, p_actor_type, p_actor_id, p_trace);
  RETURN changed;
END;
$$;
