# Database Migration Guard

Database Migration Guard reviews PostgreSQL migration changes before deployment. Its offline CLI checks documented plain SQL, SQLx, Flyway, Goose, and Supabase profiles for PostgreSQL 15–18. It compares exact Git revisions, reports risks and coverage, and applies trusted policy and evidence.

The CLI can also collect scoped read-only PostgreSQL evidence over TLS, compare schemas and application contracts, plan or run disposable Linux x86-64 rehearsals, and render saved results. GitHub and GitLab examples show how to run checks with protected inputs. [CLI quickstart](docs/cli.md) · [Supported SQL](docs/postgresql-support.md) · [GitHub](docs/github-pilot.md) · [GitLab](docs/gitlab-ci.md)

Install the [native parser prerequisites](CONTRIBUTING.md#native-postgresql-parser) before building from source.

~~~sh
cargo build --locked --release -p dmg-cli
cargo test --locked --workspace
~~~

This project is in local release preparation. See the [implementation plan](PLAN.md), [verification status](PROGRESS.md), and [release guide](docs/release.md) for the remaining platform, hosted CI, and distribution checks.
