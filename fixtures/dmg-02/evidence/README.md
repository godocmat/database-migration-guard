# DMG-02 verification

## Structural checks

On Windows, `python -B fixtures/dmg-02/corpus.py validate` passed with Python 3.11.6 and Git for Windows 2.45.1. It checked 28 cases, 38 variants, 65 reproducible Git pairs, 130 variant/profile/major combinations, and 39 SQL input digests.

- Both snapshots were restored from their Git indexes with `core.autocrlf=true`. The validator matched the original-byte inputs, committed blobs, and restored checkouts. It checked the raw Git change list, filename identities and directions, numeric forward order, selected versions, ignored files, duplicate forwards, reversible pairs, and case references. In G04, each shallow candidate names the trusted parent and `git cat-file -e` fails to resolve the parent's commit object.
- S01 uses a 164-byte LF Up and a 168-byte CRLF Up with four line endings each. The validator checked the original Up path, UTF-8 slices at the second statement and destructive subcommand, and display starts 2:66 and 4:3 for both versions. It also checked that normalizing CRLF yields the LF bytes.
- The validator cross-checked the declared policy outcomes against the required findings, integrity state, and coverage. Guard discovery, parser, rule, and policy execution are milestone checks described in the [handoff](../README.md#milestone-handoff).

## Native runner observations

On 12 November 2026, the [native probe](../native_probe.py) completed 41 fresh databases on each PostgreSQL major. T02 and T04 ran with schema only and with clean synthetic rows. The probe created a new Unix-socket-only cluster per major, used a separate database per variant/profile/seed, and removed the cluster after testing. The host was x86-64 Ubuntu 20.04 on WSL2 with Python 3.8.10. Matching PostgreSQL servers and `psql` were built from the upstream source archives with Zig 0.14.1 targeting glibc 2.31, using `--without-icu --without-readline --without-zlib --without-llvm`. SQLx CLI and library 0.8.6 were built with Rust 1.90.0 using `cargo install sqlx-cli --version 0.8.6 --locked --no-default-features --features postgres --bin sqlx --debug`. [PostgreSQL 17.12 source](https://www.postgresql.org/ftp/source/v17.12/) · [PostgreSQL 18.7 source](https://www.postgresql.org/ftp/source/v18.7/) · [SQLx CLI source](https://github.com/transact-rs/sqlx/tree/v0.8.6/sqlx-cli)

| Server | Verified source archive SHA-256 | Observations |
| --- | --- | --- |
| 17.12 | `9cf136a703d8ee2f83465c43be4e9e8e7930c82b0bfba13be7e2537be769ac55` | [41 native runs](native-17.12.jsonl) |
| 18.7 | `441dfae4280cae3e0425e5f876f80559fb0731c140f1aae22b3e5a54d0411bfd` | [41 native runs](native-18.7.jsonl) |

The JSON Lines files record runner and server versions, binary and fixture metadata digests, native exits, available server errors and positions, order, resulting schema, and SQLx history. SQLSTATE comes from the server log. The psql error text includes its file and line where available. Native process exits and history describe the probe databases. The development manifest specifies the planned Guard gate.

| Probe | Observed on both servers unless stated |
| --- | --- |
| D01, D02, D04 | D01 applied 2 then 10 and left the sentinel from the ignored inputs absent. D02 failed at the early ALTER with `42P01`. The reversible D04 kept the column added by its Up. |
| R03a, R03b | The nullable and constant-default adds kept the heap file identity. The volatile default changed it. |
| R04c | Validation preserved the three clean rows and set `convalidated` to true. The four-row seed with a null returned `23514` and left the CHECK unvalidated. |
| T01, T03 | The file wrappers, explicit plain transaction, and late SQLx directive produced `25001` and no index. |
| T02 | Plain autocommit and both SQLx byte-zero single-statement controls produced a valid concurrent index. The SQLx history included successful versions 1 and 2. |
| T04 | The server log contained the whole opt-out batch. SQLx exited 1 and the server returned `25001`. The earlier `code_copy` add rolled back, the index was absent, and history contained only version 1. |
| S01, S02, S03 | Both S01 Up variants retained the `é;🧪` default value on the seeded rows, added `label`, and dropped `legacy_code`. SQLx stored distinct SHA-384 checksums for the two Up files. S02 applied the first candidate before the later file returned `42601`. S03's dynamic SQL created the label column. |
| V01, V02 | PostgreSQL 17 rejected V01 with `42601` and V02 with `42809`. PostgreSQL 18 added both constraints unvalidated with clean and violating seeds. New violations returned `23502` and `23503`, respectively. Validation succeeded for clean seeds, failed with the same violation codes on the bad seeds, and succeeded after the violating rows were repaired. |

Run the probe from the repository root on a Unix host as a regular user with the pinned binaries. Use `--case T02 --case T04` for the focused SQLx comparison. The script verifies the SQL input digest inventory and stages the selected recipes into its disposable cluster directory. It prints observations to stdout after cleanup.

```sh
python3 -B fixtures/dmg-02/native_probe.py --pg-bin /path/to/postgresql-17.12/bin --sqlx /path/to/sqlx
python3 -B fixtures/dmg-02/native_probe.py --pg-bin /path/to/postgresql-18.7/bin --sqlx /path/to/sqlx
```

## Two-session relation locks

On 13 November 2026, the [lock probe](../lock_probe.py) ran five clean-seed cases per major in fresh PostgreSQL databases. It used Linux amd64 PostgreSQL and psql binaries from the Docker Official Images listed below. The image filesystems ran in separate WSL2 chroots with isolated network and IPC namespaces, a private Unix socket directory, and Alpine Python 3.14.10. The probe holds the candidate DDL open in one transaction and observes its granted locks from a second connection. The second session also attempts an INSERT with a 500 ms lock timeout. Successful writes roll back. [PostgreSQL 17 locking](https://www.postgresql.org/docs/17/explicit-locking.html) · [PostgreSQL 18 locking](https://www.postgresql.org/docs/18/explicit-locking.html) · [Docker Official PostgreSQL image](https://hub.docker.com/_/postgres)

| Image | Linux amd64 manifest digest | Observations |
| --- | --- | --- |
| postgres:17.12-alpine | sha256:005c5c37db96c1e6652a79ea31f9886402fba81ac95dc2db897739121dd7f8f2 | [Five runs](locks-17.12.jsonl) |
| postgres:18.7-alpine | sha256:fa1b61f5a4844f4d4b73c9b15716cfc75fe70c931f2d32723630f3b900056366 | [Five runs](locks-18.7.jsonl) |

| Case | Relation locks and concurrent write |
| --- | --- |
| R04a and R04b | Both SET NOT NULL variants held ACCESS EXCLUSIVE on app.accounts and the concurrent write returned 55P03. This probe measures the locks held after the DDL completes. The retained CHECK can avoid the scan while keeping the same lock mode. |
| R04c | CHECK validation held SHARE UPDATE EXCLUSIVE on app.accounts. The concurrent write succeeded. |
| R05 | The regular index build held SHARE on app.accounts and the concurrent write returned 55P03. |
| V02 | PostgreSQL 17 rejected the DDL with 42809. PostgreSQL 18 held SHARE ROW EXCLUSIVE on app.accounts, app.events, and its child. Inserts into either app.accounts or app.events returned 55P03. |

From the repository root on a Unix host, run the probe as a regular user with the pinned PostgreSQL binaries. The script prints JSON Lines, including binary and fixture digests, after removing the temporary cluster. For an image run, use the recorded amd64 digest, add Python 3 to the disposable Alpine environment, and run as the postgres user with a writable /tmp.

```sh
python3 -B fixtures/dmg-02/lock_probe.py --pg-bin /path/to/postgresql-17.12/bin
python3 -B fixtures/dmg-02/lock_probe.py --pg-bin /path/to/postgresql-18.7/bin
```

Remaining runtime work includes the other rule cases listed in the handoff, broader schema-only and image-pinned matrix coverage, and the later applied-history and Guard rehearsal cases. Independent holdout selection, labeling, freeze, and competitor comparison remain pending.
