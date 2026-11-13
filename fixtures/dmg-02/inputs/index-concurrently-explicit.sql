BEGIN;
CREATE INDEX CONCURRENTLY accounts_legacy_idx ON app.accounts(legacy_code);
COMMIT;
