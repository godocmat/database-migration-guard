# Migration discovery

Database Migration Guard inventories the immediate children of explicit repository-relative migration roots. Each scan uses one adapter and one version sequence across the roots. Configure nested directories as their own roots. Discovery checks for portable paths and reads ordinary files through directory handles. It rejects migration symlinks and non-regular candidates.

| Adapter | Filename and runner contract |
| --- | --- |
| Plain SQL | Supply a full filename regular expression with one capture for the positive integer version, for example `^([0-9]+)_[a-z][a-z0-9_]*\.sql$`. Leading zeros have no effect on identity or order. Declare a file transaction or statement-by-statement autocommit. Explicit SQL transaction commands remain available to analysis. |
| SQLx 0.8.6 | Declare a simple or reversible layout. The runner uses a signed 64-bit integer prefix before the first underscore and case-sensitive SQL suffixes. Reversible inventory requires one Up and one Down at each version with matching stems in the same directory. Down files retain their bytes and integrity records for review. Forward selection contains Up files. The byte-zero `-- no-transaction` prefix disables the runner's file wrapper. SQLx still sends the full file in a single query, which the analyzer uses to determine implicit PostgreSQL transaction boundaries. [SQLx directory loader](https://raw.githubusercontent.com/launchbadge/sqlx/v0.8.6/sqlx-core/src/migrate/source.rs) |
| Flyway OSS 13.12.2 | Discover versioned and repeatable files in runner order and inventory undo separately. Declare the verified JDBC transaction settings. Recognized sidecars, callbacks, and unresolved placeholders produce incomplete coverage. [Flyway migration scope](flyway-migrations.md) |
| Goose 3.30.1 | Use one root. Guard preserves combined Up and Down ranges, query grouping, and the file-level transaction opt-out. Environment substitution and custom Go migrations produce incomplete coverage. [Goose migration scope](goose-migrations.md) |
| Supabase CLI 3.0.0 | Use one root and exact lexical version strings. Guard models supported extended-query batches, standalone concurrent index operations, and explicit transaction behavior. [Supabase migration scope](supabase-migrations.md) |

The inventory is sorted by the adapter's version or repeatable order, then direction and repository-relative path. Duplicate identities, unpaired SQLx migrations, unavailable inputs, and limits produce incomplete coverage. The default limits are 20,000 immediate directory entries across the configured roots, 8 MiB per file, and 128 MiB of SQL per snapshot. Discovery retains original-byte segments and full-file SHA-256 digests. A consumer can read every available forward source to establish schema context and use the selected paths to decide which findings to report.

## Worktree and Git inputs

Use `discover_worktree` for a local filesystem scan. It selects every forward file when discovery is complete. Use `discover_git` with two exact local commit IDs to compare a trusted base with a candidate. Git for Windows 2.45.1 was verified. Git reads the original blobs without checkout filters or replacement objects. The reader disables lazy fetching and transport protocols, caps Git output and migration bytes, and uses a 30-second deadline for each Git command and each revision's batch blob reader. The Git repository and its object store are local inputs. [Git object reading](https://git-scm.com/docs/git-cat-file) · [Git local options](https://git-scm.com/docs/git)

Changed forward files are selected in version order. A byte-identical move within the same version retains unchanged integrity and records both paths. The moved forward remains selected. A move to a different version produces a missing old identity and a new identity. Changes to a reversible Down file appear in the integrity comparison. Missing Git evidence leaves selection unresolved and the available candidate inventory remains bound to the explicit head commit. The caller supplies trusted commit IDs and any required ancestry policy.

## Trusted manifests

A caller can compare a complete discovery result to a pinned integrity manifest with `compare_integrity_manifest`. Provide the original manifest bytes and a SHA-256 digest obtained from a trusted source. The manifest is versioned JSON. Each entry contains the version, direction, repository-relative path, and SHA-256 of the original migration bytes.

```json
{
  "schema_version": 1,
  "adapter": "plain",
  "migrations": [
    {
      "version": 1,
      "direction": "forward",
      "path": "migrations/1_baseline.sql",
      "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
    }
  ]
}
```

Use the actual file digest when creating a manifest. A reversible SQLx manifest requires both directions with matching names. A Goose Down entry shares the identity, path, and full-file digest of its Forward entry. Flyway versioned keys use `{"kind":"flyway","value":"1.2"}`, and repeatables use `{"kind":"repeatable","value":"refresh summary"}`. Supabase versions use `{"kind":"supabase","value":"01"}` and preserve their exact digits. When the scan is already incomplete, validating the manifest can succeed while the plan still has unresolved selection and no new integrity comparison.

An applied-history manifest is a separate, complete inventory of authorized forward history. Use `reconcile_applied_manifest` with its original bytes and trusted digest. Each entry contains the version, SHA-256 of the forward file, and an `applied` or `failed` status. An empty validated history establishes pending status for local forwards. Plain SQL uses this manifest for applied history. With no authorized history, applied status is unknown.

## Native applied history

`reconcile_native_history` compares an authorized `NativeHistoryEvidenceV1` with the candidate and expected target ID. The evidence must match the adapter and canonical forward inventory, including each forward identity, path, and raw SHA-256. Supply evidence from the designated history table and the effective runner profile. The API returns applied statuses and coverage diagnostics for the caller to use with the discovery result. [Inspection evidence contract](inspection-evidence.md#native-migration-history)

| Runner | Evidence used for reconciliation |
| --- | --- |
| SQLx 0.8.6 | Compare SHA-384 over the original UTF-8 file, including any BOM, comments, and line endings. A recorded `success = false` row establishes a failure for that version. [Checksum source](https://github.com/transact-rs/sqlx/blob/v0.8.6/sqlx-core/src/migrate/migration.rs) · [Native LF and CRLF observations](../fixtures/dmg-02/evidence/README.md) |
| Flyway 13.12.2 | Compare signed CRC32 over a single UTF-8 SQL file with one initial BOM and CR and LF line terminators removed. The verified profile uses untemplated files and default checksum settings. Compare the highest installed rank per version or repeatable description. A later successful undo clears the active forward record for that version. A latest failed event is reported as failed. The OSS probe verifies forward history, while tests exercise normalized undo records. Unrecognized or superseded latest events produce unknown status. [Checksum source](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resolver/ChecksumCalculator.java) · [Resolver source](https://github.com/flyway/flyway/blob/flyway-13.12.2/flyway-core/src/main/java/org/flywaydb/core/internal/resolver/sql/SqlMigrationResolver.java) · [Native repeatable history](../fixtures/dmg-09/flyway/README.md#discovery-and-history) |
| Goose 3.30.1 and Supabase CLI 3.0.0 | Compare presence using the runner's native identity. An applied result carries the presence-only basis and leaves content integrity unknown. [Goose history](../fixtures/dmg-09/goose/README.md#discovery-and-history) · [Supabase history](../fixtures/dmg-09/supabase/README.md#discovery-and-history) |

With complete history and candidate coverage, a matching native checksum gives applied status and a mismatch gives modified status. A repeatable whose checksum changed is modified in this comparison and may run again. An active applied row without a candidate forward file is missing. A local forward file without an active history row is pending. Pending describes the recorded history only. The native probes show that a failed attempt can leave schema changes without a history row. [Flyway partial-state observations](../fixtures/dmg-09/flyway/README.md#execution-and-partial-state)

If row coverage or head discovery is incomplete, statuses are generally unknown. An observed SQLx dirty row establishes failure even when other versions were truncated. In a ranked history, a missing later event could change the latest observed state. A checksum gap can leave row coverage complete while the affected checksum comparison remains unknown.

## Verification

Run the focused Rust tests and the corpus driver from the workspace root. The corpus driver needs Python 3 and Git and uses the existing fixture materializer. Set `DMG_FIXTURE_PYTHON` if the interpreter has a different executable name.

```text
cargo test --locked -p dmg-discovery
cargo test --locked -p dmg-discovery --test corpus -- --ignored --nocapture
```

The corpus driver exercises both worktree and exact-commit discovery on the 24 distinct D01–D05, G01–G04, S01, and T04 recipes. The recipes use the same discovery bytes for PostgreSQL 17 and 18. The test checks file order, original-byte digests, source slices, transaction delivery, coverage, and forward selection against the recorded metadata. [Fixture corpus](../fixtures/dmg-02/README.md)
