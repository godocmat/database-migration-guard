ALTER TABLE app.accounts
  DROP COLUMN note,
  DROP COLUMN label,
  ADD COLUMN legacy_code text;
