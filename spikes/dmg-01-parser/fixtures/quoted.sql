/* π; /* nested; */ lead */
ALTER TABLE IF EXISTS "App"."café" ADD COLUMN IF NOT EXISTS "Notes" text, DROP COLUMN IF EXISTS "Old" RESTRICT;
-- tail;
SELECT 'é;'