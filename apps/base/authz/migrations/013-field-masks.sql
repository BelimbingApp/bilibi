-- Masking (ADR 05): a field restriction either withholds the field, as
-- before, or masks it: the restricted roles see a partial value shaped by the
-- field's personal kind and may reveal it with a reason. A restricted field
-- has one mode for all its roles; a role that withholds wins over one that masks.
ALTER TABLE field_restrictions
    ADD COLUMN mode text NOT NULL DEFAULT 'withhold' CHECK (mode IN ('withhold', 'mask'));
