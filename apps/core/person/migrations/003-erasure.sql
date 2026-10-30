-- Erasure (ADR 05): when a data-subject erasure anonymised the person. Its
-- name is then "[erased]" and its email and phone are gone; this column tells
-- an erased contact from one that never had them.
ALTER TABLE persons ADD COLUMN erased_at timestamptz;
INSERT INTO base_personal_data_kinds(kind, module, fields, subject_identifier, person)
    VALUES ('person', 'core/person', ARRAY['name', 'email', 'phone'], true, true);
