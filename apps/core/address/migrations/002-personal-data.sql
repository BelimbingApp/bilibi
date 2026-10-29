-- The address kind's personal fields (base_personal_data_kinds, ADR 05).
INSERT INTO base_personal_data_kinds(kind, module, fields, subject_identifier, person)
    VALUES ('address', 'core/address',
            ARRAY['label', 'phone', 'line1', 'line2', 'line3', 'locality', 'postcode', 'raw_input', 'source_ref'],
            false, false);
