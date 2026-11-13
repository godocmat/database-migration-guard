-- no-transaction
ALTER TABLE app.accounts ADD COLUMN note text DEFAULT $m$é;🧪$m$; ALTER TABLE app.accounts
  ADD COLUMN label text,
  DROP COLUMN legacy_code;
