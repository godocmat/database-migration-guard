# Read-only PostgreSQL native history

Use the history reader with an authorized TLS target, an opaque target ID, and the SHA-256 forward inventory binding. Supply the exact schema and history table chosen by the operator. Guard accepts quoted identifiers and does not discover an endpoint, a schema, or a history source. Plain SQL uses an applied manifest. [Evidence and inventory binding](inspection-evidence.md#native-migration-history) · [Transport](research/postgresql-transport.md)

~~~rust
let history = HistoryOptions::new(expected_major, adapter, source)?;
let evidence = target
    .inspect_history(target_id, inventory_digest, &history, timeouts)
    .await?;
~~~

| Supported PostgreSQL runner profile | Usual history source | Required SELECT columns |
| --- | --- | --- |
| SQLx 0.8.6 | public._sqlx_migrations | version, success, checksum |
| Flyway 13.12.2 | public.flyway_schema_history | installed_rank, version, description, type, success, checksum |
| Goose 3.30.1 | public.goose_db_version | id, version_id, is_applied |
| Supabase CLI 3.0.0 | supabase_migrations.schema_migrations | version |

The table must use ordinary heap storage, the projected column types of the pinned runner, and the runner's single-column primary key on its version or rank. The login needs database CONNECT, schema USAGE, access to the fixed system catalog queries and functions, and SELECT on the projected columns. The reader accepts column-only grants. It rejects row-level security and rules on the source, views, foreign tables, partitioned tables, and individual partitions. The read uses ONLY to exclude classic inheritance children. Use a history schema and table protected from untrusted writers and DDL. [SQLx table DDL](https://github.com/launchbadge/sqlx/blob/v0.8.6/sqlx-postgres/src/migrate.rs#L104-L120) · [Flyway table DDL](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-database/flyway-database-postgresql/src/main/java/org/flywaydb/database/postgresql/PostgreSQLDatabase.java#L52-L86) · [Goose table DDL](https://github.com/pressly/goose/blob/v3.30.1/internal/dialects/postgres.go#L17-L25) · [Supabase history DDL](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-history.ts#L23-L31)

The session uses verified TLS and a bounded READ COMMITTED READ ONLY transaction with a catalog-only search path. It resolves the authorized namespace and table through parameterized catalog lookups, takes a namespace AccessShare lock through PostgreSQL's object-address function, and prepares a column reference to take a relation AccessShare lock. It checks the prepared column's origin OID and rechecks source metadata before selecting history data. The history rows come from one statement snapshot. Lock and statement limits apply to catalog and table reads, and a client deadline bounds the connection and transaction. [PostgreSQL object addresses](https://www.postgresql.org/docs/18/functions-info.html#FUNCTIONS-INFO-OBJECT) · [Read Committed snapshots](https://www.postgresql.org/docs/18/transaction-iso.html#XACT-READ-COMMITTED) · [Timeout settings](https://www.postgresql.org/docs/18/runtime-config-client.html#RUNTIME-CONFIG-CLIENT-STATEMENT)

The history projection defaults to 2,048 rows and accepts a caller limit from 1 to 8,192. It sorts by the native version or event key and requests one additional record to detect truncation. The server limits output fields before sending them. SQLx exposes the exact 48 checksum bytes when valid. Flyway retains its signed CRC32, reads the description only for a null-version record, and distinguishes a ranked SQL undo event from a forward event. Goose discards the valid version-zero bootstrap row and retains legacy false events as unknown. Supabase reads version strings only and preserves leading zeros. [Flyway undo event type](https://documentation.red-gate.com/flyway/deploying-database-changes-using-flyway/implementing-a-roll-back-strategy/tutorial-roll-back-using-undo-migrations) · [Runner reconciliation](migration-formats.md#native-applied-history)

Missing sources, denied reads, unsupported relation kinds, and deadlines return unavailable evidence. Truncation, malformed rows, and unsupported Flyway event types such as BASELINE or DELETE leave the projection incomplete. A checksum gap on the latest active SQLx or Flyway event is scoped to that migration. The evidence excludes connection details, SQL source text, arbitrary runner descriptions, and original server errors.

## Verification

The disposable fixture passed on PostgreSQL 15.20, 16.16, 17.12, and 18.7 with verified TLS. It used exact runner projection shapes, column-only grants, and synthetic history events. It exercised clean and dirty SQLx rows, Flyway versioned and repeatable events and a synthetic undo, Goose bootstrap and legacy false records, and exact Supabase versions. It verified empty versus unavailable sources, hidden columns, oversized fields, row truncation, denied privileges, RLS, table shape, view and partition rejection, lock timeouts, statement cancellation, client deadlines, and rejection of a same-name table replacement during lock acquisition. [Fixture](../fixtures/dmg-10/catalog/history_probe.py) · [Server executable hashes](postgresql-catalog.md#verification)

Run as a regular Unix user with PostgreSQL built with OpenSSL, OpenSSL, Python 3.11 or newer, and localhost resolving to loopback. Use the test executable path printed by Cargo.

~~~sh
cargo test --locked -p dmg-inspect --no-run
python3 -B fixtures/dmg-10/catalog/history_probe.py \
  --pg-bin /path/to/postgresql-18.7-tls/bin --test-binary /path/to/dmg_inspect-test
~~~
