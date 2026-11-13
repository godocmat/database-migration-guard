
# Read-only PostgreSQL catalog inspection

Call `dmg_inspect::InspectionTarget::inspect_catalog` with a protected TLS target, an opaque `TargetId`, an explicitly authorized `InspectionScope`, `CatalogOptions::new(expected_major)?`, and `InspectionTimeouts`. The result is an `InspectionEvidenceV1` with the observed server version and the client UTC time at the start of the read. A major mismatch records `ServerVersion/Unsupported` and skips catalog collection. Connection setup failures produce unavailable scope and a typed connection gap. [Transport](research/postgresql-transport.md) · [Evidence contract](inspection-evidence.md)

The effective role needs `CONNECT`, effective `USAGE` on the authorized schemas, and access to the selected system catalogs and optional functions. A scope entry can be visible, denied, missing, or unavailable. The snapshot contains the visible schemas, including those with no tables. Check structural gaps before interpreting missing rows as absence. Each query uses a savepoint inside a verified `REPEATABLE READ READ ONLY` transaction. Query errors are reduced to typed gaps. The client deadline preserves completed projections and marks interrupted ones. [Catalog feasibility](research/postgresql-inspection.md)

The structural projection covers ordinary and partitioned tables, visible columns and their qualified types, indexes, supported table constraints, and partition parent edges. Foreign keys supply the constraint-to-relation dependency projection. Outside-scope endpoints are represented without their names. Partition bounds and keys, classical inheritance, and temporal constraint semantics carry gaps when present. Index expression keys have a distinct marker. Column type definitions, independent sequences, views, foreign tables, routines, user triggers, rules, row-level security policies, and the wider dependency graph sit outside the structural projection. Table-owned sequences in the same schema are treated as part of the owning column for scope diagnostics. [PostgreSQL 15 constraints](https://www.postgresql.org/docs/15/catalog-pg-constraint.html) · [PostgreSQL 18 constraints](https://www.postgresql.org/docs/18/catalog-pg-constraint.html) · [Indexes](https://www.postgresql.org/docs/18/catalog-pg-index.html) · [Inheritance](https://www.postgresql.org/docs/18/catalog-pg-inherits.html)

`CatalogOptions::with_row_limit` accepts 1 to 8192 rows per structural projection and defaults to 2048. The structural queries that can exceed that bound request one extra row to detect truncation. The query also limits each index attribute list to 32 entries and each constraint attribute list to 128 entries. Optional definitions and total relation bytes default to off. Enable either with `CatalogOptions::with_details`. Each inspection can deparse up to 48 individual definitions, capped at 8192 accepted UTF-8 bytes each, and read physical sizes for up to 32 ordinary tables. Each optional read rolls back its savepoint to release locks before the next one. Optional definitions can contain sensitive literals and references and belong in protected evidence. A planner row or heap estimate can lag writes. Partition parents have no applicable heap estimate and aggregate rows and physical size stay unavailable. [Catalog information functions](https://www.postgresql.org/docs/18/functions-info.html#FUNCTIONS-INFO-CATALOG-TABLE) · [Size functions](https://www.postgresql.org/docs/18/functions-admin.html#FUNCTIONS-ADMIN-DBSIZE)

## Verification

The [disposable fixture](../fixtures/dmg-10/catalog/collector_probe.py) exercised the Rust API over verified TLS with a SCRAM login that could read metadata and received `42501` on an application row query. It checked visible empty, missing, denied, and quoted scopes, structural fields, masked FK and partition endpoints, estimates, projection and definition caps, major mismatch, and invalid login. It held `ACCESS EXCLUSIVE` in a second connection to verify lock contention, statement cancellation, and the overall deadline. It separately revoked optional function access and raw column catalog access to check partial evidence. PostgreSQL 18 also covered named NOT NULL, virtual generation, a non-enforced CHECK, and the temporal-key coverage gap.

Rust 1.100.0 formatting, tests, and Clippy passed for `dmg-inspect` on Windows x86-64 and Ubuntu 24.04.4 x86-64 under WSL2. The restricted PostgreSQL runs used disposable loopback clusters in isolated network and IPC namespaces. Each case passed on these OpenSSL-enabled fixture builds.

| PostgreSQL | SHA-256 of `postgres` |
| --- | --- |
| 15.20 | `d7a54e24e91880f5d9bf718226c782c64ec3fec1d517a22afa979f76cdd36ee8` |
| 16.16 | `7041232ae64529693b0fb74823d6221eb1a9b373b928eb1d8ba1931b4b2d5c1a` |
| 17.12 | `4c745a817e2bd433b795f3e1d6af15a1f0c99f1b0f75e975e9db2ad2fd70a103` |
| 18.7 | `f96e4f0bf7fdbca133f9ad9a74e8cf4c13fa511aa592d4472a9e4887db90ffc4` |

Run as a regular Unix user with a local OpenSSL-enabled PostgreSQL build, OpenSSL, Python 3.11 or newer, and `localhost` resolving to loopback. Use the test executable path printed by Cargo.

```sh
cargo test --locked -p dmg-inspect --no-run
python3 -B fixtures/dmg-10/catalog/collector_probe.py \
  --pg-bin /path/to/postgresql-18.7-tls/bin --test-binary /path/to/dmg_inspect-test
```
