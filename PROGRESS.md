# Progress

## DMG-03 workspace foundation

The shared Rust library defines source ranges, migration inventory, provenance and coverage, PostgreSQL targets, findings, policy TOML v1, and report JSON v1. The build workflow runs formatting, build, Clippy, and contract tests on Linux x64, Windows x64, and macOS arm64. See [decisions](DECISIONS.md) for the contract details.

Local verification passed with Rust 1.100.0 on native Windows x64 and Ubuntu 24.04.4 x64 in WSL2. Each host ran formatting, locked offline build, Clippy with warnings denied, and six contract tests after fetching the locked dependencies. Actionlint 1.7.12 validated the GitHub workflow locally.

A hosted GitHub Actions run and macOS arm64 verification are pending. The [implementation plan](PLAN.md#18-implementation-milestones) lists the outstanding release gates.

## DMG-06 first frozen holdout score

The independent scoring session checked the Windows x64 CLI at `052e947`, with SHA-256 `c952274d3038456f9cbb63684ae6d7b96f9eabeba46b5e6fdf4e6dd155e2b18e`. Each of the 100 report digests was identical across two runs. Gate and coverage outcomes matched for every combination, including 54 pass, 36 denied, and 10 incomplete. All 16 required destructive findings matched exactly. Every expected High or Critical risk produced its expected rule or a blocking result. The private evidence commit `d3db45a` preserves the first score and reproduction steps alongside the original independently reviewed labels.

Exact required finding agreement was 48 of 64. Eight PG002 findings were missing, four PG003 findings differed in affected-object granularity, and four PG007 findings had a wider source anchor. Four PG004 findings violated expected absences. Diagnostic agreement was 6 of 10 for category and path and 2 of 10 when requiring exact spans. The SQLx-shaped private cases used pinned Simple Query semantics, and the [public development evidence](fixtures/dmg-02/evidence/README.md) separately covers native SQLx execution. [Holdout protocol](fixtures/dmg-02/holdout.md)

## DMG-08 local pilot packages

Both packages stage `dmg 0.1.0` from clean checkouts of `c4974cd11180440d710c0843a952caa11ce786e3` with Rust 1.100.0 and the locked dependencies. The package manifests bind the source commit, target, executable, Cargo.lock, and toolchain digests. The local pilot packages are unsigned.

| Platform | Executable bytes | SHA-256 |
| --- | ---: | --- |
| Windows x86-64, Windows 11 Pro build 26200 | 7,268,352 | `e1c374c1a0c2b08c2fdd34f04f3ebe38dc811fbd5077000ff8f5853449be3c97` |
| Linux x86-64, Ubuntu 24.04.4, glibc 2.39 | 9,445,912 | `694eb4ea18e74121e6ac23e8b6c1d96799a98917f844c95e5e417fbbdf355455` |

Each packaged executable passed checksum and version checks, then matched 48 passing, 58 denied, and 24 incomplete outcomes in the 130 public Git checks. Each platform passed the GitHub replay with exits 1, 0, and 3 and verified exact merge ancestry, protected config and policy, and retained report provenance. The Linux replay ran with network access disabled. The Windows replay used Git and system paths and the installed Visual C++ runtime. A disconnected Windows VM, hosted GitHub PR and fork checks, protected ruleset enforcement, and HTTPS artifact delivery remain pending. [Replay and setup instructions](docs/github-pilot.md)

## DMG-09 native migration adapters

The integrated revision `60a83fd` passed formatting, locked offline workspace tests, Clippy, 24 discovery recipes, and 130 discovery, parser, and risk combinations on Windows and Linux. The Windows CLI separately matched 48 pass, 58 denied, and 24 incomplete in the public Git corpus and passed the local GitHub replay. Focused CLI checks covered Flyway repeatable integrity and transaction defaults, Goose shared-file directions and grouped queries, and Supabase lexical versions and automatic transaction splitting. [Adapter configuration](docs/cli.md#project-configuration)

The public native runner evidence covers 23 Flyway invocations, 19 Goose cases, and 20 Supabase cases on each of PostgreSQL 17.12 and 18.7. The verified profiles report unresolved runner behavior as incomplete. The initial profiles leave Flyway script sidecars and authored transactions, Goose environment substitution and authored transactions, and Supabase legacy init selection and managed schemas outside complete coverage. [Flyway](fixtures/dmg-09/flyway/README.md) · [Goose](fixtures/dmg-09/goose/README.md) · [Supabase](fixtures/dmg-09/supabase/README.md)

## DMG-10 inspection and native history

The versioned catalog, snapshot, and history contracts and read-only TLS transport are integrated. The restricted native catalog probe covered PostgreSQL 15.20, 16.16, 17.12, and 18.7. On PostgreSQL 18.7 the Rust transport verified the supplied CA and endpoint name, refused plaintext, redacted errors, and cleaned up the connection after a deadline. [Catalog feasibility](docs/research/postgresql-inspection.md) · [Transport evidence](docs/research/postgresql-transport.md) · [Evidence contracts](docs/inspection-evidence.md)

The reconciliation library at `66c4426` verifies the adapter, target, and forward inventory. SQLx 0.8.6 and Flyway 13.12.2 use native checksums. Goose 3.30.1 and Supabase CLI 3.0.0 supply presence evidence. A missing or incomplete history read leaves classifications unknown unless a supported observation establishes failure. Locked offline workspace tests, formatting, and Clippy passed on Windows and Linux, along with the 24-recipe discovery corpus. Disposable PostgreSQL 17.12 and 18.7 runs verified SQLx modified, missing, and dirty history cases and Flyway repeatable and checksum behavior. The dirty case used a seeded SQLx history row. [Native reconciliation limits](docs/migration-formats.md#native-applied-history)

The Rust catalog collector passed six restricted-role TLS scenarios on each of the four PostgreSQL majors. It checked schema visibility, projected table metadata, masked endpoints, limits, permission failures, and contention. Definitions and total relation bytes are opt-in. [Catalog coverage and reproduction](docs/postgresql-catalog.md)

The native history reader passed its three fixture groups on each major over verified TLS. The fixtures cover all four supported history shapes, column-only grants, empty versus unavailable history, bounded contention, and concurrent replacement of the designated table. The Flyway undo record is synthetic. The refactored transport and catalog collector also passed their PostgreSQL 18.7 TLS fixtures on `ad8a76b`. [History reader verification](docs/postgresql-history.md)

`dmg inspect` and offline pinned history import landed at `2ef5b6b`. The restricted-role PostgreSQL 18.7 CLI fixture verified scoped catalogs, incomplete access, invalid CA, target mismatch, bounded projections, and SQLx history. Windows tests verified exact Git candidate binding, raw file pinning, and unavailable history. Catalog and history are independent reads. [CLI workflow](docs/inspect.md)

## DMG-11 schema diff and application contracts

The core schema comparison distinguishes incomplete coverage from known absence and requires reviewed rename mappings. The application evaluator checks exact deployed dependencies for type, nullability, and structural write regressions. Windows verification passed nine diff, eight application-contract, and four dedicated `dmg diff` cases. The live CLI fixture at `102af26` passed on PostgreSQL 15.20, 16.16, 17.12, and 18.7 with verified TLS and a restricted reader. It checked drift, reviewed renames, critical contract regressions, and incomplete scopes. [Live verification](docs/research/inspect-diff-verification.md) · [Schema diff](docs/diff.md) · [Application contracts](docs/application-contracts.md)

## PostgreSQL 15 and 16 target verification

PostgreSQL 15.20 and 16.16 matched the PostgreSQL 17.12 expectations for the supported parser, lock, and runner fixtures. Each older major passed 12 simple-query conformance cases, 41 native psql and SQLx cases, five lock cases, 23 Flyway invocations, 19 Goose cases, and 20 Supabase cases. Both returned `42601` for the PostgreSQL 18 named NOT NULL fixture and `42809` for a NOT VALID foreign key on a partitioned parent. Static CLI targets 15 and 16 use the same pre-18 compatibility checks. [Parser 15.20 evidence](spikes/dmg-01-parser/evidence/server-15.20.jsonl) · [Parser 16.16 evidence](spikes/dmg-01-parser/evidence/server-16.16.jsonl) · [Native observations](fixtures/dmg-02/evidence) · [Adapter observations](fixtures/dmg-09)

The tests used OpenSSL-enabled source builds under Ubuntu 24.04.4 x86-64 in WSL2 with private network and IPC namespaces. The archive digests match upstream SHA-256 files. Use the original probe commands with the corresponding versioned server and client paths. The 130-combination public development corpus retains its original PostgreSQL 17 and 18 labels. [PostgreSQL 15.20 source checksum](https://ftp.postgresql.org/pub/source/v15.20/postgresql-15.20.tar.bz2.sha256) · [PostgreSQL 16.16 source checksum](https://ftp.postgresql.org/pub/source/v16.16/postgresql-16.16.tar.bz2.sha256)

The expanded CLI accepted common DDL on targets 15 and 16 and returned incomplete for the PostgreSQL 18-only syntax. Windows and Linux workspace tests and Clippy passed at `a3fde4f`. The existing 24-recipe discovery and 130-combination rule corpora passed on Linux. The Windows CLI matched the 130 expected gate outcomes, including 48 passing, 58 denied, and 24 incomplete.

## DMG-12 disposable PostgreSQL worker

The worker at `dfb48f9` passed the Rust lifecycle test against each pinned PostgreSQL image, from 15.20 through 18.7, on isolated Docker Engine 29.9.3 daemons in WSL2 with cgroup v1. The tests verified effective namespace and resource controls, restricted authentication, and removal after successful SQL, rollback, server and client deadlines, injected bootstrap failure, and cancellation with an active runtime. The Windows build passed with the explicit unsupported-platform path. [Worker controls and image pins](docs/research/simulation-worker.md)

Revision-bound rehearsal and runner delivery landed at `abb1975`. The loopback native Windows PostgreSQL 18.7 fixture passed nine runner and transaction cases plus server cancellation. It verified the raw protocol and execution core. The CLI and worker integration test binary at `102af26` cross-compiled for Linux x86-64 with Rust 1.100.0 and glibc 2.39. End-to-end Docker rehearsal on PostgreSQL 15–18 and cgroup v2 lifecycle tests remain pending. [Rehearsal profile and verification](docs/simulation.md)

`1e67576` adds `dmg simulate` with offline expected-binding generation, typed preflight and runtime outcomes, and atomic evidence output. Windows checks covered exact setup and candidate bindings, valid Linux worker paths on an unsupported host, and pinned import of the emitted incomplete report by `dmg check`. [Simulation command](docs/simulation.md#run-from-the-cli)

## DMG-13 policy evidence and DMG-14 report formats

`dd5d589` verifies protected exact-byte pins, reproduces baseline findings from the reviewed Git reference, audits exact scoped approvals and expiry, and independently reconstructs the rehearsal plan from Git. Windows workspace tests, Clippy, and the local GitHub replay passed. All 130 public CLI cases matched the expected 48 passing, 58 denied, and 24 incomplete outcomes. The policy acceptance fixtures used saved rehearsal evidence. The protected publisher supplies the approved worker binding and exact artifact pin. [Policy evidence workflow](docs/policy-evidence.md)

`d650aac` renders saved analysis as HTML, SARIF 2.1.0, and GitLab Code Quality. The focused Windows CLI and renderer tests passed. Four fixture reports passed the OASIS SARIF schema and local HTML and Code Quality checks. Hosted ingestion and standalone diff and rehearsal rendering are pending. [Report format and limits](docs/report-formats.md)

The GitLab gate and fork advisory examples landed at `22d9b06`. The Windows local replay passed ten checks, including exact revisions and parents, protected base configuration, token isolation, retained reports, and pass, deny, and incomplete outcomes. GitLab's published job and security policy schemas accepted the examples. The protected profile applies to same-project merged results. Fork scans are advisory. A runner image build and hosted enforcement are pending. [GitLab setup and acceptance checks](docs/gitlab-ci.md)
