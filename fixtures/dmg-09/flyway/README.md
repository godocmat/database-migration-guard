# Flyway SQL runner parity

The probe ran the Linux x86-64 Flyway OSS Edition 13.12.2 release on Ubuntu 24.04.4 in WSL2 with Python 3.12.3. It used source-built PostgreSQL 17.12 and 18.7 in isolated Linux network and IPC namespaces. The release archive SHA-256 was `bfb75185b58ff5003e1798522617d9fa0f1467ebbab98b89e8971f778838636f`. [Flyway release](https://github.com/flyway/flyway/releases/tag/flyway-13.12.2) · [PostgreSQL 17.12 source](https://www.postgresql.org/ftp/source/v17.12/) · [PostgreSQL 18.7 source](https://www.postgresql.org/ftp/source/v18.7/)

[PostgreSQL 17.12 observations](native-17.12.jsonl) and [PostgreSQL 18.7 observations](native-18.7.jsonl) record native command exits, Flyway errors, SQLSTATE when available, migration and sidecar digests, ordered history rows, and selected schema state. The [probe](native_probe.py) contains the exact SQL and checks every recorded behavior. The two majors produced the same outcomes.

## Discovery and history

- Flyway applied `V01.0`, `V1_2.00`, `V1.10`, `V2`, and `V10` in numeric component order. It accepted the upper-case `.SQL` suffix on the last file. It treated `01.2` and `1_02.0` as duplicate versions. Numeric comparison removes leading zeros in each component and trailing zero components. The history display preserves those zeros and converts underscores in versions to dots. [Version parser](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/api/MigrationVersion.java) · [Filename parser](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resource/ResourceNameParser.java)
- Repeatables applied after the versioned migrations. `R__alpha_z.sql` applied before `R__alpha0.sql` because their descriptions sort as `alpha z` and `alpha0`. `R__shared name.sql` and `R__shared_name.sql` produced a duplicate repeatable error. Flyway uses Java string order for descriptions. The original repeatable and its revised run had distinct checksums and retained both history rows with a null version. [Migration comparator](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resolver/ResolvedMigrationComparator.java) · [Repeatable migrations](https://documentation.red-gate.com/flyway/flyway-concepts/migrations/repeatable-migrations)
- Replacing LF with CRLF in an already applied versioned script changed its original-byte SHA-256. Flyway applied zero migrations and retained the same history checksum. For these untemplated UTF-8 scripts the signed CRC32 over decoded lines without line terminators matched every native history row. Placeholder expansion and Flyway's additional checksum modes need separate coverage. [Native checksum implementation](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resolver/ChecksumCalculator.java) · [SQL migration resolver](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resolver/sql/SqlMigrationResolver.java)

## Execution and partial state

Each case starts from a fresh database after `V1__base.sql` creates and seeds one table. A failing insert reuses the existing primary key. A session-level advisory lock is selected with `-postgresql.transactional.lock=false`.

| Candidate and runner settings | Native outcome | Candidate history |
| --- | --- | --- |
| Add column, then failing insert with defaults | Exit 1, `23505`. Column rolled back. | No row |
| Add column, then failing insert with global `executeInTransaction=false` | Exit 1, `23505`. Column persisted. | No row with the default transactional advisory lock. Failed row with a session-level lock. |
| Single concurrent index, session-level advisory lock, default execution settings | Exit 0. Index valid. | Successful row |
| Single concurrent index, default transactional advisory lock and 2-second lock timeout | Exit 1, `55P03`. Invalid index remained. | No row |
| Add column and concurrent index, session-level advisory lock and default `mixed=false` | Exit 1. Flyway rejected mixed statement modes before either statement ran. | No row |
| Add column, concurrent index, then failing insert, session-level advisory lock and `mixed=true` | Exit 1, `23505`. Column and valid index persisted. | Failed row |
| Add column and concurrent index, session-level advisory lock and sidecar `executeInTransaction=false` | Exit 0. Column and valid index persisted. | Successful row |
| Single concurrent index, session-level advisory lock and sidecar `executeInTransaction=true` | Exit 1, `25001`. Index absent. | No row |

Flyway detects some statements as nontransactional, including concurrent index creation and removal. A default file containing only such statements runs without a transaction wrapper. With the defaults `mixed=false` and `executeInTransaction=true`, a file that contains both kinds is rejected during runner parsing. A recognized per-script override takes precedence. Setting `mixed=true` lets the entire mixed file run unwrapped. The advisory lock has its own transaction context. The same unwrapped migration can leave partial schema changes with either advisory lock setting, while failed history can roll back with the default lock. [PostgreSQL parser](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-database/flyway-database-postgresql/src/main/java/org/flywaydb/database/postgresql/PostgreSQLParser.java) · [SQL script execution settings](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/sqlscript/ParserSqlScript.java) · [PostgreSQL advisory lock setting](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-postgresql-namespace/flyway-postgresql-transactional-lock-setting)

## Adapter scope

A `<migration>.sql.conf` sidecar changes execution independently of the original SQL digest. A scan that supports it needs to include the sidecar in selection and evidence. Use the exact default prefixes, separator, and suffixes or explicitly declare a different supported naming profile. Flyway filesystem locations recurse, so flat discovery requires an explicit root per included directory or incomplete coverage for a layout with unresolved directories. [Script configuration](https://documentation.red-gate.com/flyway/reference/script-configuration) · [Location discovery](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-locations-setting)

These probes cover forward versioned and repeatable scripts, default naming, and the transaction settings listed above. Undo files have separate direction and are outside forward execution. Undo is a Teams feature and the OSS probe does not run it. Callback SQL and other lifecycle hooks, placeholder expansion including `${flyway:timestamp}`, additional sidecar settings, grouped transactions, and out-of-order application remain outside the verified adapter subset. Each of these can change what the runner executes and needs an explicit coverage decision when encountered. [Undo](https://documentation.red-gate.com/flyway/reference/commands/undo) · [Callback events](https://documentation.red-gate.com/flyway/reference/callback-events) · [Placeholders](https://documentation.red-gate.com/flyway/flyway-concepts/migrations/migration-placeholders) · [Grouping](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-group-setting) · [Out-of-order execution](https://documentation.red-gate.com/flyway/reference/configuration/flyway-namespace/flyway-out-of-order-setting)

## Reproduce

Run as a regular Unix user with Python 3.9 or newer and the pinned native binaries. Each invocation creates a temporary cluster that listens on loopback with a generated password and a private Unix socket directory. The harness stops and removes the cluster before printing JSON Lines. The disposable server and Flyway use only synthetic fixtures. Use isolated network and IPC namespaces to reproduce the recorded environment.

```sh
python3 -B fixtures/dmg-09/flyway/native_probe.py \
  --flyway /path/to/flyway-13.12.2/flyway --pg-bin /path/to/pg-17.12/bin
python3 -B fixtures/dmg-09/flyway/native_probe.py \
  --flyway /path/to/flyway-13.12.2/flyway --pg-bin /path/to/pg-18.7/bin
```
