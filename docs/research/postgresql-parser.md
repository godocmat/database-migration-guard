# DMG-01 PostgreSQL parser decision brief

**Desk research · 12 November 2026**

Advance `pg_query` 18.1.0 as the first candidate for ADR 001. Its PostgreSQL-derived tree exposes the migration facts Guard needs, including ordered `ALTER TABLE` actions, deferred-validation flags, and concurrent index flags. Keep `pg_query` 6.2.1 as a PostgreSQL 17 reference and `sqlparser` 0.64.0 as the alternate. The decision remains provisional until the source mapping, server conformance, licensing, and native build checks below pass. [Implementation plan](../../PLAN.md) · [Native parse-tree schema][pg18-tree]

## Candidate pins and grammar

| Cargo pin | Grammar and role | Release and source evidence |
| --- | --- | --- |
| `pg_query = "=18.1.0"` | Bundles `libpg_query` 18.1.0, derived from PostgreSQL **18.6** (`180006`). Use it first for the declared 17 and 18 targets, with Guard's target-specific checks. | [27 October crate release][pg18-crate] · [Submodule revision][pg18-submodule] · [Bundled version header][pg18-header] |
| `pg_query = "=6.2.1"` | Bundles `libpg_query` `17-6.2.5`, derived from PostgreSQL **17.7** (`170007`). Use it as the 17 grammar reference in the spike. | [30 September crate release][pg17-crate] · [Submodule revision][pg17-submodule] · [Bundled version header][pg17-header] |
| `sqlparser = "=0.64.0"` | `PostgreSqlDialect` in the Apache DataFusion crate selects PostgreSQL behavior within a shared Rust parser. The dialect has no server-major setting or PostgreSQL grammar revision. | [9 November crate release][sql-crate] · [Dialect implementation][sql-dialect] · [Upstream parser scope][sql-readme] |

These are the latest non-yanked crate releases for the two native grammar lines and for `sqlparser` as of the research date. The downloaded package checksums matched crates.io. The two `pg_query` tags point to the native releases above, and their packaged headers, Makefiles, and Protobuf schemas matched the tagged sources. [pg_query registry index][pg-index] · [sqlparser registry index][sql-index]

PostgreSQL's current server minors are **17.12** and **18.7**. Use those as the initial server pins and record the parser's own grammar version separately. [PostgreSQL release notice][server-releases]

## AST, source locations, and diagnostics

**`pg_query`.** The native call runs PostgreSQL's raw parser and the Rust API decodes the Protobuf result. `RawStmt` wraps each top-level statement. `AlterTableStmt.cmds` preserves the command list, `AlterTableCmd` carries subtype and options, `Constraint` carries `skip_validation` and `initially_valid`, and `IndexStmt` carries `concurrent`, `unique`, and the predicate. `RangeVar` provides the parsed names and a start location. Recover original spelling and quoting from the input. Catalog resolution, execution restrictions, and lock or rewrite behavior require Guard analysis or server evidence. [Native parse call][pg18-parse] · [Rust parse wrapper][pg18-rust-parse] · [Native parse-tree schema][pg18-tree]

`RawStmt.stmt_location` and `stmt_len` describe a statement in bytes. A length of zero means the rest of the input, and a start of `-1` means unknown. The 18 native release moves the start of a later statement past intervening whitespace and comments. Node `location` fields give starts where present, and `AlterTableCmd` has no location field of its own. The scanner exposes half-open byte ranges. Derive statement and subcommand anchors from the original bytes and tokens, and check the boundaries on both bundles. The scanner-based splitter can skip invalid input, so account for skipped regions before claiming coverage. [PostgreSQL node definitions][pg18-nodes] · [Native 18 release notes][pg18-release] · [Scanner API][pg18-scanner] · [Rust scanner splitter][pg18-split]

The C API carries `PgQueryError.cursorpos`. For syntax errors the native scanner converts a byte offset to a one-based character position. Both pinned Rust wrappers retain only the native message in `Error::Parse(String)` and `Error::Scan(String)`. Embedded NUL fails at `CString::new`, and Protobuf decoding has its own error path. The 18.1.0 release adds native stack-depth checks and reports excessive nesting as an error. Measure that behavior on the target platforms. The spike must establish whether precise error locations warrant exposing the native cursor or whether tested statement ranges suffice. Keep the reported location precision explicit. [Native error struct][pg18-error] · [Scanner cursor conversion][pg18-cursor] · [18 Rust wrapper][pg18-rust-query] · [17 Rust wrapper][pg17-rust-query] · [Native 18.1.0 release][pg181-release]

**`sqlparser`.** The shared `Statement` and `AlterTableOperation` types carry useful facts such as ordered operations, quoted identifiers, and index options. Coverage is clause-specific. In 0.64.0 the table-constraint dispatch has no named table-level `NOT NULL` branch, so the PostgreSQL 18 form below is expected to fail. The `ALTER TABLE ... ADD` path consumes `NOT VALID` after any recognized table constraint, so `UNIQUE ... NOT VALID` is a candidate false acceptance. Run both fixtures to check those source predictions. [DDL types][sql-ddl] · [Identifier type][sql-ident] · [Table-constraint dispatch][sql-constraints] · [`ADD CONSTRAINT` parser][sql-add]

`Spanned` combines child locations. For example, an `ALTER TABLE` span starts from its relation or a child, and the `DROP COLUMN` operation span covers its column names. Use tokens to recover complete statement and action boundaries. Token locations start at line 1, column 1 and advance by Rust `char`. Although the `Span` field comment describes an inclusive end, the tokenizer records the position after consuming the token, as its `SELECT` test demonstrates. Verify conversions on CRLF and multibyte input. Direct tokenization exposes a structured `TokenizerError.location`. `Parser::parse_sql` returns string-backed token and syntax errors and a separate recursion-limit variant. Many syntax messages include a formatted location. [AST span implementation][sql-spans] · [Tokenizer locations and tokenization][sql-tokenizer] · [Tokenizer span test][sql-span-test] · [Parser errors][sql-errors]

## Build and redistribution evidence

Both `pg_query` releases use Cargo's `cc` build dependency to compile the vendored C and link `pg_query` statically, then run bindgen against `pg_query.h`. Bindgen needs a loadable host `libclang`. The packages ship generated C and Rust Protobuf sources. `protoc` is optional for normal builds, and the script attempts Rust Protobuf regeneration when `protoc` is found or `REGENERATE_PROTOBUF` is set, except under docs.rs. Use the shipped bindings in the initial build, with `protoc` absent from `PATH` and regeneration unset. The 18 build compiles upb and `utf8_range`, and the 17 build compiles protobuf-c. [18 build script][pg18-build] · [17 build script][pg17-build] · [Bindgen requirements][bindgen]

| Proposed native target | Build environment to verify |
| --- | --- |
| Linux x86-64 (`x86_64-unknown-linux-gnu`) | Stable Rust and Cargo, a C compiler such as GCC or Clang, system C headers and linker tools, and `libclang` such as Debian's `libclang-dev`. Record the distro and glibc baseline. [Bindgen on Linux][bindgen] |
| Windows x86-64 (`x86_64-pc-windows-msvc`) | Stable Rust and Cargo, MSVC C/C++ Build Tools with a Windows SDK, and x64 LLVM with a loadable `libclang.dll`. Record `LIBCLANG_PATH` if needed. The build script selects PostgreSQL's Windows and MSVC include directories. [Rust MSVC target][rust-msvc] · [Bindgen on Windows][bindgen] · [Build script][pg18-build] |
| macOS arm64 (`aarch64-apple-darwin`) | Stable Rust and Cargo, Apple Clang and the macOS SDK and linker from Xcode Command Line Tools or Xcode, and an arm64 `libclang` that bindgen can load. Test discovery with the chosen installation, including a separate LLVM install if needed. Record the deployment target. [Rust Apple targets][rust-apple] · [Bindgen on macOS][bindgen] |

The tagged Rust binding workflow configures Linux and Windows tests. The native library has a separate macOS `make` job. These workflows describe upstream build coverage. The spike must establish the Cargo and packaged-binary result on all three Guard targets, including the macOS Rust binding. [Rust binding workflow][pg18-ci] · [Native workflow][native-ci]

`sqlparser` has no crate-level native build script or bindgen requirement. Its default `recursive-protection` feature brings in `recursive`, `stacker`, and `psm`. The upstream lockfile resolves a graph that builds C on Windows and assembles target-specific code through `psm` on all three targets, including MASM on native Windows MSVC. A build with `default-features = false` and `features = ["std"]` removes that feature path while retaining the parser's depth counter. Check the resolved graph and measure bounded-depth behavior for the feature set considered for Guard. [Cargo features][sql-cargo] · [Upstream lockfile][sql-lock] · [Stacker build][stacker-build] · [PSM build][psm-build] · [Parser recursion limit][sql-depth]

- **`pg_query` bundle.** The binding is MIT, `libpg_query` is BSD-3-Clause, and the extracted server code uses the PostgreSQL License. The compiled vendor sources include xxHash (BSD-2-Clause), upb (BSD-3-Clause) and `utf8_range` (MIT) on 18, or protobuf-c (BSD-2-Clause) on 17. The published Rust archives omit standalone license files, so collect the matching upstream texts and embedded notices for distribution. [Binding license][pg-license] · [Native license][native-license] · [PostgreSQL license][postgres-license] · [xxHash header][xxhash-license] · [upb license][upb-license] · [`utf8_range` license][utf8-license] · [protobuf-c header][protobuf-c-license]
- **`sqlparser` bundle.** The crate is Apache-2.0 and ships the license and `NOTICE.TXT`. For either candidate, review the resolved Cargo dependencies and compiled vendored sources when assembling the binary and container notices and SBOM. [Apache license][sql-license] · [Apache notice][sql-notice] · [Package manifest][sql-cargo]

## Conformance matrix for the spike

The outcomes below are expectations from documentation and source. Run each SQL case on disposable UTF-8 PostgreSQL 17.12 and 18.7 instances with `standard_conforming_strings` on. Record `server_version_num`, statement results, and SQLSTATE and position when supplied. Send SQL fixtures over the simple query protocol, with fresh objects per case. Use `t(id integer, v integer, fk integer)`, `p(id integer PRIMARY KEY)`, `part(fk integer)` partitioned by range with one leaf, and `"App"."café"(id integer, v integer, "Old" text)` where needed. Run the concurrent index in autocommit. [Simple query protocol][server-query] · [CREATE INDEX][create-index] · [Server error fields][server-errors]

| Fixture | Expected on PostgreSQL 17 | Expected on PostgreSQL 18 | Offline result to check |
| --- | --- | --- | --- |
| Quoted multi-action `ALTER` in the sample below | Accept | Accept | Preserve schema and relation case, both ordered actions, and the table and column existence flags. Anchor the destructive `DROP COLUMN` at its own action. |
| `CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS cafe_ix ON "App"."café" (id) WHERE v IS NOT NULL;` | Accept | Accept | Preserve index name, target, key, predicate, uniqueness, concurrency, and existence flag. |
| `BEGIN; UPDATE t SET v = 1; UPDATE t SET v = 2 WHERE id = 1; COMMIT;` | Accept | Accept | Preserve four ordered statements, the transaction bounds, and absent versus present predicates. |
| `ALTER TABLE t ADD CONSTRAINT t_fk FOREIGN KEY (fk) REFERENCES p(id) NOT VALID;` then `ALTER TABLE t VALIDATE CONSTRAINT t_fk;` | Accept both | Accept both | Preserve the foreign key columns and targets, the deferred-validation flag, and the separate validate action. Observe `convalidated` change from false to true. [17 ALTER TABLE][alter17] · [18 ALTER TABLE][alter18] |
| `ALTER TABLE t ADD CONSTRAINT t_v_nn NOT NULL v NOT VALID;` | Syntax rejection | Accept | Expect the 17 grammar to reject and the 18 grammar to retain the column, name, and validation flag. With the 18 parser, emit target incompatibility for declared 17. On 18 observe a `pg_constraint` entry of type `n` with `convalidated = false`. [17 grammar][grammar17] · [18 grammar][grammar18] · [18 constraint catalog][catalog18] |
| `ALTER TABLE part ADD CONSTRAINT part_fk FOREIGN KEY (fk) REFERENCES p(id) NOT VALID;` | Reject during DDL execution, expected SQLSTATE `42809` | Accept | Expect both native grammars to parse. Guard needs the referencing relation's partitioned kind to diagnose incompatibility on 17. A missing relation kind yields unresolved compatibility. Repeat with an ordinary table as the control. [17 execution check][partition17] · [18 release notes][pg18-notes] |
| `ALTER TABLE t ADD CONSTRAINT t_u UNIQUE (id) NOT VALID;` | Reject | Reject | Check for false acceptance in `sqlparser`. Reject the construct for both targets even if a candidate emits an AST. [17 ALTER TABLE][alter17] · [18 ALTER TABLE][alter18] · [18 grammar attribute check][constraint-attr18] |
| `DO $body$ BEGIN EXECUTE 'SELECT 1;'; END; $body$;` followed by `SELECT 2;` | Accept | Accept | Keep the `DO` block intact as one statement and report unsupported procedural analysis. The following `SELECT` has its own range. [DO][do17] |
| Valid statement, malformed middle statement, valid statement, for example `SELECT 'é';` then `ALTER TABLE t ADD COLUMN broken ;` then `DROP TABLE t;` | Reject the batch at the middle statement | Reject the batch at the middle statement | Report incomplete analysis with the middle statement's span at minimum when tokenization succeeds. Record recovery of the trailing `DROP` or mark its region unexamined. Also check an unterminated dollar quote and an unexpected EOF. [Simple query error handling][server-query] |
| `\set target 1`, an unresolved `{{schema}}` placeholder, and input containing NUL or invalid UTF-8 | Adapter/input cases | Adapter/input cases | Report client directive, unresolved templating, or invalid input with an original-file range. [psql meta-commands][psql17] |

Use CRLF between these four lines and omit a newline at EOF. The sample exercises nested comments, a quoted identifier, an embedded semicolon, and a final statement without a terminator.

```sql
/* π; /* nested; */ lead */
ALTER TABLE IF EXISTS "App"."café" ADD COLUMN IF NOT EXISTS "Notes" text, DROP COLUMN IF EXISTS "Old" RESTRICT;
-- tail;
SELECT 'é;'
```

Expect two statement spans, `[30, 141)` and `[154, 166)`, from first through last SQL token, excluding the delimiter. The `"café"` token covers `[58, 65)`, and `DROP COLUMN` starts at byte 105, line 2, column 75, counting Unicode scalar values for columns. Repeat from a forward segment starting at byte 30 and require identical original-file coordinates for both statements and actions. Use the original bytes for slices and the original line starts for rendering.

## Evidence required to accept or revise ADR 001

1. **Grammar and facts.** Run the same fixture bytes through all candidates, and run the SQL rows against both servers. Record the declared target and parser pin for every offline result. Check `pg_query`'s reported Protobuf version for `180006` and `170007` using separate executables because both pins link a C library with the same name and entry points. A candidate passes when it emits the expected normalized facts for every supported fixture and the expected version diagnostic for both PostgreSQL 18 constraint cases when the target is 17. Document any candidate/server disagreement by fixture and stage. [Protobuf version field][pg18-version] · [18 build script][pg18-build] · [17 build script][pg17-build]
2. **Source ranges and coverage.** Assert original-byte slices and the specified line and column anchors, including CRLF, multibyte text, and a nonzero segment offset. Require the same normalized facts, ranges, and diagnostics across repeated runs and native targets. A syntax error, unsupported body or directive, or unresolved version condition must produce explicit incomplete coverage. For lexical failures with unprovable statement boundaries, identify the affected segment. For precise errors, verify the reported token or EOF anchor against the original input. Test malformed input, a nested expression beyond the probe's depth limit, and a file one byte over its size limit. Each must produce a diagnostic without a panic or crash, followed by successful parsing of a valid input in the same probe.
3. **Clean native build and offline execution.** Build a probe using the preferred published crate and `cargo build --locked --release` on each target. Record `rustc -Vv`, the resolved features, exact build commands, platform baseline, compiler and SDK versions, any `libclang` version, and environment settings. Inspect the executable's runtime dependencies and run the same smoke fixtures on a clean, disconnected host with only the declared runtime prerequisites. Pass when each advertised target builds and runs, the smoke results match, and the linked components and notices can be accounted for.

The spike must decide whether one 18-derived grammar plus explicit compatibility checks covers the 17 subset, whether actionable error anchors require exposing the native cursor, and which pinned toolchains and runtime baselines pass. If the preferred pin misses a gate, record the failed fixture or target and revisit the parser, adapter, or advertised support in ADR 001 before the foundation work. Apply the same gates to `sqlparser` if it becomes the candidate.

[pg18-crate]: https://crates.io/api/v1/crates/pg_query/18.1.0
[pg17-crate]: https://crates.io/api/v1/crates/pg_query/6.2.1
[sql-crate]: https://crates.io/api/v1/crates/sqlparser/0.64.0
[pg-index]: https://index.crates.io/pg/_q/pg_query
[sql-index]: https://index.crates.io/sq/lp/sqlparser
[pg18-submodule]: https://api.github.com/repos/pganalyze/pg_query.rs/contents/libpg_query?ref=v18.1.0
[pg17-submodule]: https://api.github.com/repos/pganalyze/pg_query.rs/contents/libpg_query?ref=v6.2.1
[pg18-header]: https://github.com/pganalyze/libpg_query/blob/18.1.0/pg_query.h#L213-L215
[pg17-header]: https://github.com/pganalyze/libpg_query/blob/17-6.2.5/pg_query.h#L165-L167
[sql-dialect]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/dialect/postgresql.rs#L202-L230
[sql-readme]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/README.md#L68-L78
[server-releases]: https://www.postgresql.org/about/news/postgresql-187-1712-1616-1520-1425-and-19-rc-2-released-3442/
[pg18-parse]: https://github.com/pganalyze/libpg_query/blob/18.1.0/src/pg_query_parse.c#L66-L104
[pg18-rust-parse]: https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/src/query.rs#L36-L48
[pg18-rust-query]: https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/src/query.rs
[pg17-rust-query]: https://github.com/pganalyze/pg_query.rs/blob/v6.2.1/src/query.rs
[pg18-tree]: https://github.com/pganalyze/libpg_query/blob/18.1.0/protobuf/pg_query.proto
[pg18-nodes]: https://github.com/pganalyze/libpg_query/blob/18.1.0/src/postgres/include/nodes/parsenodes.h
[pg18-release]: https://github.com/pganalyze/libpg_query/releases/tag/18.0.0
[pg181-release]: https://github.com/pganalyze/libpg_query/releases/tag/18.1.0
[pg18-scanner]: https://github.com/pganalyze/libpg_query/blob/18.1.0/pg_query.h#L37-L44
[pg18-split]: https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/src/query.rs#L421-L438
[pg18-error]: https://github.com/pganalyze/libpg_query/blob/18.1.0/pg_query.h#L12-L18
[pg18-cursor]: https://github.com/pganalyze/libpg_query/blob/18.1.0/src/postgres/src_backend_parser_scan.c#L11146-L11158
[sql-ddl]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/ast/ddl.rs
[sql-ident]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/ast/mod.rs#L264-L277
[sql-constraints]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/parser/mod.rs#L10699-L10935
[sql-add]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/parser/mod.rs#L11344-L11356
[sql-spans]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/ast/spans.rs
[sql-tokenizer]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/tokenizer.rs#L550-L1070
[sql-span-test]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/tokenizer.rs#L4064-L4092
[sql-errors]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/parser/mod.rs#L55-L69
[pg18-build]: https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/build.rs
[pg17-build]: https://github.com/pganalyze/pg_query.rs/blob/v6.2.1/build.rs
[bindgen]: https://rust-lang.github.io/rust-bindgen/requirements.html
[rust-msvc]: https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html
[rust-apple]: https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html
[pg18-ci]: https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/.github/workflows/main.yml
[native-ci]: https://github.com/pganalyze/libpg_query/blob/18.1.0/.github/workflows/ci.yml
[sql-cargo]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/Cargo.toml
[sql-lock]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/Cargo.lock
[stacker-build]: https://docs.rs/crate/stacker/0.1.25/source/build.rs
[psm-build]: https://docs.rs/crate/psm/0.1.32/source/build.rs
[sql-depth]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/src/parser/mod.rs#L432-L460
[pg-license]: https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/LICENSE
[native-license]: https://github.com/pganalyze/libpg_query/blob/18.1.0/LICENSE
[postgres-license]: https://www.postgresql.org/about/licence/
[xxhash-license]: https://github.com/pganalyze/libpg_query/blob/18.1.0/vendor/xxhash/xxhash.h#L1-L28
[upb-license]: https://github.com/pganalyze/libpg_query/blob/18.1.0/vendor/upb/LICENSE
[utf8-license]: https://github.com/pganalyze/libpg_query/blob/18.1.0/vendor/upb/third_party/utf8_range/LICENSE
[protobuf-c-license]: https://github.com/pganalyze/libpg_query/blob/17-6.2.5/vendor/protobuf-c/protobuf-c.c#L1-L24
[sql-license]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/LICENSE.TXT
[sql-notice]: https://github.com/apache/datafusion-sqlparser-rs/blob/v0.64.0/NOTICE.TXT
[create-index]: https://www.postgresql.org/docs/18/sql-createindex.html#SQL-CREATEINDEX-CONCURRENTLY
[server-query]: https://www.postgresql.org/docs/18/protocol-flow.html#PROTOCOL-FLOW-MULTI-STATEMENT
[server-errors]: https://www.postgresql.org/docs/17/protocol-error-fields.html
[alter17]: https://www.postgresql.org/docs/17/sql-altertable.html
[alter18]: https://www.postgresql.org/docs/18/sql-altertable.html
[grammar17]: https://github.com/postgres/postgres/blob/REL_17_12/src/backend/parser/gram.y#L4117-L4255
[grammar18]: https://github.com/postgres/postgres/blob/REL_18_7/src/backend/parser/gram.y#L4190-L4232
[catalog18]: https://www.postgresql.org/docs/18/catalog-pg-constraint.html
[partition17]: https://github.com/postgres/postgres/blob/REL_17_12/src/backend/commands/tablecmds.c#L10275-L10290
[pg18-notes]: https://www.postgresql.org/docs/18/release-18.html#RELEASE-18-CONSTRAINTS
[constraint-attr18]: https://github.com/postgres/postgres/blob/REL_18_7/src/backend/parser/gram.y#L19486-L19498
[do17]: https://www.postgresql.org/docs/17/sql-do.html
[psql17]: https://www.postgresql.org/docs/17/app-psql.html#APP-PSQL-META-COMMANDS
[pg18-version]: https://github.com/pganalyze/libpg_query/blob/18.1.0/src/pg_query_outfuncs_protobuf.c#L275-L283
