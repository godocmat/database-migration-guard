ALTER TABLE app.accounts ADD COLUMN observed_at timestamptz DEFAULT clock_timestamp();
