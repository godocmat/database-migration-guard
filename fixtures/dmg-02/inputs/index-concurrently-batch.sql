-- no-transaction
ALTER TABLE app.accounts ADD COLUMN code_copy text;
CREATE INDEX CONCURRENTLY accounts_code_copy_idx ON app.accounts(code_copy);
