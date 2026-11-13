-- Create the synthetic accounts and events schema.
CREATE SCHEMA app;

CREATE TABLE app.accounts (
  id bigint PRIMARY KEY,
  email text,
  legacy_code text,
  active boolean
);

CREATE TABLE app.events (
  id bigint,
  account_id bigint
) PARTITION BY RANGE (id);

CREATE TABLE app.events_1_999 PARTITION OF app.events
  FOR VALUES FROM (1) TO (1000);
