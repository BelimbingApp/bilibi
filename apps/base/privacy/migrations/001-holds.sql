-- Legal holds on a person or one record (ADR 05). A hold stops erasure and
-- retention sweeps for what it names; artifact holds stay authoritative for
-- documents. Each change is one append-only event row whose time PostgreSQL
-- stamps. The facade refuses first; these constraints and triggers refuse
-- whatever bypasses it.
CREATE TABLE base_privacy_holds (
    id bigserial PRIMARY KEY,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    -- Exactly one target: a person, or a record kind and ID.
    person_id bigint CHECK (person_id > 0),
    record_kind varchar(255) CHECK (length(record_kind) >= 1),
    record_id bigint CHECK (record_id > 0),
    reason varchar(255) NOT NULL CHECK (length(reason) >= 1),
    placed_by bigint NOT NULL CHECK (placed_by > 0),
    placed_at timestamptz NOT NULL,
    released_at timestamptz,
    released_by bigint CHECK (released_by > 0),
    release_reason varchar(255) CHECK (length(release_reason) >= 1),
    CHECK ((person_id IS NULL) <> (record_kind IS NULL)),
    CHECK ((record_kind IS NULL) = (record_id IS NULL)),
    CHECK ((released_at IS NULL) = (released_by IS NULL)),
    CHECK (released_at IS NOT NULL OR release_reason IS NULL)
);
CREATE UNIQUE INDEX base_privacy_holds_person_active ON base_privacy_holds(tenant_id,person_id)
    WHERE released_at IS NULL AND person_id IS NOT NULL;
CREATE UNIQUE INDEX base_privacy_holds_record_active ON base_privacy_holds(tenant_id,record_kind,record_id)
    WHERE released_at IS NULL AND record_kind IS NOT NULL;
CREATE INDEX base_privacy_holds_tenant ON base_privacy_holds(tenant_id,id);

-- A hold is placed once and released once; nothing else about it changes and
-- no row is removed.
CREATE FUNCTION base_privacy_holds_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.placed_at := clock_timestamp();
        IF NEW.released_at IS NOT NULL THEN
            RAISE EXCEPTION 'a new hold cannot be released'
                USING ERRCODE = 'check_violation', CONSTRAINT = 'base_privacy_holds_guard';
        END IF;
        RETURN NEW;
    END IF;
    IF TG_OP = 'DELETE' OR TG_OP = 'TRUNCATE' THEN
        RAISE EXCEPTION 'base_privacy_holds rows are never removed'
            USING ERRCODE = 'check_violation', CONSTRAINT = 'base_privacy_holds_guard';
    END IF;
    IF OLD.released_at IS NOT NULL
       OR NEW.released_at IS NULL
       OR (NEW.tenant_id, NEW.person_id, NEW.record_kind, NEW.record_id, NEW.reason, NEW.placed_by, NEW.placed_at)
          IS DISTINCT FROM
          (OLD.tenant_id, OLD.person_id, OLD.record_kind, OLD.record_id, OLD.reason, OLD.placed_by, OLD.placed_at) THEN
        RAISE EXCEPTION 'hold % can only be released, once', OLD.id
            USING ERRCODE = 'check_violation', CONSTRAINT = 'base_privacy_holds_guard';
    END IF;
    NEW.released_at := clock_timestamp();
    RETURN NEW;
END $$;
CREATE TRIGGER base_privacy_holds_guard BEFORE INSERT OR UPDATE OR DELETE ON base_privacy_holds
    FOR EACH ROW EXECUTE FUNCTION base_privacy_holds_guard();
CREATE TRIGGER base_privacy_holds_no_truncate BEFORE TRUNCATE ON base_privacy_holds
    FOR EACH STATEMENT EXECUTE FUNCTION base_privacy_holds_guard();

CREATE TABLE base_privacy_hold_events (
    id bigserial PRIMARY KEY,
    hold_id bigint NOT NULL REFERENCES base_privacy_holds(id) ON DELETE RESTRICT,
    tenant_id bigint NOT NULL REFERENCES tenants(id) ON DELETE RESTRICT,
    event text NOT NULL CHECK (event IN ('placed','released')),
    reason varchar(255) NOT NULL CHECK (length(reason) >= 1),
    actor_id bigint NOT NULL CHECK (actor_id > 0),
    impersonator_id bigint CHECK (impersonator_id > 0),
    trace_id text,
    -- Stamped by the trigger below; the application cannot supply it.
    recorded_at timestamptz NOT NULL
);
CREATE INDEX base_privacy_hold_events_hold ON base_privacy_hold_events(hold_id,id);

CREATE FUNCTION base_privacy_hold_events_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'INSERT' THEN
        NEW.recorded_at := clock_timestamp();
        RETURN NEW;
    END IF;
    RAISE EXCEPTION 'base_privacy_hold_events is append-only'
        USING ERRCODE = 'check_violation', CONSTRAINT = 'base_privacy_hold_events_append_only';
END $$;
CREATE TRIGGER base_privacy_hold_events_append_only BEFORE INSERT OR UPDATE OR DELETE
    ON base_privacy_hold_events FOR EACH ROW EXECUTE FUNCTION base_privacy_hold_events_guard();
CREATE TRIGGER base_privacy_hold_events_no_truncate BEFORE TRUNCATE
    ON base_privacy_hold_events FOR EACH STATEMENT EXECUTE FUNCTION base_privacy_hold_events_guard();
