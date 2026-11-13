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

`ae83f53` adds scoped catalog context to offline `scan` and `check`. Guard verifies the saved artifact's byte digest, target, PostgreSQL major, and freshness. Positive observations and labeled estimates can refine finding explanations. Static risk and gate conclusions stay conservative. The integrated Windows workspace passed 147 active tests, formatting, and Clippy. [Catalog context workflow](docs/catalog-context.md)

## DMG-11 schema diff and application contracts

The core schema comparison distinguishes incomplete coverage from known absence and requires reviewed rename mappings. The application evaluator checks exact deployed dependencies for type, nullability, and structural write regressions. Windows verification passed nine diff, eight application-contract, and four dedicated `dmg diff` cases. The live CLI fixture at `102af26` passed on PostgreSQL 15.20, 16.16, 17.12, and 18.7 with verified TLS and a restricted reader. It checked drift, reviewed renames, critical contract regressions, and incomplete scopes. [Live verification](docs/research/inspect-diff-verification.md) · [Schema diff](docs/diff.md) · [Application contracts](docs/application-contracts.md)

## PostgreSQL 15 and 16 target verification

PostgreSQL 15.20 and 16.16 matched the PostgreSQL 17.12 expectations for the supported parser, lock, and runner fixtures. Each older major passed 12 simple-query conformance cases, 41 native psql and SQLx cases, five lock cases, 23 Flyway invocations, 19 Goose cases, and 20 Supabase cases. Both returned `42601` for the PostgreSQL 18 named NOT NULL fixture and `42809` for a NOT VALID foreign key on a partitioned parent. Static CLI targets 15 and 16 use the same pre-18 compatibility checks. [Parser 15.20 evidence](spikes/dmg-01-parser/evidence/server-15.20.jsonl) · [Parser 16.16 evidence](spikes/dmg-01-parser/evidence/server-16.16.jsonl) · [Native observations](fixtures/dmg-02/evidence) · [Adapter observations](fixtures/dmg-09)

The tests used OpenSSL-enabled source builds under Ubuntu 24.04.4 x86-64 in WSL2 with private network and IPC namespaces. The archive digests match upstream SHA-256 files. Use the original probe commands with the corresponding versioned server and client paths. The 130-combination public development corpus retains its original PostgreSQL 17 and 18 labels. [PostgreSQL 15.20 source checksum](https://ftp.postgresql.org/pub/source/v15.20/postgresql-15.20.tar.bz2.sha256) · [PostgreSQL 16.16 source checksum](https://ftp.postgresql.org/pub/source/v16.16/postgresql-16.16.tar.bz2.sha256)

The expanded CLI accepted common DDL on targets 15 and 16 and returned incomplete for the PostgreSQL 18-only syntax. Windows and Linux workspace tests and Clippy passed at `a3fde4f`. The existing 24-recipe discovery and 130-combination rule corpora passed on Linux. The Windows CLI matched the 130 expected gate outcomes, including 48 passing, 58 denied, and 24 incomplete.

## DMG-12 disposable PostgreSQL worker

The worker at `dfb48f9` passed the Rust lifecycle test against each pinned PostgreSQL image, from 15.20 through 18.7, on isolated Docker Engine 29.9.3 daemons in WSL2 with cgroup v1. The tests verified effective namespace and resource controls, restricted authentication, and removal after successful SQL, rollback, server and client deadlines, injected bootstrap failure, and cancellation with an active runtime. The Windows build passed with the explicit unsupported-platform path. [Worker controls and image pins](docs/research/simulation-worker.md)

Revision-bound rehearsal and runner delivery landed at `abb1975`. The loopback native Windows PostgreSQL 18.7 fixture passed nine runner and transaction cases plus server cancellation. It verified the raw protocol and execution core. [Rehearsal profile and verification](docs/simulation.md)

`1e67576` adds `dmg simulate` with offline expected-binding generation, typed preflight and runtime outcomes, and atomic evidence output. Windows checks covered exact setup and candidate bindings, valid Linux worker paths on an unsupported host, and pinned import of the emitted incomplete report by `dmg check`. [Simulation command](docs/simulation.md#run-from-the-cli)

The offline Ubuntu 24.04.5 x86-64 guest passed the cgroup v2 lifecycle, server and wall deadlines, and nine rehearsal scenarios on each pinned PostgreSQL release. Docker Engine 29.9.3 had zero containers after every test group. A seeded PostgreSQL 18.7 CLI smoke bound exact Git revisions and verified committed success, rolled-back failure, and a missing report through protected `dmg check` exits 0, 1, and 3. HTML and Markdown preserved both saved outcomes. A separate native CLI smoke verified a schema-only baseline with no seed and a committed catalog delta on PostgreSQL 18.7. [Cgroup v2 verification and scope](docs/research/simulation-cgroup-v2-verification.md) · [Live schema-only CLI verification](docs/research/release-verification-ci.md#offline-wrapper-and-schema-only-replay)

## DMG-13 policy evidence and DMG-14 report formats

`dd5d589` verifies protected exact-byte pins, reproduces baseline findings from the reviewed Git reference, audits exact scoped approvals and expiry, and independently reconstructs the rehearsal plan from Git. Windows workspace tests, Clippy, and the local GitHub replay passed. All 130 public CLI cases matched the expected 48 passing, 58 denied, and 24 incomplete outcomes. The policy acceptance fixtures used saved rehearsal evidence. The protected publisher supplies the approved worker binding and exact artifact pin. [Policy evidence workflow](docs/policy-evidence.md)

`d650aac` renders saved analysis as HTML, SARIF 2.1.0, and GitLab Code Quality. The focused Windows CLI and renderer tests passed. Four fixture reports passed the OASIS SARIF schema and local HTML and Code Quality checks. `a24fb60` adds HTML and Markdown review for saved schema comparisons and rehearsal outcomes. Tests verified real CLI artifacts and preserved incomplete coverage, cleanup status, and transactional durability. Hosted ingestion is pending. [Report format and limits](docs/report-formats.md)

The GitLab gate and fork advisory examples landed at `22d9b06`. The Windows local replay passed ten checks, including exact revisions and parents, protected base configuration, token isolation, retained reports, and pass, deny, and incomplete outcomes. GitLab's published job and security policy schemas accepted the examples. The protected profile applies to same-project merged results. Fork scans are advisory. A runner image build and hosted enforcement are pending. [GitLab setup and acceptance checks](docs/gitlab-ci.md)

## DMG-15 frozen holdout rescore

The frozen independent holdout was rescored against the integrated Windows x86-64 release build from `d1942fa`, SHA-256 `0db7642f5804a210ff479fa9e2a27933e11b0d65eacf462f4dfa7c52b79df35f`. All 100 gate and coverage outcomes matched. The 16 required destructive findings matched, and every expected High or Critical case produced its required rule or a blocking result. The exact finding and diagnostic scores stayed at 48 of 64 required findings and 2 of 10 exact diagnostic anchors. Four expected absences were violated. The gate returned 54 passes, 36 denials, and 10 incomplete outcomes. The private evidence commit `5b1e244` retains the detailed rescore. The public fixture replay also matched all 130 gate outcomes with this binary. [Holdout protocol](fixtures/dmg-02/holdout.md)

Ruleset 2 adds PG002 compatibility findings for table and column removal alongside PG001. The frozen rescore of the staged Windows `af21401` candidate matched all 100 gate and coverage outcomes and all 16 required destructive findings. Exact required findings rose from 48 to 56 of 64, including all 16 PG002 findings. Four PG003 object identities, four PG007 anchors, eight exact diagnostic anchors, and four expected absences still differed from the frozen labels. No expected High or Critical rule was missing with a nonblocking result. Private commit `397793e` retains the rescore. [Rules](docs/rules.md) · [Staged candidate digest and public corpus](docs/research/integrated-windows-verification.md)

The DMG-15 benchmark ran 30 repetitions of eight synthetic workloads on each platform. The Windows `22d9b06` baseline took 2.474 seconds at p95 with a 108.46-MiB peak working set for the 5,000-file, 50-MiB scan. The cross-built Linux `af21401` executable took 1.389 seconds at p95 with 105.88 MiB peak RSS on an offline 2-vCPU, 8-GiB Ubuntu guest. It met the 15-second and 512-MiB planning targets. Four concurrent Linux scans took 2.577 seconds at p95 and retained identical reports. The guest then passed the installed-path smoke, all 130 public cases, both CI replays, and rollback to the retained Linux pilot. [Benchmark method, scope, and raw evidence](fixtures/dmg-15/README.md)

The independent boundary review at `192d13b` made Unix CLI file opens nonblocking before validating the opened handle. It also added the optional `cleanup_worker` name to unconfirmed rehearsal evidence for same-daemon reconciliation. The current decoder accepts older reports. Older strict decoders reject this field when present. Windows workspace tests and Clippy passed, and the Linux x86-64 CLI tests cross-compiled. Large saved reports can still use more memory than their input byte size, and exact Git discovery has per-command deadlines. An abruptly stopped caller or daemon can prevent a cleanup record. [Worker recovery](docs/research/simulation-worker.md#postgresql-connection-and-teardown)

Cargo Audit 0.22.2 evaluated the exact integrated Cargo.lock against RustSec advisory database `4ef2df8e806a65e2f1ecc14f56f51249062b23d6`. It reported zero known vulnerabilities and zero warnings across 226 locked packages. The local scan used a pinned advisory checkout and skipped the online yanked-package check. Separate evidence covers vendored native code, runtime packages, and container contents. [RustSec advisory database](https://github.com/RustSec/advisory-db/tree/4ef2df8e806a65e2f1ecc14f56f51249062b23d6)

The staged Windows `af21401` executable also passed 201 generated, bounded CLI inputs across PostgreSQL targets 15–18. The cases included 48 valid source-span and line-ending variants, 60 malformed SQL inputs, 48 invalid project configurations, and 45 invalid policy files. The probe checked deterministic repeated reports, incomplete SQL coverage, and preservation of an existing report on invalid configuration. The probe covered the installed CLI and used synthetic inputs without a database connection.

## DMG-16 local OCI preview

The `ae83f53` Linux x86-64 release executable cross-compiled with Rust 1.100.0, Zig 0.16.0, and the glibc 2.39 target. Its SHA-256 is `66bc72d299d2525805789d8025aa2c2c65e2ae9ea812a14135fac92b6029b80b`. A local unsigned OCI layout assembled that executable and the pinned Ubuntu 24.04 `buildpack-deps:noble-scm` base into manifest `sha256:7a4362cde4d857107b833d254858e707511a9c3531ff990c51d5652d8e1bdb3c`. Static verification checked the base and added layer digests, Linux x86-64 ELF header, executable permissions, exact executable digest, and non-root entrypoint configuration.

Syft 1.57.1 detected 740 image filesystem components, including Git and glibc. The CycloneDX 1.5 schema accepted the scan after five license records used their SPDX names and URLs for two identifiers introduced after the schema's SPDX list. The image scan covers filesystem components. Application and build dependencies are documented separately. The subsequent native candidate OCI artifacts have their own runtime and scan evidence. [OCI image format](https://github.com/opencontainers/image-spec/tree/v1.1.1) · [CycloneDX 1.5 schema](https://github.com/CycloneDX/specification/tree/1.5/schema)

## DMG-16 integrated Windows package

Two independent native stages of `af21401` on Windows x86-64 reproduced the executable, manifest, CycloneDX 1.5 SBOM and checksums byte for byte. The installed candidate passed the 130 public checks, three GitHub replay outcomes, ten GitLab replay tests, and revision-based upgrade and rollback with the retained pilot. Formatting, strict Clippy, and 152 active workspace tests passed. The smoke used isolated local profiles on the existing host with networking enabled. Clean disconnected Windows installation, hosted enforcement, and signatures remain pending. [Integrated Windows package verification](docs/research/integrated-windows-verification.md)

## DMG-16 integrated Linux package

Two native Linux x86-64 stages of `af21401` on an offline Ubuntu 24.04.5 guest reproduced all four package files byte for byte. The exported package passed independent manifest and CycloneDX 1.5 verification. A regular user ran the installed smoke, both local CI replays, all 130 public cases, and rollback to the retained pilot. The Linux and earlier Windows staging manifests reflect LF and CRLF build inputs respectively. [Integrated Linux package and exact inputs](docs/research/integrated-linux-verification.md)

The release tooling now disables host Git line-ending conversion during staging and compares the archived lockfile and toolchain against their committed Git blobs. Local regression checks covered conflicting Git defaults, SHA-1 and SHA-256 repositories, and rejection of converted or missing pinned files. The manual GitHub workflows stage native packages, scan the pinned Cargo advisory database, and run the four PostgreSQL worker suites from a protected default branch. Workflow lint, helper checks, and the SBOM generator's producer attestations passed locally. The exact rehearsal wrapper also passed in the no-network Linux guest on PostgreSQL 18.7 and preserved an existing socket outside its mount namespace. Hosted dispatch remains pending. [Manual verification workflows](docs/research/release-verification-ci.md)

The native package notice review checked all 180 registry archives against the committed Cargo.lock and collected 400 pinned notice files for review. The draft covers both x86-64 native candidates and identifies outstanding generated-source, runtime, image and macOS scope. Project licensing and approval of the final notice set remain release-owner decisions. [Notice inventory and source pins](docs/research/release-notices-review.md)

## DMG-16 native Linux container artifacts

BuildKit produced the pinned CLI and GitLab OCI archives offline from the exact native `af21401` executable. Docker runtime checks verified the CLI's nonroot exact Git decisions and all ten replays using the installed GitLab gate. The GitLab recipe now uses the Ubuntu SCM base and explicitly sets the executable's parent directory to mode 0755. Both image filesystem SBOMs passed CycloneDX 1.5 validation. The image scans retain Ubuntu package findings and eight upstream binary matches with outstanding Canonical review or fixes. Hosted enforcement, signed provenance, and release review remain pending. [OCI artifacts and advisory evidence](docs/research/container-verification.md)
