# Goose v3.30.1 migration parity

The [native probe](native_probe.py) ran 19 synthetic cases on each of PostgreSQL 17.12 and 18.7. Each case used its own database and migration directory in a disposable Unix-socket-only cluster. The PostgreSQL server logs identify each Simple Query containing migration SQL. The probe asserts the CLI exit, SQLSTATE, submitted queries, surviving columns, index validity, and history as applicable. Two runs on each server produced byte-identical observations. The [17.12 observations](native-17.12.jsonl) and [18.7 observations](native-18.7.jsonl) differ only in their headers.

## Discovery and history

- Goose reads immediate lowercase `.sql` files and orders the positive signed 64-bit integer prefix before the first underscore. The probe applied versions 2, 3, and 10 in numeric order, including a file named `+3_.sql`. Uppercase `.SQL` and a file in a child directory were ignored. Aliases `02` and `2` produced a duplicate-version error before any SQL ran. A malformed SQL filename and an unregistered Go migration also failed. [Pinned discovery](https://github.com/pressly/goose/blob/v3.30.1/migrate.go) · [Version parsing](https://github.com/pressly/goose/blob/v3.30.1/migration.go)
- One CRLF file carried both directions. Up added a column with a Unicode default, and Down removed it and deleted the applied version from history. A file with only Up also allowed Down, which executed no migration SQL, kept the table, and deleted the history row. [Pinned parser](https://github.com/pressly/goose/blob/v3.30.1/internal/sqlparser/parser.go) · [Execution and history](https://github.com/pressly/goose/blob/v3.30.1/migration_sql.go)
- The default PostgreSQL history table has a bootstrap row at version 0 and records version and applied state without a file checksum. Editing an applied file changed its original-byte SHA-256. A repeated Up kept the prior history and column value. A failed no-transaction run can also leave committed SQL without a version row. Compare file bytes using trusted Git or a Guard manifest. The initial history lookup can log 42P01 on a fresh database before Goose creates the table. The evidence records that bootstrap event separately. [PostgreSQL history schema](https://github.com/pressly/goose/blob/v3.30.1/internal/dialects/postgres.go)

## Transaction and annotation behavior

Goose executes each buffered group as a Simple Query under the PostgreSQL CLI driver. It normally wraps one direction's groups and history update in a transaction. NO TRANSACTION removes that wrapper for the file, even if the annotation appears in Down while running Up. The boundaries between queries still matter because PostgreSQL groups the statements inside a multi-statement Simple Query in an implicit transaction. [Goose SQL execution](https://github.com/pressly/goose/blob/v3.30.1/migration_sql.go) · [PostgreSQL Simple Query](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-MULTI-STATEMENT)

| Fixture | Exit | PostgreSQL state |
| --- | ---: | --- |
| Default wrapper, ALTER and concurrent index on separate lines | 1 | Two Simple Queries. The index returned 25001, the ALTER rolled back, and the candidate version remained absent. |
| NO TRANSACTION, same statements on separate lines | 0 | Two Simple Queries. The ALTER committed, the index was valid, and history contained the candidate. |
| NO TRANSACTION, same line or one StatementBegin/End group | 1 | One Simple Query. The index returned 25001, the ALTER rolled back, and the candidate version remained absent. |
| NO TRANSACTION, separately submitted ALTER before the failing group | 1 | The earlier ALTER committed, the grouped ALTER rolled back, and the candidate version remained absent. |
| NO TRANSACTION in the Down section | 0 | The Up statements ran as separate Simple Queries without a file wrapper. The index was valid. |
| NO TRANSACTION, concurrent index without a semicolon inside StatementBegin/End | 0 | StatementEnd flushed the query and the index was valid. |

In ordinary mode, Goose scans whole lines. It tests the last whitespace-delimited token before the first token starting with `--` for a trailing semicolon. A space before an inline comment let the ALTER terminate on that line. An attached comment kept it buffered with the concurrent index and produced 25001. The probe also verified that the annotation command is case-insensitive, the `+goose` marker is case-sensitive, and leading space invalidates a recognized annotation. Unknown annotations, a missing StatementEnd, and an unfinished ordinary query failed before their SQL ran. ENVSUB ON and OFF changed the stored test string according to the runner environment. Treat environment substitution as incomplete in offline analysis unless the inputs can be established. [Pinned parser](https://github.com/pressly/goose/blob/v3.30.1/internal/sqlparser/parser.go) · [Goose annotation guide](https://pressly.github.io/goose/documentation/annotations/)

These results cover the pinned standalone CLI with its default PostgreSQL driver and version tracking. Custom Go migrations, custom drivers, out-of-order flags, and alternate history stores need separate parity evidence.

## Reproduction

Run as a regular user on Linux with Python 3.11 or newer and a writable `/tmp`. Use the official Goose Linux x86-64 executable with SHA-256 18e32a18f6b49c45c2ea9ce75445c4d34a0a5f110d1a58e05e464d62a03b15e6 and matching PostgreSQL server and client binaries. The recorded runs used Ubuntu 24.04 x86-64 under WSL2 with an isolated network and IPC namespace. The script emits JSON Lines after stopping and removing its cluster. [Goose release](https://github.com/pressly/goose/releases/tag/v3.30.1)

~~~sh
python3 -B fixtures/dmg-09/goose/native_probe.py \
  --goose /path/to/goose --pg-bin /path/to/postgresql-17.12/bin
python3 -B fixtures/dmg-09/goose/native_probe.py \
  --goose /path/to/goose --pg-bin /path/to/postgresql-18.7/bin
~~~
