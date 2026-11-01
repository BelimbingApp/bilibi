-- Two product glyphs lost their prefix. Saved values follow the renamed icon ids.
UPDATE notifications SET icon = 'impersonate' WHERE icon = 'bilimbi-impersonate';
UPDATE notifications SET icon = 'pin' WHERE icon = 'bilimbi-pin';

UPDATE user_pins SET icon = 'impersonate' WHERE icon = 'bilimbi-impersonate';
UPDATE user_pins SET icon = 'pin' WHERE icon = 'bilimbi-pin';

UPDATE user_database_queries SET icon = 'impersonate' WHERE icon = 'bilimbi-impersonate';
UPDATE user_database_queries SET icon = 'pin' WHERE icon = 'bilimbi-pin';
