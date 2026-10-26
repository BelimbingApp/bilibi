-- Audit erasure (ADR 05): the one narrow exception to append-only evidence.
-- base_audit_erase_values replaces the declared personal fields of one
-- record's mutation rows with "[erased]" and writes one append-only ledger row
-- in the same statement. Who, when, which record and which field names remain;
-- only values go. Rows still within their legal minimum stay, and a deferral
-- row makes the retention sweep erase them once it has passed.
--
-- No session setting opens this path. The guard admits a change only while
-- the PL/pgSQL call stack runs one of the erasure functions as the role that
-- owns the evidence, and EXECUTE on them is revoked from PUBLIC: a role that
-- was granted UPDATE on the table, or that sets any variable, is refused.

--
-- A hash chain links every mutation and action row, in the order the sealer
-- reaches them, over its non-personal facts only: ID, tenant, time, actor,
-- event, record and field names. Approved redaction changes values, so it
-- leaves the chain whole; a deleted row, or a changed ID, actor, time, event,
-- record or field name, breaks it, and base_audit_verify says where.

-- Whether the caller is inside `p_function`, running as `p_table`'s owner.
CREATE FUNCTION base_audit_within(p_function text, p_table oid) RETURNS boolean
LANGUAGE plpgsql STABLE AS $$
DECLARE
  stack text;
BEGIN
  GET DIAGNOSTICS stack = PG_CONTEXT;
  RETURN position(('function ' || p_function || '(') IN stack) > 0
     AND current_user = (SELECT pg_get_userbyid(relowner) FROM pg_class WHERE oid = p_table);
END;
$$;

ALTER TABLE base_audit_mutations ADD COLUMN chain_seq bigint, ADD COLUMN chain_hash bytea;
ALTER TABLE base_audit_actions ADD COLUMN chain_seq bigint, ADD COLUMN chain_hash bytea;
CREATE UNIQUE INDEX base_audit_mutations_chain ON base_audit_mutations(chain_seq) WHERE chain_seq IS NOT NULL;
CREATE INDEX base_audit_mutations_unsealed ON base_audit_mutations(id) WHERE chain_seq IS NULL;
CREATE UNIQUE INDEX base_audit_actions_chain ON base_audit_actions(chain_seq) WHERE chain_seq IS NOT NULL;
CREATE INDEX base_audit_actions_unsealed ON base_audit_actions(id) WHERE chain_seq IS NULL;
-- The last sealed link of each chain, so a removed tail is found too.
CREATE TABLE base_audit_chain_heads (
 chain text PRIMARY KEY CHECK (chain IN ('mutations', 'actions')),
 seq bigint NOT NULL CHECK (seq >= 0),
 hash bytea NOT NULL,
 sealed_at timestamptz NOT NULL DEFAULT clock_timestamp()
);
INSERT INTO base_audit_chain_heads(chain, seq, hash) VALUES ('mutations', 0, ''::bytea), ('actions', 0, ''::bytea);

CREATE TABLE base_audit_erasures (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL CHECK (tenant_id > 0),
 -- Exactly one cause: the request or the retention sweep that erased.
 request_id bigint CHECK (request_id > 0),
 sweep_id bigint CHECK (sweep_id > 0),
 auditable_type varchar(255) NOT NULL,
 auditable_id varchar(128) NOT NULL,
 fields text[] NOT NULL CHECK (cardinality(fields) <= 64),
 mutation_rows integer NOT NULL CHECK (mutation_rows >= 0),
 -- Rows still within their legal minimum, kept until kept_until.
 kept_rows integer NOT NULL DEFAULT 0 CHECK (kept_rows >= 0),
 kept_until timestamptz,
 actor_type varchar(40) NOT NULL,
 actor_id bigint NOT NULL,
 trace_id text,
 occurred_at timestamptz,
 recorded_at timestamptz(6),
 CHECK ((request_id IS NULL) <> (sweep_id IS NULL)),
 CHECK ((kept_rows = 0) = (kept_until IS NULL)),
 CHECK (actor_type IN ('user','agent','guest','console','scheduler','queue','system','unknown'))
);
CREATE INDEX base_audit_erasures_request ON base_audit_erasures(tenant_id, request_id, id) WHERE request_id IS NOT NULL;
CREATE INDEX base_audit_erasures_sweep ON base_audit_erasures(tenant_id, sweep_id, id) WHERE sweep_id IS NOT NULL;

-- Evidence kept for its legal minimum, waiting for the sweep. One open row per
-- record; it is closed once, when the sweep has erased the rest.
CREATE TABLE base_audit_erasure_deferrals (
 id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
 tenant_id bigint NOT NULL CHECK (tenant_id > 0),
 auditable_type varchar(255) NOT NULL,
 auditable_id varchar(128) NOT NULL,
 fields text[] NOT NULL CHECK (cardinality(fields) <= 64),
 subject_identifier boolean NOT NULL,
 due_at timestamptz NOT NULL,
 created_at timestamptz NOT NULL DEFAULT clock_timestamp(),
 done_at timestamptz
);
CREATE UNIQUE INDEX base_audit_erasure_deferrals_open ON base_audit_erasure_deferrals(tenant_id, auditable_type, auditable_id)
  WHERE done_at IS NULL;
CREATE INDEX base_audit_erasure_deferrals_due ON base_audit_erasure_deferrals(tenant_id, due_at) WHERE done_at IS NULL;

CREATE OR REPLACE FUNCTION base_audit_evidence_v1() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  -- Nested: PL/pgSQL may evaluate every operand, and only mutation rows have
  -- these columns; one IF would fail every write to the other evidence tables.
  IF TG_OP = 'UPDATE' AND TG_TABLE_NAME = 'base_audit_mutations' THEN
    IF base_audit_within('base_audit_erase_values', TG_RELID)
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
            OLD.agent_self_report, OLD.client_identity)
       AND (NEW.chain_seq, NEW.chain_hash) IS NOT DISTINCT FROM (OLD.chain_seq, OLD.chain_hash) THEN
      RETURN NEW;
    END IF;
  END IF;
  -- The sealer links a row once and changes nothing else about it.
  IF TG_OP = 'UPDATE' AND TG_TABLE_NAME IN ('base_audit_mutations', 'base_audit_actions') THEN
    IF OLD.chain_seq IS NULL AND NEW.chain_seq IS NOT NULL AND NEW.chain_hash IS NOT NULL
       AND base_audit_within('base_audit_seal', TG_RELID)
       AND (to_jsonb(NEW) - 'chain_seq' - 'chain_hash') = (to_jsonb(OLD) - 'chain_seq' - 'chain_hash') THEN
      RETURN NEW;
    END IF;
  END IF;
  IF TG_TABLE_NAME = 'base_audit_chain_heads' THEN
    IF TG_OP = 'UPDATE' THEN
      IF base_audit_within('base_audit_seal', TG_RELID) AND NEW.chain = OLD.chain THEN
        RETURN NEW;
      END IF;
    END IF;
    RAISE EXCEPTION 'audit chain heads move only through base_audit_seal' USING ERRCODE = '42501';
  END IF;
  IF TG_TABLE_NAME = 'base_audit_erasures' THEN
    IF TG_OP <> 'INSERT' OR NOT base_audit_within('base_audit_erase_values', TG_RELID) THEN
      RAISE EXCEPTION 'the audit erasure ledger is written only by base_audit_erase_values'
        USING ERRCODE = '42501';
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
CREATE TRIGGER base_audit_chain_heads_guard BEFORE INSERT OR UPDATE OR DELETE ON base_audit_chain_heads FOR EACH ROW EXECUTE FUNCTION base_audit_evidence_v1();
CREATE TRIGGER base_audit_chain_heads_truncate_guard BEFORE TRUNCATE ON base_audit_chain_heads FOR EACH STATEMENT EXECUTE FUNCTION base_audit_evidence_v1();
CREATE TRIGGER base_audit_erasures_evidence_guard BEFORE INSERT OR UPDATE OR DELETE ON base_audit_erasures FOR EACH ROW EXECUTE FUNCTION base_audit_evidence_v1();
CREATE TRIGGER base_audit_erasures_truncate_guard BEFORE TRUNCATE ON base_audit_erasures FOR EACH STATEMENT EXECUTE FUNCTION base_audit_evidence_v1();

CREATE FUNCTION base_audit_deferrals_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'INSERT' AND base_audit_within('base_audit_erase_values', TG_RELID) THEN
    RETURN NEW;
  END IF;
  IF TG_OP = 'UPDATE' AND OLD.done_at IS NULL
     AND (base_audit_within('base_audit_erase_values', TG_RELID) OR base_audit_within('base_audit_erase_due', TG_RELID))
     AND (NEW.id, NEW.tenant_id, NEW.auditable_type, NEW.auditable_id, NEW.created_at)
         IS NOT DISTINCT FROM (OLD.id, OLD.tenant_id, OLD.auditable_type, OLD.auditable_id, OLD.created_at) THEN
    RETURN NEW;
  END IF;
  RAISE EXCEPTION 'audit erasure deferrals change only through the erasure functions' USING ERRCODE = '42501';
END;
$$;
CREATE TRIGGER base_audit_erasure_deferrals_guard BEFORE INSERT OR UPDATE OR DELETE ON base_audit_erasure_deferrals
  FOR EACH ROW EXECUTE FUNCTION base_audit_deferrals_guard();
CREATE TRIGGER base_audit_erasure_deferrals_truncate_guard BEFORE TRUNCATE ON base_audit_erasure_deferrals
  FOR EACH STATEMENT EXECUTE FUNCTION base_audit_deferrals_guard();

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

-- Erases one record's evidence for one cause. The caller (Audit's Rust facade,
-- given a sealed grant) passes the fields its record kind declares. With
-- p_keep_days, rows newer than that many days stay and one open deferral says
-- when the sweep may erase them; the erasure's own rows (its trace) are never
-- kept, since no erasure trail may hold the values it erased. Returns the rows
-- erased, the rows kept and until when.
CREATE FUNCTION base_audit_erase_values(
  p_tenant bigint, p_request bigint, p_sweep bigint, p_type text, p_id text, p_fields text[],
  p_subject_identifier boolean, p_keep_days integer,
  p_actor_type text, p_actor_id bigint, p_trace text,
  OUT erased integer, OUT kept integer, OUT kept_until timestamptz
) LANGUAGE plpgsql AS $$
DECLARE
  cutoff timestamptz;
  newest timestamptz;
BEGIN
  IF p_tenant IS NULL OR p_tenant <= 0 OR (p_request IS NULL) = (p_sweep IS NULL)
     OR p_type IS NULL OR p_id IS NULL OR p_fields IS NULL OR cardinality(p_fields) > 64
     OR p_subject_identifier IS NULL OR (p_keep_days IS NOT NULL AND p_keep_days NOT BETWEEN 1 AND 36500) THEN
    RAISE EXCEPTION 'invalid audit erasure' USING ERRCODE = '22023';
  END IF;
  IF p_keep_days IS NOT NULL THEN
    cutoff := clock_timestamp() - make_interval(days => p_keep_days);
  END IF;
  UPDATE base_audit_mutations
     SET old_values = base_audit_erase_json(old_values, p_fields),
         new_values = base_audit_erase_json(new_values, p_fields),
         subject_identifier = CASE WHEN p_subject_identifier AND subject_identifier IS NOT NULL
                                   THEN '[erased]' ELSE subject_identifier END
   WHERE tenant_id = p_tenant AND auditable_type = p_type AND auditable_id = p_id
     AND (cutoff IS NULL OR occurred_at < cutoff OR trace_id IS NOT DISTINCT FROM p_trace);
  GET DIAGNOSTICS erased = ROW_COUNT;
  SELECT count(*), max(occurred_at) INTO kept, newest FROM base_audit_mutations
   WHERE tenant_id = p_tenant AND auditable_type = p_type AND auditable_id = p_id
     AND cutoff IS NOT NULL AND occurred_at >= cutoff AND trace_id IS DISTINCT FROM p_trace;
  IF kept > 0 THEN
    kept_until := newest + make_interval(days => p_keep_days);
    INSERT INTO base_audit_erasure_deferrals(tenant_id, auditable_type, auditable_id, fields, subject_identifier, due_at)
      VALUES (p_tenant, p_type, p_id, p_fields, p_subject_identifier, kept_until)
      ON CONFLICT (tenant_id, auditable_type, auditable_id) WHERE done_at IS NULL
      DO UPDATE SET due_at = greatest(base_audit_erasure_deferrals.due_at, EXCLUDED.due_at),
                    fields = ARRAY(SELECT DISTINCT unnest(base_audit_erasure_deferrals.fields || EXCLUDED.fields) ORDER BY 1),
                    subject_identifier = base_audit_erasure_deferrals.subject_identifier OR EXCLUDED.subject_identifier;
  ELSE
    kept_until := NULL;
    -- Nothing left to wait for: an open deferral for this record is done.
    UPDATE base_audit_erasure_deferrals SET done_at = clock_timestamp()
     WHERE tenant_id = p_tenant AND auditable_type = p_type AND auditable_id = p_id AND done_at IS NULL;
  END IF;
  INSERT INTO base_audit_erasures(tenant_id, request_id, sweep_id, auditable_type, auditable_id, fields,
      mutation_rows, kept_rows, kept_until, actor_type, actor_id, trace_id)
    VALUES (p_tenant, p_request, p_sweep, p_type, p_id, p_fields, erased, kept, kept_until,
      p_actor_type, p_actor_id, p_trace);
END;
$$;

-- The retention sweep's half: erases up to p_limit deferred records whose
-- legal minimum has passed, for one sweep. Returns how many it closed.
CREATE FUNCTION base_audit_erase_due(
  p_tenant bigint, p_sweep bigint, p_limit integer, p_actor_type text, p_actor_id bigint, p_trace text
) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
  due record;
  closed integer := 0;
BEGIN
  IF p_tenant IS NULL OR p_tenant <= 0 OR p_sweep IS NULL OR p_sweep <= 0 OR p_limit NOT BETWEEN 1 AND 1000 THEN
    RAISE EXCEPTION 'invalid audit erasure' USING ERRCODE = '22023';
  END IF;
  FOR due IN SELECT id, auditable_type, auditable_id, fields, subject_identifier FROM base_audit_erasure_deferrals
              WHERE tenant_id = p_tenant AND done_at IS NULL AND due_at <= clock_timestamp()
              ORDER BY due_at, id LIMIT p_limit FOR UPDATE SKIP LOCKED LOOP
    PERFORM base_audit_erase_values(p_tenant, NULL, p_sweep, due.auditable_type, due.auditable_id, due.fields,
      due.subject_identifier, NULL, p_actor_type, p_actor_id, p_trace);
    UPDATE base_audit_erasure_deferrals SET done_at = clock_timestamp() WHERE id = due.id AND done_at IS NULL;
    closed := closed + 1;
  END LOOP;
  RETURN closed;
END;
$$;

REVOKE EXECUTE ON FUNCTION base_audit_erase_values(bigint, bigint, bigint, text, text, text[], boolean, integer, text, bigint, text) FROM PUBLIC;
REVOKE EXECUTE ON FUNCTION base_audit_erase_due(bigint, bigint, integer, text, bigint, text) FROM PUBLIC;

-- The chain link of one row: the previous link, then the row's non-personal
-- facts. Field names are the sorted top-level keys of its values.
CREATE FUNCTION base_audit_keys(a jsonb, b jsonb) RETURNS text LANGUAGE sql IMMUTABLE AS $$
  SELECT coalesce(string_agg(k, ',' ORDER BY k), '') FROM (
    SELECT jsonb_object_keys(CASE WHEN jsonb_typeof(a) = 'object' THEN a ELSE '{}'::jsonb END) AS k
    UNION
    SELECT jsonb_object_keys(CASE WHEN jsonb_typeof(b) = 'object' THEN b ELSE '{}'::jsonb END)) keys
$$;
CREATE FUNCTION base_audit_link(
  prev bytea, p_id bigint, p_tenant bigint, p_at timestamptz, p_actor_type text, p_actor_id bigint,
  p_event text, p_type text, p_record text, p_fields text
) RETURNS bytea LANGUAGE sql STABLE AS $$
  SELECT sha256(prev || convert_to(concat_ws(chr(31), p_id::text, coalesce(p_tenant::text, ''),
    to_char(p_at AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.US'), p_actor_type, p_actor_id::text,
    p_event, coalesce(p_type, ''), coalesce(p_record, ''), p_fields), 'UTF8'))
$$;

-- Links up to p_limit committed, unsealed rows of each chain, oldest ID first.
-- One sealer at a time. Returns the rows linked.
CREATE FUNCTION base_audit_seal(p_limit integer) RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
  r record;
  next_seq bigint;
  link bytea;
  linked integer := 0;
BEGIN
  IF p_limit NOT BETWEEN 1 AND 100000 THEN
    RAISE EXCEPTION 'invalid seal limit' USING ERRCODE = '22023';
  END IF;
  PERFORM pg_advisory_xact_lock(841332);
  SELECT h.seq, h.hash INTO next_seq, link FROM base_audit_chain_heads h WHERE chain = 'mutations' FOR UPDATE;
  FOR r IN SELECT id, tenant_id, occurred_at, actor_type, actor_id, event, auditable_type, auditable_id, old_values, new_values
             FROM base_audit_mutations WHERE chain_seq IS NULL ORDER BY id LIMIT p_limit LOOP
    next_seq := next_seq + 1;
    link := base_audit_link(link, r.id, r.tenant_id, r.occurred_at, r.actor_type, r.actor_id, r.event,
      r.auditable_type, r.auditable_id, base_audit_keys(r.old_values, r.new_values));
    UPDATE base_audit_mutations SET chain_seq = next_seq, chain_hash = link WHERE id = r.id;
    linked := linked + 1;
  END LOOP;
  UPDATE base_audit_chain_heads SET seq = next_seq, hash = link, sealed_at = clock_timestamp() WHERE chain = 'mutations';
  SELECT h.seq, h.hash INTO next_seq, link FROM base_audit_chain_heads h WHERE chain = 'actions' FOR UPDATE;
  FOR r IN SELECT id, tenant_id, occurred_at, actor_type, actor_id, event, payload
             FROM base_audit_actions WHERE chain_seq IS NULL ORDER BY id LIMIT p_limit LOOP
    next_seq := next_seq + 1;
    link := base_audit_link(link, r.id, r.tenant_id, r.occurred_at, r.actor_type, r.actor_id, r.event,
      NULL, NULL, base_audit_keys(r.payload, NULL));
    UPDATE base_audit_actions SET chain_seq = next_seq, chain_hash = link WHERE id = r.id;
    linked := linked + 1;
  END LOOP;
  UPDATE base_audit_chain_heads SET seq = next_seq, hash = link, sealed_at = clock_timestamp() WHERE chain = 'actions';
  RETURN linked;
END;
$$;

-- Recomputes one chain from its first link. `reason` is NULL when it is whole;
-- otherwise `missing` (a link was removed), `changed` (a row's facts differ
-- from its link) or `tail` (the last links were removed), at break_seq.
CREATE FUNCTION base_audit_verify(
  p_chain text, OUT checked bigint, OUT unsealed bigint, OUT break_seq bigint, OUT break_id bigint, OUT reason text
) LANGUAGE plpgsql STABLE AS $$
DECLARE
  r record;
  prev bytea := ''::bytea;
  expect bigint := 0;
  link bytea;
  head_seq bigint;
  head_hash bytea;
BEGIN
  checked := 0;
  IF p_chain = 'mutations' THEN
    FOR r IN SELECT id, tenant_id, occurred_at, actor_type, actor_id, event, auditable_type, auditable_id,
                    old_values, new_values, chain_seq, chain_hash
               FROM base_audit_mutations WHERE chain_seq IS NOT NULL ORDER BY chain_seq LOOP
      expect := expect + 1;
      IF r.chain_seq <> expect THEN
        break_seq := expect; break_id := r.id; reason := 'missing'; RETURN;
      END IF;
      link := base_audit_link(prev, r.id, r.tenant_id, r.occurred_at, r.actor_type, r.actor_id, r.event,
        r.auditable_type, r.auditable_id, base_audit_keys(r.old_values, r.new_values));
      IF link IS DISTINCT FROM r.chain_hash THEN
        break_seq := r.chain_seq; break_id := r.id; reason := 'changed'; RETURN;
      END IF;
      prev := link;
      checked := checked + 1;
    END LOOP;
    SELECT count(*) INTO unsealed FROM base_audit_mutations WHERE chain_seq IS NULL;
  ELSIF p_chain = 'actions' THEN
    FOR r IN SELECT id, tenant_id, occurred_at, actor_type, actor_id, event, payload, chain_seq, chain_hash
               FROM base_audit_actions WHERE chain_seq IS NOT NULL ORDER BY chain_seq LOOP
      expect := expect + 1;
      IF r.chain_seq <> expect THEN
        break_seq := expect; break_id := r.id; reason := 'missing'; RETURN;
      END IF;
      link := base_audit_link(prev, r.id, r.tenant_id, r.occurred_at, r.actor_type, r.actor_id, r.event,
        NULL, NULL, base_audit_keys(r.payload, NULL));
      IF link IS DISTINCT FROM r.chain_hash THEN
        break_seq := r.chain_seq; break_id := r.id; reason := 'changed'; RETURN;
      END IF;
      prev := link;
      checked := checked + 1;
    END LOOP;
    SELECT count(*) INTO unsealed FROM base_audit_actions WHERE chain_seq IS NULL;
  ELSE
    RAISE EXCEPTION 'unknown audit chain %', p_chain USING ERRCODE = '22023';
  END IF;
  SELECT h.seq, h.hash INTO head_seq, head_hash FROM base_audit_chain_heads h WHERE chain = p_chain;
  IF head_seq IS DISTINCT FROM expect OR head_hash IS DISTINCT FROM prev THEN
    break_seq := expect + 1; reason := 'tail';
  END IF;
END;
$$;
REVOKE EXECUTE ON FUNCTION base_audit_seal(integer) FROM PUBLIC;
