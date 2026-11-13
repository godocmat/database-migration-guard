# DMG-01 PostgreSQL parser conformance spike

**13 November 2026**

Use `pg_query = "=18.1.0"` behind the Guard parser interface for the declared PostgreSQL 17 and 18 targets. It uses PostgreSQL 18.6 grammar and returned Protobuf version `180006`. The spike verified the reviewed subset on Linux x86-64 and Windows x86-64. Use Rust 1.100.0 with edition 2024 for foundation work. macOS arm64 and a disconnected clean Windows VM remain release acceptance checks. [Parser decision brief](../../docs/research/postgresql-parser.md)

## Conformance

Each executable emitted 21 observations per target. All 126 observations passed assertions for the expected facts, byte ranges, and diagnostics, including the expected limitations of the reference and alternate parsers. The same results were reproduced on both operating systems. The raw parser and server observations are in [evidence/](evidence/).

| Case | Result |
| --- | --- |
| Quoted CRLF multi-action DDL | The 18.1.0 scanner and AST yielded ordered actions, relation and existence flags, and token spans of `[30, 141)` and `[154, 166)`. The quoted name occupies `[58, 65)`. The `DROP COLUMN` action starts at byte 105, line 2, column 75. Parsing from byte 30 preserved the same original-file locations. |
| Indexes, updates, foreign keys | The preferred pin preserved index flags and predicate, transaction order, the broad and filtered update distinction, and foreign key identity and validation flags. |
| Named `NOT NULL ... NOT VALID` | The 18.1.0 AST retained the constraint name, column and flags. The reference pin (`6.2.1`, PostgreSQL 17.7, Protobuf `170007`) rejected it. The probe marked it incompatible when the declared target was 17. |
| Partitioned `FOREIGN KEY ... NOT VALID` | Both native grammars accepted it. The probe marked it incompatible on target 17 for a known partitioned relation and unresolved on target 17 when the relation kind was unknown. |
| Alternate `sqlparser 0.64.0` | The tested `std` feature build recovered the selected byte spans and basic facts. It rejected the supported PostgreSQL 18 named `NOT NULL` case and the server-accepted `DO` body, and accepted `UNIQUE ... NOT VALID`, which both servers rejected. The probe added a coverage diagnostic for that invalid constraint. |
| Malformed and unsupported input | Both native parsers recovered the trailing `DROP TABLE` after a malformed statement with verifiable boundaries. Coverage stayed incomplete. An unterminated dollar quote marked the whole segment incomplete. A parsed `DO` body and a client directive, unresolved template, embedded NUL, or invalid UTF-8 each produced incomplete coverage. |
| Preflight and recovery | In each executable a delimiter nesting depth of 65 exceeded the probe's bound of 64 and a 65,537-byte input exceeded the probe's 64 KiB bound. Both gave explicit preflight diagnostics. The subsequent valid input passed in the same process. |

The server harness used exactly PostgreSQL 17.12 and 18.7 with UTF-8, standard-conforming strings, and a single simple-protocol query per fixture. It reproduced 12 observations on each server. PostgreSQL 17.12 rejected the named constraint with SQLSTATE `42601` and the partitioned foreign key during execution with `42809`. PostgreSQL 18.7 recorded both as unvalidated. The ordinary foreign key control worked on both servers and changed from unvalidated to validated in the separate validation step. The malformed batch left its trailing drop unapplied. [Server 17.12 evidence](evidence/server-17.12.jsonl) · [Server 18.7 evidence](evidence/server-18.7.jsonl)

### Integration implications

- Keep a single 18-derived grammar in Guard. Apply version checks to normalized operations and treat relation kind as evidence for the partitioned foreign key case. Keep the 17 reference in a separate executable because both native libraries export the same symbols.
- Recover statement and subcommand ranges from original scanner tokens. `RawStmt` and node locations are starting points. `AlterTableCmd` has no action location field. Use half-open UTF-8 byte ranges and one-based Unicode scalar columns. Preserve incomplete coverage when tokenization fails, alignment is ambiguous, or a recognized action lacks required facts. This harness only aligns the fixture subset. Production splitting and nested expression analysis need their own bounded behavior and coverage tests.
- The Rust binding exposes messages for parse and scan failures. The native cursor is one-based in characters and can be mapped back to the original bytes, including across multibyte characters and at EOF. The spike repeats a failed call via the pinned C ABI to measure that behavior. For Guard, ship statement or segment precision through the public binding first. Use an upstream structured cursor or a narrowly audited binding change if exact diagnostic anchors become required. Keep diagnostic text redacted at report boundaries.

## Native build and offline smoke

Builds used `rustc 1.100.0 (0ef1bc386 2026-11-09)` with Cargo 1.100.0. Both `pg_query` probes used default crate features. The alternate used `default-features = false, features = ["std"]`. Builds used the vendored generated Protobuf sources, with `REGENERATE_PROTOBUF` unset and `protoc` absent from the build path.

| Target | Build and runtime evidence |
| --- | --- |
| Linux x86-64 | Ubuntu 24.04.4 on WSL2, GCC 13.3.0, glibc 2.39, and libclang 18.1.3. All three clean locked builds, formatting and Clippy passed. In a network-isolated minimal chroot, the three binaries reproduced their JSONL files with an empty environment and only the dynamic loader, libc and libgcc_s available. The 18.1.0 executable imports glibc symbols through 2.34. The exercised runtime baseline was glibc 2.39. |
| Windows x86-64 MSVC | Windows 11 build 26200, MSVC 19.44.35230, Windows SDK headers and libs 10.0.26100.0, and x64 LLVM 21.1.8. All three clean locked builds passed. The probes ran with the inherited environment cleared and the path reduced to Windows system directories. The preferred executable imports Windows libraries, UCRT and VCRUNTIME140. The host's VCRUNTIME140 was 14.51.36247.0. A disconnected VM with declared runtime prerequisites remains pending. |
| macOS arm64 | Native build and runtime pending on an Apple Silicon runner with Xcode command-line tools, an explicit deployment target, and arm64 libclang. |

### Reproduce

Install the pinned Rust toolchain, your target's C toolchain, and a loadable libclang. On Windows use an x64 MSVC developer shell with the Windows SDK and point `LIBCLANG_PATH` at the directory containing `libclang.dll`. On Linux, the observed `LIBCLANG_PATH` was `/usr/lib/llvm-18/lib`. Use the appropriate arm64 LLVM directory on macOS and record the deployment target. Run the following from this directory. The `.exe` suffix applies on Windows. `--offline` can be added to the build commands after Cargo has cached the locked dependencies. [Bindgen requirements](https://rust-lang.github.io/rust-bindgen/requirements.html) · [Rust MSVC target](https://doc.rust-lang.org/rustc/platform-support/windows-msvc.html) · [Rust macOS targets](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html)

```sh
cargo +1.100.0 fmt --all --check
cargo +1.100.0 clippy --locked --release --workspace -- -D warnings
cargo +1.100.0 build --locked --release -p dmg-pg18
cargo +1.100.0 build --locked --release -p dmg-pg17
cargo +1.100.0 build --locked --release -p dmg-sql
mkdir -p observations
target/release/dmg-pg18 > observations/dmg-pg18.jsonl
target/release/dmg-pg17 > observations/dmg-pg17.jsonl
target/release/dmg-sql > observations/dmg-sql.jsonl
python check.py --offline observations
```

For the server runs use the pinned source releases with separate installation prefixes. The WSL rerun used stock `./configure --prefix=...`, `make`, and `make install` builds. Use a non-root shell with Python 3 on Linux. The harness creates a private UNIX socket and a temporary cluster, sets a five-second statement timeout, and shuts down its own PostgreSQL process. It requires `libpq.so` from the corresponding build, with `--libpq` available for a different library path. `fsync` is disabled for the disposable fixture cluster. [PostgreSQL 17.12 source checksum](https://ftp.postgresql.org/pub/source/v17.12/postgresql-17.12.tar.bz2.sha256) · [PostgreSQL 18.7 source checksum](https://ftp.postgresql.org/pub/source/v18.7/postgresql-18.7.tar.bz2.sha256)

```sh
python3 server.py /path/to/pg-17.12 17.12 > observations/server-17.12.jsonl
python3 server.py /path/to/pg-18.7 18.7 > observations/server-18.7.jsonl
python3 check.py --servers observations
```

## Redistribution

The selected binary links the MIT binding, BSD-3-Clause `libpg_query`, PostgreSQL-licensed source, BSD-2-Clause xxHash, BSD-3-Clause upb, and MIT `utf8_range`. Release packaging should collect the tagged texts and embedded notices listed in the research brief. If the reference is distributed, include its protobuf-c notice as well. If the alternate is distributed, include the Apache-2.0 license and `NOTICE.TXT`. [Binding license](https://github.com/pganalyze/pg_query.rs/blob/v18.1.0/LICENSE) · [Bundled source and vendor licenses](../../docs/research/postgresql-parser.md#build-and-redistribution-evidence)

The locked normal dependency graph for the preferred probe includes bytes and zmij (MIT), prost (Apache-2.0), memchr (Unlicense or MIT), and the dual-license either, itertools, serde, serde_core, serde_json, itoa, and thiserror packages. Record proc-macro and build tools in the build inventory. Review the final compiled and packaged graph, including runtime redistributables if bundled, when generating notices and the SBOM. [Pinned Cargo graph](Cargo.lock)
