# Local migration lint pilot

On 13 November 2026, the local pilot ran seven public plain SQL variants on PostgreSQL 17.12 and 18.7. The [probe](../competitor_probe.py) and [JSON Lines observations](competitor-pilot.jsonl) retain the two Git commit IDs, baseline and candidate paths and SHA-256 digests, expected risks, actual exits, tool diagnostics and native coordinates, and replay observations. The same bytes and result pattern held for both majors. Every pair has one baseline file and one new forward file with equal-width numeric prefixes.

## Pins and conditions

| Tool | Tested edition and build | Binary SHA-256 |
| --- | --- | --- |
| Guard | CLI 0.1.0 at `6215f6b582e50b65e876c8d0c5d67010fc805db3`, Linux x86-64, report schema and ruleset 1 | `7621fa05227b125cf9ba79cb2c98a1881445d0b654860c04840257bf29bd9295` |
| Squawk | Standalone CLI 2.72.0, published Linux x86-64 musl asset | `b4cda2069a3ca5b75b47f025b569df9e6a762262b9258bc4db3f43381f8544d8` |
| Atlas | Community v1.3.0, published Linux amd64 binary | `10d7913e3dce43ab99b8d71534a4cbadaf11a16dc293adf3b91d10e83a0ac70b` |

Squawk's GitHub release asset metadata and Atlas's published SHA-256 matched the downloaded binaries. The pinned Atlas Community binary ran `migrate lint` without an account in the isolated environment and returned exit 1 with an unsupported-command message for `migrate test`. The Community edition matrix and the v0.38 release note give different descriptions of lint availability. The observed results apply to the exact binary above. The official standard distribution's lint documentation specifies Atlas Pro and `atlas login`. [Squawk release](https://github.com/sbdchd/squawk/releases/tag/v2.72.0) · [Squawk asset metadata](https://api.github.com/repos/sbdchd/squawk/releases/assets/722956776) · [Atlas binary](https://atlasbinaries.com/atlas/atlas-community-linux-amd64-v1.3.0) · [Atlas checksum](https://atlasbinaries.com/atlas/atlas-community-linux-amd64-v1.3.0.sha256) · [Atlas Community edition matrix](https://atlasgo.io/community-edition) · [Atlas v0.38 release note](https://atlasgo.io/blog/2025/10/28/v038-analyzers-pii-and-migration-hooks#change-in-v038-atlas-migrate-lint) · [Standard Atlas lint](https://atlasgo.io/versioned/lint)

The host was x86-64 WSL2 with Ubuntu 24.04.4, Git 2.43.0, and Python 3.12.3 in a disposable chroot. Commands ran as a regular user with an empty home in isolated network and IPC namespaces. The probe used source-built PostgreSQL and matching `psql` in place of container images. The binary digests are in the JSON Lines records. The published source archive SHA-256 values are `9cf136a703d8ee2f83465c43be4e9e8e7930c82b0bfba13be7e2537be769ac55` for 17.12 and `441dfae4280cae3e0425e5f876f80559fb0731c140f1aae22b3e5a54d0411bfd` for 18.7. [PostgreSQL 17.12 source](https://www.postgresql.org/ftp/source/v17.12/) · [PostgreSQL 18.7 source](https://www.postgresql.org/ftp/source/v18.7/)

- Guard analyzed the trusted Git pair with `transaction = "file"`, `deny_at = "high"`, and required Git base coverage. The reported migration inventory, source digests, and selection agreed with the materialized fixture.
- Squawk used an explicit empty configuration, `--pg-version` set to the exact target minor, `--assume-in-transaction`, and the JSON reporter. The harness selected the candidate paths. Squawk performed static analysis of those files.
- Atlas replayed the baseline SQL and candidate in a fresh, schema-empty dev database over a private Unix socket. The probe used the Atlas directory format and generated `atlas.sum` from the exact candidate directory. This checksum verifies the directory used for lint. The `--git-base pilot-base` and `--latest 1` runs agreed on selected files, bytes, diagnostics, and exit. Server logs confirmed the order of the four baseline statements before the candidate.
- Atlas lint submitted each statement on its own. On both servers its R07 index statement started with no transaction ID from the preceding table creation. The `psql --single-transaction` control logged `BEGIN` and `COMMIT` and started the index statement with an already assigned transaction ID. R07 depends on the table being new within the same transaction, so the Atlas R07 result is outside that comparison. The other six candidates each contain one statement. The Atlas observations describe lint replay only. [Pinned Atlas replay implementation](https://github.com/ariga/atlas/blob/v1.3.0/cmd/atlas/internal/migratelint/lint.go#L210)

## Observations

The numbers in the table are native exits. Coordinates appear as one-based line and column positions. The observations retain the original coordinates, including Squawk's zero-based spans, Guard's one-based spans, and Atlas's statement byte offset. Each diagnostic path is the selected candidate in the corresponding evidence record.

| Public variant | Reviewed risk | Guard | Squawk default | Atlas Community default |
| --- | --- | --- | --- | --- |
| R01 drop column | Loss of stored column values | `1` · PG001 critical, 1:26 | `1` · `ban-drop-column` client compatibility warning, 1:26, plus timeout warnings | `1` · DS103 destructive column drop, byte 0 of statement at 1:1 |
| R01 truncate | Loss of table rows | `1` · PG001 critical, 1:1 | `1` · timeout warnings at 1:1 | `0` · no diagnostics |
| R03a nullable add | Control for heap rewrite | `0` · no rewrite finding | `1` · timeout warnings at 1:1 | `0` · no diagnostics |
| R03a constant default | Control for heap rewrite | `0` · no rewrite finding | `1` · timeout warnings at 1:1 | `0` · no diagnostics |
| R03b volatile default | Heap rewrite on `app.accounts` | `1` · PG003 high, 1:26 | `1` · `adding-field-with-default` at the expression, 1:69, plus timeout warnings | `0` · no diagnostics |
| R05 regular index | Writer blocking on existing `app.accounts` | `1` · PG005 high, 1:1 | `1` · `require-concurrent-index-creation` at 1:1, plus timeout warnings | `0` · no diagnostics, `concurrent_index` absent from the pinned Community analyzer registry |
| R07 new relation | Control for existing-relation index blocking within one file transaction | `0` · no existing-relation finding | `1` · timeout warnings on the index at 2:1 | `0` · no diagnostics under statement replay |

For each major, Guard matched four reviewed risks and produced no targeted finding on the three controls. Squawk matched two risks directly, the rewrite and the index lock. Its drop-column finding identified the same operation and column with a different impact, client compatibility. Its truncate findings addressed timeout settings. Its two timeout requirements caused all three controls to fail by default. Guard still reported `ACCESS EXCLUSIVE` for both add-column controls and `SHARE` on the new relation in R07. [Squawk drop rule](https://squawkhq.com/docs/ban-drop-column) · [Squawk rewrite rule](https://squawkhq.com/docs/adding-field-with-default) · [Squawk index rule](https://squawkhq.com/docs/require-concurrent-index-creation) · [Native rewrite and lock observations](README.md)

The focused Squawk pass excluded `require-lock-timeout` and `require-statement-timeout`. Drop column, volatile default, and the existing-table index still exited 1. Truncate and the three controls exited 0. The Atlas R05 configuration probe set `lint.concurrent_index.error = true` with `--env pilot`. It still returned 0 with no diagnostics on either major. The pinned Community PostgreSQL analyzer registry omits `concurrent_index` and `blocking`. The standard Atlas documentation lists them as Pro analyzers. A matched three-tool score would require the supported Atlas rules and a runner-compatible setup. [Pinned Community PostgreSQL analyzers](https://github.com/ariga/atlas/blob/v1.3.0/sql/postgres/postgrescheck/postgrescheck.go) · [Standard Atlas analyzers](https://atlasgo.io/lint/analyzers#concurrent-index-policy-postgresql)

The local probe timed process startup and each lint invocation on a single host with warmed executables and fixed tool order. Setup, fixture generation, database startup, and hosted CI are outside these timings. Atlas lint timing includes replay and introspection on a local server. See `elapsed_ms` in the observations for individual durations.

## Reproduce

Use a Unix x86-64 environment with the pinned binaries and Git on `PATH`. Download the two published assets from the sources above and verify their SHA-256 values before running them. Build Guard from the recorded revision with `cargo build --locked --release -p dmg-cli` or use the exact pilot binary. Use the matching `bin` directories from PostgreSQL 17.12 and 18.7. From the repository root, run as a regular user with a writable temporary directory. The script creates and removes its own socket-only clusters and prints JSON Lines to standard output.

```sh
python3 -B fixtures/dmg-02/competitor_probe.py \
  --dmg /path/to/dmg --squawk /path/to/squawk --atlas /path/to/atlas \
  --pg17-bin /path/to/postgresql-17.12/bin --pg18-bin /path/to/postgresql-18.7/bin
```

Hosted PR checks and exception handling, edited or removed history, SQLx reversible selection, the seeded Atlas rehearsal, and independent reviewer time remain open in the [pilot protocol](../pilot.md). No CI action revision or image digest applies to this local source-built run.
