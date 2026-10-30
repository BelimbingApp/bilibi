-- A workspace address carries its complete tile tree.
ALTER TABLE user_pins DROP CONSTRAINT user_pins_url_check;
ALTER TABLE user_pins ADD CONSTRAINT user_pins_url_check CHECK (char_length(url) BETWEEN 1 AND 8192);
