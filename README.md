# Database Migration Guard

Database Migration Guard is a PostgreSQL migration review tool under development. The MVP targets offline review of plain SQL and SQLx migrations for PostgreSQL 17 and 18. The [implementation plan](PLAN.md) defines the scope and acceptance checks.

Build the offline CLI and run the workspace tests from the repository root after installing the [native parser prerequisites](CONTRIBUTING.md#native-postgresql-parser).

```sh
cargo build --locked --release -p dmg-cli
cargo test --locked --workspace
```

See the [offline CLI quickstart](docs/cli.md), [GitHub pilot setup](docs/github-pilot.md), and [verification status](PROGRESS.md).
