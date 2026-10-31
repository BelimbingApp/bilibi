-- One-time exception: preserve existing people using the same slug and suffix
-- rules as core/person's available_code and core/party's slug_code.
ALTER TABLE persons ADD COLUMN code text;
ALTER TABLE persons ADD UNIQUE (tenant_id, code);
DO $$
DECLARE
    person record;
    ch text;
    slug text;
    base text;
    candidate text;
    separator boolean;
    suffix bigint;
    i integer;
BEGIN
    FOR person IN SELECT id, tenant_id, name FROM persons ORDER BY tenant_id, id LOOP
        slug := '';
        separator := false;
        FOR i IN 1..char_length(person.name) LOOP
            ch := substr(person.name, i, 1);
            IF ch = '@' THEN
                IF separator AND slug <> '' THEN slug := slug || '_'; END IF;
                slug := slug || 'at';
                separator := true;
                CONTINUE;
            ELSIF ch IN ('-', '_') OR (ascii(ch) BETWEEN 9 AND 13 OR ch = ' ') OR ch IN (U&'\0085', U&'\00A0', U&'\1680', U&'\2000', U&'\2001', U&'\2002', U&'\2003', U&'\2004', U&'\2005', U&'\2006', U&'\2007', U&'\2008', U&'\2009', U&'\200A', U&'\2028', U&'\2029', U&'\202F', U&'\205F', U&'\3000') THEN
                separator := slug <> '';
            ELSE
                ch := translate(ch, 'ÀÁÂÃÄÅĀĂĄàáâãäåāăąÇĆĈĊČçćĉċčĎĐďđÈÉÊËĒĔĖĘĚèéêëēĕėęěĜĞĠĢĝğġģĤĦĥħÌÍÎÏĨĪĬĮİìíîïĩīĭįıĴĵĶķĹĻĽĿŁĺļľŀłÑŃŅŇñńņňÒÓÔÕÖØŌŎŐòóôõöøōŏőŔŖŘŕŗřŚŜŞŠśŝşšŢŤŦţťŧÙÚÛÜŨŪŬŮŰŲùúûüũūŭůűųŴŵÝŶŸýÿŷŹŻŽźżž', 'AAAAAAAAAaaaaaaaaaCCCCCcccccDDddEEEEEEEEEeeeeeeeeeGGGGggggHHhhIIIIIIIIIiiiiiiiiiJjKkLLLLLlllllNNNNnnnnOOOOOOOOOoooooooooRRRrrrSSSSssssTTTtttUUUUUUUUUUuuuuuuuuuuWwYYYyyyZZZzzz');
                IF strpos('ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789', ch) > 0 THEN
                    IF separator AND slug <> '' THEN slug := slug || '_'; END IF;
                    slug := slug || translate(ch, 'ABCDEFGHIJKLMNOPQRSTUVWXYZ', 'abcdefghijklmnopqrstuvwxyz');
                    separator := false;
                END IF;
            END IF;
        END LOOP;
        base := left(slug, 230);
        IF base = '' THEN base := 'person'; END IF;
        candidate := base;
        IF EXISTS (SELECT 1 FROM persons WHERE tenant_id = person.tenant_id AND code = candidate) THEN
            SELECT coalesce(max(id), 0) + 1 INTO suffix FROM persons WHERE tenant_id = person.tenant_id;
            LOOP
                candidate := base || '_' || suffix;
                EXIT WHEN NOT EXISTS (SELECT 1 FROM persons WHERE tenant_id = person.tenant_id AND code = candidate);
                suffix := suffix + 1;
            END LOOP;
        END IF;
        UPDATE persons SET code = candidate WHERE id = person.id AND tenant_id = person.tenant_id;
    END LOOP;
END;
$$;

ALTER TABLE persons ALTER COLUMN code SET NOT NULL;
ALTER TABLE persons ADD CHECK (length(code) BETWEEN 1 AND 255);

-- A party's code survives name changes and retirement.
CREATE FUNCTION person_fixed_code() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    IF NEW.code IS DISTINCT FROM OLD.code THEN
        RAISE EXCEPTION 'A person code cannot change' USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;
CREATE TRIGGER person_fixed_code BEFORE UPDATE ON persons
    FOR EACH ROW EXECUTE FUNCTION person_fixed_code();

ALTER TABLE persons DROP COLUMN search_vector;
ALTER TABLE persons ADD COLUMN search_vector tsvector
    GENERATED ALWAYS AS (setweight(to_tsvector('simple', name || ' ' || code), 'A')) STORED;
CREATE INDEX persons_search ON persons USING gin (search_vector);
CREATE INDEX persons_code_prefix ON persons (tenant_id, lower(code) text_pattern_ops);
