# DMG-02 development fixtures

The [development manifest](manifest.json) instantiates the 28 reviewed designs as 38 variants and 130 variant, adapter, and PostgreSQL-major combinations. Each fixture uses synthetic SQL and rows. The PostgreSQL pins are 17.12 and 18.7, with matching `psql` clients and SQLx CLI and library 0.8.6. The [research brief](../../docs/research/fixtures-and-competitors.md) records the source rationale. The [evidence log](evidence/README.md) records structural checks and native observations. The [holdout procedure](holdout.md) and [pilot protocol](pilot.md) are ready for their respective reviewers.

## Run or inspect a case

From the repository root, use Python 3.8+ and Git with `git init --initial-branch` and `--object-format` support. Choose a new output directory whose parent already exists.

```sh
python -B fixtures/dmg-02/corpus.py list
python -B fixtures/dmg-02/corpus.py validate
python -B fixtures/dmg-02/corpus.py materialize T04/implicit-block/sqlx-simple/pg17 ./dmg02-t04
```

A run ID takes the form `case/variant/profile/pg<major>`. Profiles specify plain SQL, simple SQLx, or reversible SQLx. Materialization writes a clean candidate checkout in `repo/` and a sibling `case.json` with its explicit base and head commit IDs, ordered inventory, selected forward paths, runner modes, file digests, seed references, and target-specific expectations. The same variant and profile produce identical Git commits on both majors. Fixture commits use fixed identities, timestamps, and SHA-1 object format for reproducibility.

G04 uses a depth-one clone whose candidate commit names the real, unavailable parent. Pass the base ID from `case.json` when testing the required-evidence gap. G03 retains an ignored `migrations/README.sql` in both revisions so the removal case exercises a missing identity with a configured migration root.

## Read the manifest

| Field | Meaning |
| --- | --- |
| `profiles`, `modes` | Plain SQL uses top-level filenames with positive integer prefixes and lowercase stems. It runs one `psql -X -v ON_ERROR_STOP=1 --file` per file and defaults to `--single-transaction`. The profile and any override describe the selected runner and boundary. SQLx opts out of its wrapper when the directive begins at byte zero. |
| `baselines`, `baseline_extra`, `write`, `remove` | Ordered pairs map repository-relative paths to shared SQL inputs. The baseline includes the extra files before the base commit. Candidate writes occur in the listed order. D01 writes version 10 before version 2. The Git tree captures the files and their original bytes. |
| `discovery` | Version lists are numeric and ordered. `forward` lists inventory versions, including duplicate identities. `selected` lists new or modified forwards for analysis. `down` describes inventory only. A null selection marks an unresolved discovery or Git evidence gap. |
| `git`, `applied_status` | Git compares the two explicit commits and reports unchanged, new, modified, and missing identities. Missing arrays are empty and a null classification marks unresolved identities or unavailable Git evidence. Applied status is unknown throughout the development labels because they supply Git evidence only. |
| `outcomes`, `policy` | Labels apply to both majors via `all` or are keyed by major. They specify required findings, targeted absences, coverage diagnostics, and expected `check` exit status. Additional findings must remain consistent with the expected gate. A fallback describes a blocking coverage gap for a capability outside the verified implementation subset. |
| `seeds` | The default recipe loads clean rows after version 1 and before any extra base step. Variants can also request schema-only, clean data plus one null email, or clean data plus one orphan event. Seed rows are outside the migration and Git inputs. |

The fixture policy requires complete coverage and a trusted Git base, denies HIGH and CRITICAL findings, and denies known modification or removal of a base identity. `check` returns 0 for pass, 1 for denial, 2 for invalid invocation or configuration, and 3 for required coverage or evidence incomplete. The reviewed designs exercise 0, 1, and 3. S02 retains its destructive finding while the later parse failure takes precedence with 3. A complete `scan` can succeed with blocking findings. [Guard plan](../../PLAN.md#10-risk-classification-and-policy)

| Cases | Matched inputs and controls |
| --- | --- |
| D01–D05 | Numeric order and a reversed dependency, colliding zero-padded prefixes, and matched, orphaned, and mismatched reversible files. |
| G01–G04 | Unchanged base plus append, comment-only and CRLF edits, deletion and version move, and an unavailable trusted base. |
| R01, R02 | Drop and truncate destructive forms and an existing-column rename, paired with the additive G01 control. |
| R03a, R03b | Nullable and constant-default adds paired with a volatile default. |
| R04a, R04b, R04c | SET NOT NULL with and without retained validated proof, and a separate CHECK validation with clean and violating seeds. |
| R05, R06, R07 | Regular index on an existing table with T02 and newly created relation controls, plus broad and primary-key-scoped updates. |
| T01–T04 | File wrapper and explicit transaction, autocommit and byte-zero opt-out, late opt-out, and SQLx implicit multi-statement transaction. |
| S01–S03 | Original reversible Up coordinates, a late parse failure after destructive SQL, and valid dynamic DDL outside the offline subset. |
| V01, V02 | Identical SQL across both majors for named deferred NOT NULL and a deferred foreign key on a partitioned table. |

## Byte and location contract

[SHA-256 digests](sha256.json) cover the original bytes of every input. Scoped Git attributes preserve SQL line endings. The validator recreates both committed snapshots with `core.autocrlf=true`, restores each SQL checkout from the index, compares file bytes to Git blobs, verifies the raw Git change list, and checks discovery and expected gate metadata for each variant and profile.

Both S01 Up variants use `migrations/002_spans.up.sql`, UTF-8 without BOM and a final newline. The semicolon between `é` and `🧪` belongs to the dollar-quoted literal. Guard should produce two forward statements and locate PG001 at the original `DROP COLUMN legacy_code` subcommand. Byte ranges are zero-based and half-open. Proposed display positions are one-based with Unicode scalar columns.

| S01 variant | Second `ALTER TABLE` byte start | `DROP COLUMN legacy_code` byte range | Display starts |
| --- | --- | --- | --- |
| LF | 87 | `[139, 162)` | 2:66 and 4:3 |
| CRLF | 88 | `[142, 165)` | 2:66 and 4:3 |

## Milestone handoff

| Milestone | Remaining use of the corpus |
| --- | --- |
| DMG-04 discovery and integrity | Verify adapter behavior against the inventory and Git pairs. Confirm D03 collision handling, D05 orphan and mismatched-pair diagnostics, and the policy for a byte-identical path move that keeps the same version. G03 covers a move to a different version. |
| DMG-05 parsing and coverage | Verify S01's two normalized statements and mapped spans after directive handling, choose the report column unit, retain S02's first finding and parse location, and report S03's unsupported body. Confirm the analyzer subset for V01/V02 and use the explicit incomplete fallbacks if necessary. |
| DMG-06 rules and runtime | Check expected Guard findings and T04 transaction modeling against native evidence. Use the recorded two-session lock observations for R04, R05, and V02. Extend runner coverage to R01, R02, R04a, R04b, R05, R06, and R07, including an R02 client selecting the old column. Run the independent holdout gate. |
| DMG-07 policy | Exercise the fixture policy, including incomplete precedence and passing warnings. Add separate invalid configuration and invocation checks. |
| DMG-08 pilot | Pin available Squawk and Atlas binaries and editions, verify comparable input selection and execution, and run the [pilot protocol](pilot.md). |
| DMG-10 history | Hold the G01 Git pair fixed and vary authorized history between base-only, base plus candidate, and unavailable. Reuse G02/G03 with history captured from the original base to show applied modification and missing-version cases. Verify SQLx's mismatch, missing-version, and dirty paths in separate disposable databases. Plain manifests use a Guard byte digest and SQLx history uses the runner's native SHA-384. |
| DMG-11 and DMG-12 | Add declared application contract checks and Guard rehearsal. Exercise clean and violating seeds, a deliberate late failure under plain autocommit and file transactions, and Down rehearsal on a fresh disposable forward state. Bind replay evidence to the revision, seed, runner, and pinned server image. |

The history rows in the native observations describe disposable probe databases. The offline Git fixtures use the original-byte SHA-256 and the trusted Git base. The [SQLx migration type](https://github.com/transact-rs/sqlx/blob/v0.8.6/sqlx-core/src/migrate/migration.rs) defines the runner checksum.
