# Progress

## DMG-03 workspace foundation

The shared Rust library defines source ranges, migration inventory, provenance and coverage, PostgreSQL targets, findings, policy TOML v1, and report JSON v1. The build workflow runs formatting, build, Clippy, and contract tests on Linux x64, Windows x64, and macOS arm64. See [decisions](DECISIONS.md) for the contract details.

Local verification passed with Rust 1.100.0 on native Windows x64 and Ubuntu 24.04.4 x64 in WSL2. Each host ran formatting, locked offline build, Clippy with warnings denied, and six contract tests after fetching the locked dependencies. Actionlint 1.7.12 validated the GitHub workflow locally.

A hosted GitHub Actions run and macOS arm64 verification are pending. The [implementation plan](PLAN.md#18-implementation-milestones) lists the outstanding release gates.
