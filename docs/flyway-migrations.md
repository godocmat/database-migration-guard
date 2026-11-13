# Flyway SQL migrations

Guard inventories UTF-8 PostgreSQL SQL migrations for the Flyway OSS 13.12.2 filesystem and JDBC profile. Configure the same migration locations and execution settings in Guard and Flyway. Add each nested directory as a Guard root, including intermediate directories. Flyway discovers recursively, so an unlisted child directory makes coverage incomplete. Linked paths and nonregular inputs are outside this profile. [Native runner evidence](../fixtures/dmg-09/flyway/README.md)

~~~toml
schema_version = 1
target = 18
roots = ["sql"]

[adapter]
kind = "flyway"
execute_in_transaction = true
mixed = false
transactional_lock = true
~~~

These values are the defaults. They correspond to Flyway's `executeInTransaction`, `mixed`, and `postgresql.transactional.lock`. The profile uses default migration names and SQL suffixes, with grouping, out-of-order application, and JDBC batching disabled. Guard takes its settings from the trusted project configuration. Mirror effective Flyway configuration, including any deployment-time overrides. Custom naming, classpath resources, script migrations, callbacks, and baseline migrations require separate coverage. A recognizable unsupported input under a configured root produces an incomplete result. [Flyway execution settings](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-execute-in-transaction-setting) · [Flyway location settings](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-locations-setting)

## Identities and source

- `V1_02__example.sql` is a versioned migration with canonical version `1.2`. Underscores separate version components. Leading component zeros and trailing zero components do not change the identity. Components compare by numeric magnitude.
- `R__refresh_summary.sql` is a repeatable with the description `refresh summary`. Repeatables follow versioned migrations and sort by the normalized description in Java string order. A changed repeatable remains in the Git integrity comparison and is selected for analysis.
- `U1.2__undo.sql` is a separate Down record for the same version. Undo SQL is available in the inventory and integrity comparison. Forward analysis excludes it. [Flyway undo](https://documentation.red-gate.com/flyway/reference/commands/undo)

The adapter retains original bytes for SHA-256, including a UTF-8 BOM and CRLF line endings. A leading BOM is skipped only in the SQL source segment. Flyway's applied-history CRC32 is a separate checksum. Duplicate canonical versions or repeatable descriptions make coverage incomplete. A Flyway repeatable change can execute in a database that already contains previously applied repeatable objects. Static order describes a clean forward run. [Native history and checksum observations](../fixtures/dmg-09/flyway/README.md#discovery-and-history)

## Transaction coverage

Guard parses the full migration and models individual JDBC statements. Ordinary supported statements use a file transaction when `execute_in_transaction = true`. Flyway runs files containing only supported concurrent index operations without that wrapper. With `mixed = false` and `execute_in_transaction = true`, Flyway rejects a file that combines ordinary and concurrent index statements. Guard reports that runner rejection as incomplete coverage. With `mixed = true` or `execute_in_transaction = false`, supported mixed files run unwrapped.

Concurrent index operations require `transactional_lock = false` for complete Guard coverage. This models the native session-level advisory lock. With the default lock, the runner can wait and leave an invalid index after a timeout. The native probe verified both settings on PostgreSQL 17.12 and 18.7. [Native execution and partial state](../fixtures/dmg-09/flyway/README.md#execution-and-partial-state) · [PostgreSQL advisory lock setting](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-postgresql-namespace/flyway-postgresql-transactional-lock-setting)

Guard reports incomplete coverage when a file contains unresolved Flyway placeholders, authored SQL transaction commands, or unsupported SQL that can affect execution. A `<script>.sql.conf` sidecar also makes coverage incomplete, including a Git change that adds a sidecar without changing SQL. Its settings can affect execution independently of the migration digest and require revision-bound evidence. [Flyway script configuration](https://documentation.red-gate.com/flyway/reference/script-configuration)
