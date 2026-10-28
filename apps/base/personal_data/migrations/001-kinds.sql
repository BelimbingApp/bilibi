-- Record kinds (ADR 05) in the database: the personal fields each owning
-- module declares for its kind, written by that module's own migration. Audit
-- erasure clears only fields declared here, so code that names another field
-- is refused by the database too, and `bilibi_audit::erase_record` refuses a
-- kind whose code and row disagree. A kind with no personal field needs no row.
-- Rows are never removed. A declaration changes only in a transaction that
-- also records a migration in the ledger, so the running application cannot
-- widen what erasure may clear.
CREATE TABLE base_personal_data_kinds (
    kind text PRIMARY KEY CHECK (length(kind) BETWEEN 1 AND 255),
    module text NOT NULL CHECK (length(module) BETWEEN 1 AND 255),
    fields text[] NOT NULL CHECK (cardinality(fields) <= 64),
    subject_identifier boolean NOT NULL,
    person boolean NOT NULL
);

CREATE FUNCTION base_personal_data_kinds_guard() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF TG_OP = 'UPDATE' THEN
    IF NEW.kind = OLD.kind AND NEW.module = OLD.module THEN
      RETURN NEW;
    END IF;
    RAISE EXCEPTION 'a record kind stays with its kind name and owning module' USING ERRCODE = '42501';
  END IF;
  RAISE EXCEPTION 'record kinds are never removed' USING ERRCODE = '42501';
END;
$$;
CREATE TRIGGER base_personal_data_kinds_guard BEFORE UPDATE OR DELETE ON base_personal_data_kinds
    FOR EACH ROW EXECUTE FUNCTION base_personal_data_kinds_guard();
CREATE TRIGGER base_personal_data_kinds_truncate_guard BEFORE TRUNCATE ON base_personal_data_kinds
    FOR EACH STATEMENT EXECUTE FUNCTION base_personal_data_kinds_guard();

-- Checked at commit: the same transaction wrote a migration ledger row.
-- Shared with other declaration tables that only migrations may change.
CREATE FUNCTION base_personal_data_in_migration() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF NOT EXISTS (SELECT 1 FROM bilibi_migrations WHERE xmin = pg_current_xact_id()::xid) THEN
    RAISE EXCEPTION '% changes only in a migration', TG_TABLE_NAME USING ERRCODE = '42501';
  END IF;
  RETURN NULL;
END;
$$;
CREATE CONSTRAINT TRIGGER base_personal_data_kinds_migration AFTER INSERT OR UPDATE ON base_personal_data_kinds
    DEFERRABLE INITIALLY DEFERRED FOR EACH ROW EXECUTE FUNCTION base_personal_data_in_migration();
