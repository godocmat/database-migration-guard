ALTER TABLE app.events ADD CONSTRAINT events_account_fk FOREIGN KEY (account_id) REFERENCES app.accounts(id) NOT VALID;
