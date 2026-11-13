INSERT INTO app.accounts (id, email, legacy_code, active) VALUES
  (1, 'one@example.invalid', 'LEGACY-1', true),
  (2, 'two@example.invalid', 'LEGACY-2', true),
  (3, 'three@example.invalid', 'LEGACY-3', false);

INSERT INTO app.events (id, account_id) VALUES (1, 1), (2, 2), (3, 3);
