DO $migration$
BEGIN
  EXECUTE format('ALTER TABLE %I.%I ADD COLUMN %I text', 'app', 'accounts', 'label');
END;
$migration$;
