-- Erasure (ADR 05): when a data-subject erasure anonymised the person. Its
-- name is then "[erased]", its email and phone are gone, and its code, which
-- may contain personal data, becomes "erased-<id>"; this column tells an erased contact
-- from one that never had them.
ALTER TABLE persons ADD COLUMN erased_at timestamptz;
INSERT INTO base_personal_data_kinds(kind, module, fields, subject_identifier, person)
    VALUES ('person', 'core/person', ARRAY['name', 'code', 'email', 'phone'], true, true);

-- A code still never changes, except that once, by the erasure itself.
CREATE OR REPLACE FUNCTION person_fixed_code() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code IS DISTINCT FROM OLD.code
       AND NOT (NEW.erased_at IS NOT NULL AND NEW.code = 'erased-' || NEW.id) THEN
        RAISE EXCEPTION 'A person code cannot change' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
