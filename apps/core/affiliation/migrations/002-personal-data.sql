-- The affiliation kind's personal fields (base_personal_data_kinds, ADR 05).
INSERT INTO base_personal_data_kinds(kind, module, fields, subject_identifier, person)
    VALUES ('affiliation', 'core/affiliation', ARRAY['title', 'work_email', 'work_phone'], false, false);
