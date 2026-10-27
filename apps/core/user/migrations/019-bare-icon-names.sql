-- Bilimbi's stored notification/pin/query icons now use the same bare names
-- as menu contributions. Migrate saved values once; the renderer has no aliases.
UPDATE notifications
   SET icon = CASE icon WHEN 'hero-impersonate' THEN 'bilimbi-impersonate'
                        ELSE substr(icon, 6) END
 WHERE left(icon, 5) = 'hero-';

UPDATE user_pins
   SET icon = CASE icon WHEN 'hero-impersonate' THEN 'bilimbi-impersonate'
                        ELSE substr(icon, 6) END
 WHERE left(icon, 5) = 'hero-';

UPDATE user_database_queries
   SET icon = CASE icon WHEN 'hero-impersonate' THEN 'bilimbi-impersonate'
                        ELSE substr(icon, 6) END
 WHERE left(icon, 5) = 'hero-';
