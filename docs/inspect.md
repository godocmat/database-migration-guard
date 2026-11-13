# Inspect PostgreSQL

Run `dmg inspect` from a protected job or workstation with explicit authorization for one PostgreSQL target. Catalog inspection requires database `CONNECT`, effective `USAGE` on the authorized schemas, and access to the fixed catalog projections. Give the inspection role only the grants it needs. [Catalog permissions and projections](postgresql-catalog.md)

## Connection configuration

Place the connection TOML, CA bundle, and password file in a directory controlled by the operator or protected job. Use restrictive file permissions or Windows ACLs to limit access to the account that runs inspection. Keep the connection TOML and password out of repository inputs and logs.

```toml
schema_version = 1
target_id = "orders-staging"
target = 18
schemas = ["orders"]
host = "postgres.example.internal"
port = 5432
database = "orders"
user = "guard_inspect"
ca_file = "ca.pem"
password_file = "password.bin"

# Optional catalog settings
definitions = false
total_bytes = false
row_limit = 2048

[timeouts]
overall_ms = 5000
statement_ms = 1000
lock_ms = 100
idle_transaction_ms = 3000

# Optional source used by --history
[history]
schema = "public"
table = "_sqlx_migrations"
row_limit = 2048
```

Set an opaque `target_id` of 1 to 128 ASCII letters, digits, dots, underscores, or hyphens, starting with a letter or digit. Set `target` to PostgreSQL major 15, 16, 17, or 18. Supply one to 32 distinct non-system schema names with their exact catalog spelling. The connection file accepts only the documented keys. It supplies the endpoint and authentication fields explicitly. Relative CA and password paths resolve from the connection file's directory. The PEM bundle supplies the trust anchors for required TLS and host verification. The password file supplies 1 to 4096 exact bytes. Save just the password bytes, since line endings, whitespace, and a byte-order mark become part of the password. [Transport and deadlines](research/postgresql-transport.md)

All timeouts accept 1 to 30,000 milliseconds and default to the values shown. The catalog row limit applies to each structural projection. The history row limit applies to the designated history source. Both accept 1 to 8,192 and default to 2,048. Enable `definitions` for deparsed expressions and `total_bytes` for observed physical relation sizes. These reads have separate bounds and can be incomplete. [Catalog detail bounds](postgresql-catalog.md)

## Catalog evidence

```sh
dmg inspect --connection /protected/inspection.toml --output /protected/catalog.json
```

Catalog mode writes version 1 inspection JSON with the observation time, server version when available, requested schema states, visible structural facts, and typed gaps. Confirmed missing schemas count as observed absence. Denied or unavailable scope, interrupted or unsupported structural projections, and unavailable requested definitions or sizes produce exit 3 with the available evidence. Missing advisory planner row or heap estimates leave the structural exit status unchanged.

`--output` replaces the destination atomically. Omit it to write JSON to stdout. Keep the JSON in protected storage because catalog identifiers and optional definitions can disclose sensitive information. Use it as a saved input to `dmg diff` when a structural comparison is needed. [Inspection evidence format](inspection-evidence.md) · [Diff command](diff.md#command-line-workflow)

## Native migration history

Use `--history` and an explicit project `--config` to write standalone version 1 native history JSON. The project selects the runner profile, migration roots, and PostgreSQL major. The connection's `[history]` entry separately authorizes its exact schema and table. The history role needs effective `USAGE` and narrow `SELECT` on the reader's projected columns. Use a table protected from untrusted writers and DDL. [Supported history sources and grants](postgresql-history.md)

```sh
dmg inspect --history --connection /protected/inspection.toml \
  --config /protected/dmg.toml --repo . --base "$BASE_SHA" --head "$HEAD_SHA" \
  --output /protected/history.json
```

Supply both Git revisions to bind history to the discovered candidate commit's forward inventory. Omit both to bind to the working directory inventory. History mode requires a complete candidate inventory before connecting. Plain SQL uses a trusted applied manifest. The reader records unavailable, truncated, or otherwise incomplete observations and exits 3. A fully observed empty history can establish pending migrations for a matching inventory. A recorded Goose event with an unknown state can still leave reconciliation incomplete. Catalog and history commands collect separate observations.

Use the exact saved file's SHA-256 from a trusted source. Supply the history file, pin, and expected opaque target ID together to `scan` or `check`. Choose one source of applied history per invocation.

```sh
dmg check --config /protected/dmg.toml --policy /protected/policy.toml \
  --repo . --base "$BASE_SHA" --head "$HEAD_SHA" \
  --native-history /protected/history.json --native-history-sha256 "$HISTORY_SHA256" \
  --target-id orders-staging --format json
```

Import runs offline. It verifies the exact saved bytes against the pin, then verifies the target ID, server major when available, runner profile, and forward inventory against the current analysis. Binding or coverage failures produce an incomplete report. The JSON report records reconciliation in `migrations.applied_history`, including the canonical history digest, inventory digest, evidence method, and migration statuses. Protect the pin and target ID through the workflow that produced the artifact. The pin authenticates the expected bytes only to the extent that its source is trusted. The recorded timestamp describes collection time. [Native history evidence and inventory binding](inspection-evidence.md#native-migration-history)

For `inspect`, exit 0 means complete requested observation, 2 means invalid invocation or configuration, and 3 means incomplete observation or an operational failure. `scan` and `check` use the project's normal exit code settings.
