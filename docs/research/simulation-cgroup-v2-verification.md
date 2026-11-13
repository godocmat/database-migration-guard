# PostgreSQL rehearsal on Linux cgroup v2

On 2026-11-13, the Linux x86-64 CLI and `dmg-simulate` integration tests from `8edf66f6d7298c5631a8cc5708699962579c4247` ran in a disposable Ubuntu 24.04.5 guest with kernel `6.8.0-147-generic`, two vCPUs, and 4 GiB of RAM. QEMU 11.1.0 used the Windows Hypervisor Platform without a virtual network adapter. The guest verified that loopback was its only network interface. Each PostgreSQL release used a fresh Docker Engine 29.9.3 daemon with `vfs`, the systemd cgroup v2 driver, and built-in seccomp. The daemons ran in private mount, network, and IPC namespaces and loaded the [approved images](simulation-worker.md#images-and-resources) from local archives.

| Artifact | SHA-256 |
| --- | --- |
| Linux x86-64 debug CLI | `848a3fcc97f82545661aa87aa2e563a24a898e46b5f46d3863350249cf27c053` |
| Linux x86-64 integration test binary | `014a858f422b7f1daa01626e27b93e703fc764e7f2243a30c5625257529836cd` |
| Tested source archive | `32826090ef1ca2928382af0cd227596ca603bf854aa21a6ff9596ccb3f32bed3` |
| Ubuntu Server AMD64 cloud image VMDK | `9a1e3959c9f4ad9d4752101fd4ff9a43191fb7d72e61fa20e033f9fd235ce794` |
| Docker Engine Linux x86-64 archive | `3fb5d1f13014c18cc54945bc177c58a9056a482dada031e17d12cf2e6922c973` |
| Guest serial log | `c612468d06b5f9d2166e3436293c0d54c051ffd24f5286f5ec3d409871998092` |

The Windows host cross-built the CLI and test binary with the locked Cargo dependencies, Rust 1.100.0, and the glibc 2.39 target. The guest confirmed both executable digests before running them. The same CLI matched all 130 public fixture exits, with 48 passes, 58 denials, and 24 incomplete checks. The local GitHub and ten-case GitLab replays also passed in the guest.

## Worker test scope

The lifecycle test checks that the worker verifies its effective CPU, memory, swap, process, filesystem, network, and process privilege settings before bootstrap. It checks restricted authentication and exact worker removal after successful execution, an operation error, a wall deadline, a bootstrap error, an unavailable image, and dropping the handle while the runtime remains active. The deadline fixture separately checks PostgreSQL SQLSTATE `57014` and a client wall timeout during the before-migration observation.

The nine rehearsal cases per release cover Plain SQL file transactions and autocommit, explicit transaction and savepoint rollback, SQLx single-query opt-out behavior, Goose statement groups, Flyway rollback, Supabase batch rollback and concurrent-index separation, and a successful enforced `NOT VALID` constraint. Assertions cover database errors and source spans, seeded row counts, durable changes after partial failure, before and after catalog evidence, cleanup status, and omission of SQL, database error text, and seed values. Catalog-diff assertions match the exact qualified object and owning table. Columns and constraints are identified by the owning table's name and a separate member name. Indexes have their own qualified name.

| PostgreSQL | Worker lifecycle | Server and wall deadlines | Nine rehearsal cases |
| --- | ---: | ---: | ---: |
| 15.20 | Pass, 23.51 s | Pass, 12.63 s | Pass, 57.64 s |
| 16.16 | Pass, 16.68 s | Pass, 6.22 s | Pass, 38.35 s |
| 17.12 | Pass, 14.90 s | Pass, 5.63 s | Pass, 28.72 s |
| 18.7 | Pass, 20.72 s | Pass, 6.70 s | Pass, 35.53 s |

Every daemon had zero containers after each test group. The durations include worker startup and teardown under guest load.

To rerun against a disposable Linux x86-64 daemon with a preloaded approved image, set `DMG_WORKER_TEST_DOCKER` to the absolute Docker executable, `DMG_WORKER_TEST_SOCKET` to the Unix socket path, and `DMG_WORKER_TEST_POSTGRES` to one of the four tested releases. Repeat for each release with an empty container inventory.

```sh
cargo test --locked --offline -p dmg-simulate linux::tests::isolated_worker_lifecycle -- --ignored --exact
cargo test --locked --offline -p dmg-simulate isolated_rehearsal -- --ignored --test-threads=1
docker container ls -aq
```

## CLI workflow

The PostgreSQL 18.7 CLI fixture used a protected baseline, one synthetic seed row, and separate exact Git revisions for a successful column addition and a duplicate-key failure. An independently generated `dmg simulate --plan-only` binding matched each saved report. Both runs recorded complete before and after observations and schema diffs. The successful addition was committed. The duplicate-key error returned SQLSTATE `23505` and rolled back its preceding addition.

| Rehearsal | `simulate` exit | `check` exit with authenticated report | `check` exit without report |
| --- | ---: | ---: | ---: |
| Successful addition | 0 | 0 | 3 |
| Duplicate-key failure | 3 | 1 | 3 |

Each check verified the original rehearsal bytes and recorded the matching outcome and SHA-256 in its policy audit. Both rehearsal reports rendered to HTML and Markdown without changing the source bytes or displaying seed values, SQL, or the Docker socket. The daemon reported zero containers after the CLI checks. The guest recorded `DMG_MATRIX_STATUS=0` and powered off cleanly.
