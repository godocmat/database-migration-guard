# Migration rehearsal

`dmg-simulate` replays the supported SQL in a reviewed Git revision on a fresh, [disposable PostgreSQL worker](research/simulation-worker.md). The protected caller supplies the repository identity, exact Git discovery, reviewed configuration and policy digests, approved PostgreSQL image, scoped schemas and ordered search path. `prepare_rehearsal` checks plan coverage before any Docker call. `run_rehearsal` owns one worker through verified removal.

## Run from the CLI

Run rehearsal in a protected job for one reviewed Git revision. Supply the full local base and candidate commit IDs, trusted project and policy files, and a protected rehearsal configuration. The project supplies the declared PostgreSQL major and exact search path order. The rehearsal config supplies the approved image release and schemas owned by the migration role in the disposable worker. The scope must include every schema in the project search path. Repository and target IDs must match the policy's [evidence scope](policy-evidence.md) when configured.

```toml
schema_version = 1
repository_id = "example-repo"
target_id = "production"
schemas = ["public"]
postgres = "18.7"

[setup]
kind = "schema_only"
baseline_file = "baseline.sql"

[worker]
docker_executable = "/usr/bin/docker"
docker_socket = "/run/dmg/docker.sock"

[worker.timeouts]
startup_ms = 30000
operation_ms = 60000
statement_ms = 10000
lock_ms = 1000
cleanup_ms = 10000
```

Use `postgres = "15.20"`, `"16.16"`, `"17.12"`, or `"18.7"` for the matching major. The selected release fixes both image digests. Preload that exact image into the [isolated Docker Engine](research/simulation-worker.md#images-and-resources). The worker uses explicit absolute Linux paths for the Docker executable and local Unix socket. It checks the executable and socket during provisioning. The displayed timeout values are defaults. The maximums in the same order are 120,000, 300,000, 60,000, 60,000, and 30,000 milliseconds. The minimum is one millisecond.

The rehearsal TOML can be up to 64 KiB. Select `kind = "empty_database"` with no setup files, `kind = "schema_only"` with `baseline_file`, or `kind = "seeded"` with `baseline_file`, `seed_file` and `seed_kind` set to `"synthetic"` or `"sanitized"`. Relative setup paths resolve from the rehearsal TOML's directory. Keep the rehearsal config and setup files under the protected caller's control.

First generate the independently expected binding on a trusted machine. Planning works offline on the CLI's supported platforms and can omit the worker table. Use the output as the `expected_binding` object in the protected policy evidence bundle.

```sh
dmg simulate --repo . --base "$BASE_SHA" --head "$HEAD_SHA" \
  --config /protected/dmg.toml --policy /protected/policy.toml \
  --rehearsal-config /protected/rehearsal.toml --plan-only \
  --output /protected/expected-binding.json
```

Run the same reviewed inputs on the disposable Linux x86-64 worker, with the protected worker table populated. Save the full report.

```sh
dmg simulate --repo . --base "$BASE_SHA" --head "$HEAD_SHA" \
  --config /protected/dmg.toml --policy /protected/policy.toml \
  --rehearsal-config /protected/rehearsal.toml \
  --output /protected/rehearsal.json
```

On a protected publisher, use the independently generated binding and the SHA-256 of the exact trusted rehearsal report bytes for the bundle's `rehearsal.expected_binding` and `rehearsal.report_sha256`. Hash the saved report without reformatting it. Author and pin the bundle and supply it with the report to `dmg check` using the same revisions, project and policy. [Policy evidence JSON and trusted check](policy-evidence.md#rehearsal)

Without `--output`, JSON goes to stdout. The file output replaces the destination atomically. Exit 0 means the binding was planned or the rehearsal finished successfully. Exit 2 means invalid invocation or configuration and preserves the previous output. Exit 3 means incomplete planning coverage, failed or interrupted execution, or an operational failure. Preflight coverage failures emit version 1 diagnostics with `outcome = "incomplete"` and a `coverage` object. A prepared worker run emits a version 1 rehearsal report with its outcome, including `unsupported_platform` on platforms other than Linux x86-64. Successful SQL rehearsal may include partial catalog observations. Use `require_complete_schema` in the protected policy if the check requires complete schema observations and comparison.

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

Unconfirmed worker cleanup makes the rehearsal incomplete. Reports can include a `cleanup_worker` name for [operator reconciliation on the same Docker daemon](research/simulation-worker.md#postgresql-connection-and-teardown).

## Verification

The ignored worker integration tests use synthetic Git revisions and run against an explicitly selected isolated Docker Engine with one approved image. Supply `DMG_WORKER_TEST_DOCKER`, `DMG_WORKER_TEST_SOCKET` and `DMG_WORKER_TEST_POSTGRES` as described in the worker documentation.

```sh
cargo test --locked --offline -p dmg-simulate isolated_rehearsal -- --ignored --test-threads=1
```

The ignored `isolated_postgres_protocol` test separately checks SQL delivery, server errors and transactional effects using a fresh loopback PostgreSQL fixture. The fixture database and SCRAM login are both named `dmg_rehearsal_fixture`. Supply its port, ephemeral password and approved server release in `DMG_PROTOCOL_TEST_PORT`, `DMG_PROTOCOL_TEST_PASSWORD` and `DMG_PROTOCOL_TEST_POSTGRES`. Run it only against the disposable fixture. Docker lifecycle and catalog capture are exercised by the worker integration tests.
