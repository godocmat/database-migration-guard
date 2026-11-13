# Migration discovery

Database Migration Guard inventories the immediate children of explicit repository-relative migration roots. Each scan uses one adapter and one version sequence across the roots. Configure nested directories as their own roots. Discovery checks for portable paths and reads ordinary files through directory handles. It rejects migration symlinks and non-regular candidates.

| Adapter | Filename and runner contract |
| --- | --- |
| Plain SQL | Supply a full filename regular expression with one capture for the positive integer version, for example `^([0-9]+)_[a-z][a-z0-9_]*\.sql$`. Leading zeros have no effect on identity or order. Declare a file transaction or statement-by-statement autocommit. Explicit SQL transaction commands remain available to analysis. |
| SQLx 0.8.6 | Declare a simple or reversible layout. The runner uses a signed 64-bit integer prefix before the first underscore and case-sensitive SQL suffixes. Reversible inventory requires one Up and one Down at each version with matching stems in the same directory. Down files retain their bytes and integrity records for review. Forward selection contains Up files. The byte-zero `-- no-transaction` prefix disables the runner's file wrapper. SQLx still sends the full file in a single query, which the analyzer uses to determine implicit PostgreSQL transaction boundaries. [SQLx directory loader](https://raw.githubusercontent.com/launchbadge/sqlx/v0.8.6/sqlx-core/src/migrate/source.rs) |
| Flyway OSS 13.12.2 | Discover versioned and repeatable files in runner order and inventory undo separately. Declare the verified JDBC transaction settings. Recognized sidecars, callbacks, and unresolved placeholders produce incomplete coverage. [Flyway migration scope](flyway-migrations.md) |
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

Use the actual file digest when creating a manifest. A reversible SQLx manifest requires both directions with matching names. Flyway versioned keys use `{"kind":"flyway","value":"1.2"}`, and repeatables use `{"kind":"repeatable","value":"refresh summary"}`. Supabase versions use `{"kind":"supabase","value":"01"}` and preserve their exact digits. When the scan is already incomplete, validating the manifest can succeed while the plan still has unresolved selection and no new integrity comparison.

An applied-history manifest is a separate, complete inventory of authorized forward history. Use `reconcile_applied_manifest` with its original bytes and trusted digest. Each entry contains the version, SHA-256 of the forward file, and an `applied` or `failed` status. An empty validated history establishes pending status for local forwards. Without a history manifest, applied status is unknown. The manifest uses Guard's byte digest. Native SQLx checksum and database history parity belong to DMG-10.

## Verification

Run the focused Rust tests and the corpus driver from the workspace root. The corpus driver needs Python 3 and Git and uses the existing fixture materializer. Set `DMG_FIXTURE_PYTHON` if the interpreter has a different executable name.

```text
cargo test --locked -p dmg-discovery
cargo test --locked -p dmg-discovery --test corpus -- --ignored --nocapture
```

The corpus driver exercises both worktree and exact-commit discovery on the 24 distinct D01–D05, G01–G04, S01, and T04 recipes. The recipes use the same discovery bytes for PostgreSQL 17 and 18. The test checks file order, original-byte digests, source slices, transaction delivery, coverage, and forward selection against the recorded metadata. [Fixture corpus](../fixtures/dmg-02/README.md)
