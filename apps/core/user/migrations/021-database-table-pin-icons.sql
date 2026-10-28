-- Pins of database table pages were stored without an icon; give them the
-- database icon the pinned bar draws for the page. Migrate saved values once.
UPDATE user_pins
   SET icon = 'database'
 WHERE (icon IS NULL OR icon = '')
   AND (url = '/system/database-tables'
        OR url LIKE '/system/database-tables/%'
        OR url LIKE '/system/database-tables?%');
