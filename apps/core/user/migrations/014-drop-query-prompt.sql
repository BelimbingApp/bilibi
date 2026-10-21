-- The saved-query editor uses SQL directly; its unused AI prompt is removed.
-- Keep every saved query, its SQL and optimistic-lock version.
ALTER TABLE user_database_queries DROP COLUMN prompt;
