ALTER TABLE app.accounts ADD CONSTRAINT accounts_email_present CHECK (email IS NOT NULL);
