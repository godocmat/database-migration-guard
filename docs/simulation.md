# Migration rehearsal

`dmg-simulate` replays the supported SQL in a reviewed Git revision on a fresh, [disposable PostgreSQL worker](research/simulation-worker.md). The protected caller supplies the repository identity, exact Git discovery, reviewed configuration and policy digests, approved PostgreSQL image, scoped schemas and ordered search path. `prepare_rehearsal` checks plan coverage before any Docker call. `run_rehearsal` owns one worker through verified removal.

## Baseline and seeds

| Mode | Setup | Migration selection |
| --- | --- | --- |
| Empty database | Provision the declared schemas | Replay the complete forward inventory in adapter order |
| Schema-only | Load a trusted schema SQL script that represents the exact Git base | Apply new forwards after the existing base versions, followed by new Flyway repeatables |
| Seeded | Load the trusted base schema, then synthetic or sanitized INSERTs | Apply the same pending forwards as schema-only mode |

The caller is responsible for supplying a baseline that represents the base and for sanitizing any seed data. The evidence records each setup file's original byte count and SHA-256 digest. Seeded mode also records the total rows reported by successful INSERT command tags. Baseline and seed scripts are UTF-8, bounded to 256 KiB each, and run atomically. Baseline supports the normalized schema subset. Seed supports normalized INSERTs with VALUES or DEFAULT VALUES. An existing PostgreSQL dump may contain directives, ownership changes, schema creation, data or extensions outside this profile.

The worker creates the declared schemas before connecting as the migration role. Authored CREATE SCHEMA requires database-level privileges, including when the schema already exists and the statement has IF NOT EXISTS. Other coverage gaps include unsupported PostgreSQL operations, unsupported runner hooks and substitution, qualified relations outside scope, and ambiguous pending order. With a Git-base baseline the planner requires unchanged base forwards, rejects a new version ordered before an existing version, and checks consistency with any supplied applied-history reconciliation. Down files are outside forward rehearsal. [PostgreSQL CREATE SCHEMA](https://www.postgresql.org/docs/18/sql-createschema.html)

## Execution

The planner retains original byte digests and source spans and sends the exact source ranges over the authenticated worker connection. The supported runner profiles define the transaction boundary.

| Adapter | Delivery |
| --- | --- |
| Plain SQL | One Simple Query per statement. The configured file mode adds one transaction around the file. Autocommit mode supports authored transactions and savepoints. |
| SQLx 0.8.6 | One Simple Query for the forward file, with a file transaction unless the native opt-out applies. |
| Goose 3.30.1 | One Simple Query per discovered Up group, in one transaction unless NO TRANSACTION applies. |
| Flyway 13.12.2 | One Simple Query per PostgreSQL statement with the resolved transaction mode for the supported Flyway settings. |
| Supabase CLI 3.0.0 | Parse, Bind and Execute each statement with one Sync per batch. Recognized concurrent index commands run separately. A file with native transaction control or a supported transaction opt-out runs sequentially. |

The replay covers user SQL delivery and PostgreSQL transaction effects. It uses a clean migration-role session for each file and omits native migration-ledger writes, runner lock coordination, external hooks, custom session settings and extension-specific prerequisites. These dependencies need explicit coverage for a deployment claim. The worker supports PostgreSQL 15.20, 16.16, 17.12 and 18.7 on Linux x86-64. [Runner format profiles](migration-formats.md) · [PostgreSQL protocol flow](https://www.postgresql.org/docs/18/protocol-flow.html) · [Supabase CLI 3.0.0 executor](https://github.com/supabase/cli/blob/v3.0.0/apps/cli/src/command-internal/migration-apply.ts)

## Evidence

A report binds the exact base and candidate commits, repository and target IDs, project and policy digests, forward inventory and execution plan digests, runner profile version, setup inputs, schema scope and search order, approved image digests, and observed server version. An independent consumer can recompute the ordered group metadata and plan digest with `expected_rehearsal_plan`, using trusted Git discovery and the trusted rehearsal mode.

Statement timing is the monotonic time from group dispatch to receipt of its completion response. Group timing includes the PostgreSQL ReadyForQuery acknowledgement. Migration timing covers connection setup, the migration groups and any transaction finalization. Durability applies to transactional effects. Prior acknowledged commits remain visible in the report if a later group fails. A failed concurrent index operation can leave partial catalog effects, and an interrupted transport can leave durability unknown.

Read-only catalog observations surround migration execution after the baseline and seed have loaded. The summaries include structural digests and constraint validation and enforcement states. The diff includes structural changes and its own coverage. A report can have successful execution with incomplete catalog coverage. Failed and timed-out runs preserve available observations, including known partial commits. The report records typed failure phases and SQLSTATEs without SQL, server messages, result rows or connection secrets.

## Verification

The ignored worker integration tests use synthetic Git revisions and run against an explicitly selected isolated Docker Engine with one approved image. Supply `DMG_WORKER_TEST_DOCKER`, `DMG_WORKER_TEST_SOCKET` and `DMG_WORKER_TEST_POSTGRES` as described in the worker documentation.

```sh
cargo test --locked --offline -p dmg-simulate isolated_rehearsal -- --ignored --test-threads=1
```

The ignored `isolated_postgres_protocol` test separately checks SQL delivery, server errors and transactional effects using a fresh loopback PostgreSQL fixture. The fixture database and SCRAM login are both named `dmg_rehearsal_fixture`. Supply its port, ephemeral password and approved server release in `DMG_PROTOCOL_TEST_PORT`, `DMG_PROTOCOL_TEST_PASSWORD` and `DMG_PROTOCOL_TEST_POSTGRES`. Run it only against the disposable fixture. Docker lifecycle and catalog capture are exercised by the worker integration tests.
