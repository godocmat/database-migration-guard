# Offline CLI

Build `dmg` from the workspace root with `cargo build --locked --release -p dmg-cli`. The executable is under `target/release`, with an `.exe` suffix on Windows. See the native parser prerequisites before building. [Build instructions](../CONTRIBUTING.md)

```sh
dmg init --target 18
dmg scan --repo . --config dmg.toml
dmg policy validate --policy policy.toml
dmg check --repo . --config dmg.toml --policy policy.toml --format json
```

`init` writes a starter `dmg.toml` and preserves any existing file at that path. Set `--config` to choose a different destination. Adjust the generated roots, filename pattern, and transaction setting to match the repository's runner.

## Project configuration

```toml
schema_version = 1
target = 18
roots = ["migrations"]
search_path = ["public"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z][a-z0-9_]*\.sql$'
version_order = "positive_integer"
transaction = "file"
```

Select PostgreSQL `15`, `16`, `17`, or `18`. Roots are relative to `--repo`, which defaults to the current directory. File arguments such as `--config`, `--policy`, and `--output` resolve from the invocation directory. `--config` defaults to `dmg.toml`. Use `transaction = "autocommit"` for a plain runner that submits individual statements and follows explicit SQL transaction commands. The default static search path is `["public"]`. One configured schema resolves an unqualified name to that schema. For several possible schemas, qualify names in migrations where the rules need a resolved identity.

For SQLx 0.8.6, use `[adapter]` with `kind = "sqlx"` and `layout = "simple"` or `"reversible"`. Reversible Down files remain in the integrity inventory. [Migration discovery and transaction modes](migration-formats.md)

For Flyway OSS 13.12.2, set `kind = "flyway"` and mirror the runner's `execute_in_transaction`, `mixed`, and `transactional_lock` settings. [Flyway configuration and limits](flyway-migrations.md)

For Goose 3.30.1, set `kind = "goose"` and supply one migration root. Combined SQL files retain distinct Up and Down inventory entries when both directions are authored. [Goose configuration and limits](goose-migrations.md)

For Supabase CLI 3.0.0, set `kind = "supabase"` and supply one root, typically `roots = ["supabase/migrations"]`. [Supabase configuration and limits](supabase-migrations.md)

## Trusted inputs

With no revision arguments, Guard scans the working directory. Supply both `--base` and `--head` to use exact local commits. Each argument is a full Git object ID. Guard reads the candidate migration bytes from Git even if the checked-out files differ. A missing commit or blob yields incomplete coverage. Both the project configuration and policy remain explicit files supplied by the caller.

```sh
dmg check --repo . --config /trusted/dmg.toml --policy /trusted/policy.toml \
  --base "$BASE_SHA" --head "$HEAD_SHA" --format json --output report.json
```

In a protected workflow, load the configuration and policy from trusted inputs and pass the reviewed revision IDs. The CLI applies the supplied policy. Repository and CI protection determine who can select those inputs.

For worktree integrity evidence, pair `--integrity-manifest FILE` with `--integrity-sha256 SHA256`. Pair `--applied-manifest FILE` with `--applied-sha256 SHA256` for authorized applied history in either input mode. Use lowercase SHA-256 digests from a trusted source. A supplied manifest that cannot be read or verified produces an incomplete report. Git integrity and applied-history evidence have separate provenance. [Manifest formats](migration-formats.md#trusted-manifests)

For authorized catalog collection or native history from SQLx, Flyway, Goose, or Supabase, use the explicit inspection workflow. An offline scan or check can import a pinned history artifact for its discovered candidate. [Inspection and saved history](inspect.md)

## Policy and process exit

```toml
schema_version = 1
deny_at = "high"

[integrity]
require_git_base = true
deny_modified = true
deny_missing = true

[rules.PG005]
severity = "medium"
```

`schema_version = 1` alone selects defaults. The default threshold is `high`. With a baseline, the default policy denies edits to versioned migrations and missing migrations, including removed Flyway repeatables. Edited repeatables remain selected for analysis and the finding threshold applies. `require_git_base` defaults to false. When true, the check requires a complete comparison between the trusted Git base and candidate. Per-rule overrides appear as effective severities in the check report. A finding emitted as critical remains critical. `policy validate` rejects unknown fields, versions, and rule IDs.

| Exit | Meaning |
| --- | --- |
| 0 | Complete scan, passing check, successful init, or valid policy |
| 1 | Policy denied the completed check |
| 2 | Invalid command, project configuration, or policy |
| 3 | Incomplete required coverage or evidence, or operational failure |

A scan can succeed with high or critical findings. A check validates invocation and configuration first, checks required coverage and evidence second, and applies policy thresholds and integrity outcomes last. The `[exit_codes]` project table can override `denied` and `incomplete`. Supply distinct integers from 1 through 125 and leave 2 reserved for invalid input. JSON decision statuses stay `pass`, `denied`, and `incomplete`.

Text and JSON reports go to stdout unless `--output` is supplied. Fatal diagnostics go to stderr. Both denied and incomplete analyses still emit their report. A file report replaces its destination atomically after rendering succeeds. An invalid invocation or configuration leaves an existing report in place. Findings include repository-relative source coordinates and explanations. Reports omit migration SQL text and include the original policy SHA-256 for checks. [Report schema and source coordinates](../DECISIONS.md)

### Protected policy evidence

Add `[evidence]` to trusted policy TOML to authorize evidence for an opaque repository and target. With the example below, each `PG001` finding requires approval and a rehearsal completed in the past hour. A critical `PG001` finding can pass that rule only with an exact approval that reviewed the successful rehearsal. Its severity stays critical in the report. Other critical findings block the check. A baseline may suppress the severity threshold for an exact matching noncritical finding. A baseline match leaves every approval, integrity, and coverage requirement in force.

```toml
[evidence]
repository_id = "example-repo"
target_id = "production"
require_baseline = false
require_approvals = ["PG001"]
allow_critical_approvals = ["PG001"]

[evidence.rehearsal]
rules = ["PG001"]
max_age_seconds = 3600
require_complete_schema = true
```

Omit `allow_critical_approvals` to use the default critical denial. Every rule in that list must also require approval and rehearsal. Omit `[evidence.rehearsal]` for checks that use only baselines or approvals. Within the rehearsal table, omit `rules` or use an empty list to require rehearsal on every check. `require_complete_schema` also requires complete before and after schema observations and diff coverage on successful runs. The age limit is an integer greater than zero. If supplied, the native history target ID must equal the policy target ID.

```sh
dmg check --repo . --config /trusted/dmg.toml --policy /trusted/policy.toml \
  --base "$BASE_SHA" --head "$HEAD_SHA" \
  --policy-evidence /trusted/evidence.json \
  --policy-evidence-sha256 "$TRUSTED_EVIDENCE_SHA256" \
  --rehearsal-report rehearsal.json --format json --output report.json
```

Supply the bundle SHA-256 from the protected workflow that authorized it. In that workflow, use the reviewed revision, exact migration sources, approved image, and approved setup to establish the expected rehearsal binding. Retain the authorized bundle and its digest independently of the candidate artifacts. The checker validates the exact report bytes against the pin and independently reconstructs the ordered execution plan and statement spans from Git. It also checks the report against the authorized environment, configured search path, original policy and configuration, current clock, and verified cleanup.

An absent, stale, mismatched, or malformed required rehearsal gives an incomplete check. Supplying optional evidence also triggers verification. A verified rehearsal with a PostgreSQL migration failure denies when other required coverage and evidence are complete. A missing, expired, or mismatched approval denies the finding when its rule requires approval. [Policy evidence JSON format](policy-evidence.md)

## Review formats

Render a saved version 1 JSON report as Markdown, GitHub Actions annotations, standalone HTML, SARIF 2.1.0, or GitLab Code Quality. Rendering returns success when the requested output is written. Use the original `dmg check` status for the gate decision. [Saved report formats](report-formats.md)

```sh
dmg report --input report.json --format markdown --output summary.md
dmg report --input report.json --format github
```
