# Progress

## DMG-03 workspace foundation

The shared Rust library defines source ranges, migration inventory, provenance and coverage, PostgreSQL targets, findings, policy TOML v1, and report JSON v1. The build workflow runs formatting, build, Clippy, and contract tests on Linux x64, Windows x64, and macOS arm64. See [decisions](DECISIONS.md) for the contract details.

Local verification passed with Rust 1.100.0 on native Windows x64 and Ubuntu 24.04.4 x64 in WSL2. Each host ran formatting, locked offline build, Clippy with warnings denied, and six contract tests after fetching the locked dependencies. Actionlint 1.7.12 validated the GitHub workflow locally.

A hosted GitHub Actions run and macOS arm64 verification are pending. The [implementation plan](PLAN.md#18-implementation-milestones) lists the outstanding release gates.

## DMG-06 first frozen holdout score

The independent scoring session checked the Windows x64 CLI at `052e947`, with SHA-256 `c952274d3038456f9cbb63684ae6d7b96f9eabeba46b5e6fdf4e6dd155e2b18e`. Each of the 100 report digests was identical across two runs. Gate and coverage outcomes matched for every combination, including 54 pass, 36 denied, and 10 incomplete. All 16 required destructive findings matched exactly. Every expected High or Critical risk produced its expected rule or a blocking result. The private evidence commit `d3db45a` preserves the first score and reproduction steps alongside the original independently reviewed labels.

Exact required finding agreement was 48 of 64. Eight PG002 findings were missing, four PG003 findings differed in affected-object granularity, and four PG007 findings had a wider source anchor. Four PG004 findings violated expected absences. Diagnostic agreement was 6 of 10 for category and path and 2 of 10 when requiring exact spans. The SQLx-shaped private cases used pinned Simple Query semantics, and the [public development evidence](fixtures/dmg-02/evidence/README.md) separately covers native SQLx execution. [Holdout protocol](fixtures/dmg-02/holdout.md)
