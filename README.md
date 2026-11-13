# Database Migration Guard

Database Migration Guard is a PostgreSQL migration review tool under development. The MVP targets offline review of plain SQL and SQLx migrations for PostgreSQL 17 and 18. The [implementation plan](PLAN.md) defines the scope and acceptance checks.

The Rust workspace currently provides the shared source, migration, policy, and report types in `dmg-core`. Build and run the contract tests from the repository root.

```sh
cargo build --locked --workspace
cargo test --locked --workspace
```

See [contributing](CONTRIBUTING.md) for toolchain and validation commands, [decisions](DECISIONS.md) for the v1 contracts, and [progress](PROGRESS.md) for verification status.
