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
