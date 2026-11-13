# Decisions

## DMG-03 foundation

The workspace pins Rust 1.100.0 and uses edition 2024 with Cargo resolver 3. Keep the parser spike as a separate workspace. The library's initial dependency graph covers serialization, typed errors, and SHA-256 hashing. [Cargo workspace reference](https://doc.rust-lang.org/cargo/reference/workspaces.html)

- A source span uses a repository-relative UTF-8 path with slash separators and zero-based, half-open offsets into the original file. Display lines and Unicode scalar columns start at one. LF ends a line. The two CRLF bytes map to the same end-of-line position. An empty range can mark EOF. The mapping rejects offsets outside the input or inside a UTF-8 scalar.
- A migration identity is the adapter and its native key. Up and Down records share the identity and carry a separate direction. See [DMG-09 adapter contracts](#dmg-09-adapter-contracts) for the key and execution representations. Head inventory follows adapter order. Forward selection follows execution order. A missing selection records unresolved selection, and an empty list records a resolved selection with zero forward inputs.
- Integrity rows describe the original-byte SHA-256 and repository path on either side of an identity and direction. A byte-identical path change at the same identity retains `unchanged` status with both paths available. Git integrity is associated with an explicit base and head. Applied state stays unknown until authorized history is available.
- Execution metadata records the runner's transaction wrapper and statement delivery. SQLx's wrapper opt-out retains single-query delivery. The parser and analyzer use this information to determine the effective transaction context.
- Coverage gaps carry a typed reason and repository, migration, path, or source-span scope. Report construction combines discovery and analysis gaps while preserving existing findings.

## DMG-09 adapter contracts

Plain SQL, SQLx, and Goose keys use signed 64-bit numbers in the report and manifest `version` field. Each adapter still decides which numbers it accepts. Flyway and Supabase use an object with string `kind` and `value` fields for `version`. The tags are `flyway` for canonical dotted Flyway versions, `repeatable` for Flyway descriptions, and `supabase` for exact digit strings. Flyway numeric parts compare by magnitude. Leading zeros in each part and trailing zero parts collapse to the same identity. Repeatables follow versioned migrations and sort by normalized description. Supabase compares digit strings lexically and retains leading zeros. [Flyway 13.12.2 version comparison](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/api/MigrationVersion.java) · [Flyway 13.12.2 repeatable comparison](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resolver/ResolvedMigrationComparator.java) · [Supabase CLI 3.0.0](https://github.com/supabase/cli/releases/tag/v3.0.0)

The integrity baseline records original SHA-256 bytes for each identity and direction. A Flyway repeatable revision still appears as `modified` and is selected for analysis. The `deny_modified` setting protects versioned migrations; `deny_missing` also applies to repeatables. Static order represents inventory and a clean forward run. In an existing Flyway database, a pending versioned migration can refer to objects from a previously applied repeatable. That context requires applied history and catalog evidence.

A file can emit more than one directional record. Goose shares the file path and full-file SHA-256 between Up and Down and carries the relevant original ranges in each record. A Goose integrity manifest may repeat a path for its two directions when both entries have the same identity and digest. A Down row requires the corresponding Forward row at that path. Source byte and file budgets apply once per physical file.

`per_statement` runs each parsed statement individually. `single_query` sends each segment as a simple Query, as used by SQLx and by Goose query groups. A Goose group with multiple PostgreSQL statements can open an implicit block even when the file wrapper is disabled. `extended_batch` identifies a group delivered as an implicit extended-query batch, with a commit boundary at the end of its original byte segment and no explicit savepoint scope. Adapters must account for runner-specific splits before claiming complete coverage. [Goose v3.30.1 SQL execution](https://github.com/pressly/goose/blob/v3.30.1/migration_sql.go) · [PostgreSQL extended protocol](https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-PIPELINING)

Adapter modules in `crates/dmg-discovery/src` implement filename recognition and source layout independently. Wire those methods into `Adapter::kind`, `Adapter::candidate`, and `Adapter::source_layouts`. The filename hook returns an error for recognizable unsupported candidates, which discovery turns into a path-scoped coverage gap. The layout hook receives the original bytes and can return zero or more `SourceLayout` records and an optional structured gap. A zero-record result for a recognized migration needs a gap. Common discovery owns raw reads, digests, duplicate detection, and comparison. Register the accepted adapter and trusted settings in the CLI after its fixtures pass.

## Policy and report v1

Policy TOML requires `schema_version = 1`. The v1 shape rejects unknown settings. Rule IDs use the form `PG001` through `PG999`, and rule registry membership is checked by the evaluator when the ruleset is available. The initial policy shape accepts a severity threshold, optional Git evidence requirement, integrity outcomes, and per-rule severity.

```toml
schema_version = 1
deny_at = "high"

[integrity]
require_git_base = true
deny_modified = true
deny_missing = true

[rules.PG005]
severity = "medium"
```

Report JSON requires `schema_version = 1` and accepts additive fields. Its top-level fields identify the tool, ruleset, PostgreSQL target, migration plan, coverage, severity counts, findings, and optional policy decision. Creation sorts findings by original source span with deterministic tie-breakers and derives severity counts from the same findings. Adapter order remains part of the migration plan. Consumers should interpret an absent applied-history record as unknown and a null policy decision as a report without a decision. Future major versions need explicit migration guidance and compatibility handling.
