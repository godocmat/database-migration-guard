CREATE TABLE app.probe (id bigint PRIMARY KEY, flag boolean);
CREATE INDEX probe_flag_idx ON app.probe(flag);
