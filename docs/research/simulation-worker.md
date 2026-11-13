# Disposable PostgreSQL worker

`dmg-simulate` provides an explicit Linux x86-64 Docker worker API. Supply an absolute trusted Docker executable and local Unix socket from the protected caller. The worker uses a private empty Docker config and clears the subprocess environment. The daemon administrator controls the isolation boundary. The worker requires the built-in seccomp profile and supported cgroup CPU, memory, swap and process limits. [Docker seccomp](https://docs.docker.com/engine/security/seccomp/) · [Docker container creation](https://docs.docker.com/reference/cli/docker/container/create/)

## Images and resources

Load an approved PostgreSQL Docker Official Image into the disposable daemon ahead of time. Each linux/amd64 manifest identifies the configuration by digest. The worker verifies the locally loaded image by its full immutable config ID and compares the connected server's numeric version with the pin. Manifest digests identify the approved registry inputs and config IDs also work with an offline archive that has no repository digest alias. [PostgreSQL image sources](https://github.com/docker-library/postgres)

| PostgreSQL | Registry manifest SHA-256 | Docker config SHA-256 |
| --- | --- | --- |
| 15.20 | `675e329d71b3d00534e50356ad068fa95d7bdd9c68f498f17a016ae19990d85e` | `24084862c6bcb8a9b348892f7135c1099cb581dbca4d046aec95734717714b18` |
| 16.16 | `a2f7e20374cd8fd8ed4a588b79549e10589bea7b1686e77922f5ac741b80b8a1` | `3cbd775daa28461ef484ed88ca1e5a5a72ff126e196deeebce6fec764edf1200` |
| 17.12 | `005c5c37db96c1e6652a79ea31f9886402fba81ac95dc2db897739121dd7f8f2` | `6b75136145ddabf3d9a4b69660402082de7c56b734cfe50555b0674774dd742a` |
| 18.7 | `fa1b61f5a4844f4d4b73c9b15716cfc75fe70c931f2d32723630f3b900056366` | `0f6c51e48b3458e87ab43692e49c9dafc72ffe0a0dbde3fd8cdb6ab92d5c025e` |

The worker verifies Docker configuration before starting the container and checks the effective namespace and limits before initializing the migration role. It uses a read-only image, UID/GID 70, zero capabilities, no new privileges, built-in seccomp, Docker's `none` network, and no host mounts or published ports. Memory and combined memory plus swap are capped at 192 MiB, CPU is capped at one logical core, and process count is capped at 36. Writable tmpfs areas include 128 MiB for the cluster, 16 MiB each for temporary files and shared memory, and 1 MiB over the image's declared volume. [Docker none network](https://docs.docker.com/engine/network/drivers/none/) · [Docker resource options](https://docs.docker.com/reference/cli/docker/container/run/)

## PostgreSQL connection and teardown

The fixed bootstrap configures a Unix socket with peer authentication for the local administrator and a loopback TCP endpoint with SCRAM authentication for one migration role. The role owns `public` by default or the exact predeclared schemas supplied by the protected caller. Its database grants allow connection and exclude database-level CREATE and TEMP. PostgreSQL requires database-level CREATE for schema and trusted extension installation. The resulting sandbox supports changes within provisioned schemas and uses its own fresh cluster for every rehearsal. [PostgreSQL authentication](https://www.postgresql.org/docs/18/auth-pg-hba-conf.html) · [CREATE EXTENSION privileges](https://www.postgresql.org/docs/18/sql-createextension.html)

`DockerWorker::run` applies one deadline to the rehearsal and awaits cleanup on both success and error. `with_client` runs a worker-only PostgreSQL client with server statement and lock limits, and `connect_stream` exposes the same time-bounded private byte stream for protocol-level batching. The protocol consumer bounds frame and retained-result sizes. The fixed bridge runs BusyBox nc inside the container and connects to its loopback PostgreSQL listener. Treat `connection_config` and PostgreSQL response data as private. Keep server messages, SQL and the ephemeral password out of diagnostics and artifacts. The worker runs only its fixed administrator bootstrap through `psql`. Migration execution uses the authenticated database protocol.

Each worker has a random name and matching ownership label. Normal teardown matches that label, name, image and full container ID, removes the exact owned container and confirms its absence. `CleanupUnconfirmed` carries the generated worker name and preserves an earlier operation error when applicable. Dropping a worker during cancellation schedules cleanup while the Tokio runtime remains available. If the caller or daemon stops abruptly, the operator can inspect the reported worker on the same daemon, compare its full ID, exact name, image and `io.database-migration-guard.worker` label, then remove that confirmed ID and check its absence. The server process has a separate maximum lifetime of 450 seconds. Automatic confirmation still requires the daemon to answer.

## Verification

Run the ignored worker lifecycle test against a fresh isolated daemon that contains only the selected approved image. Supply the exact executable path, Unix socket path and release as `DMG_WORKER_TEST_DOCKER`, `DMG_WORKER_TEST_SOCKET` and `DMG_WORKER_TEST_POSTGRES`. The fixture performs synthetic SQL under the migration role and checks Docker inventory after success, a failed operation, a wall deadline, failed bootstrap and a dropped handle.

```sh
cargo test --locked --offline -p dmg-simulate isolated_worker_lifecycle -- --ignored
```

The current implementation runs workers on Linux x86-64. The native Windows build exposes the unsupported-platform result. This layer supplies the disposable database and transport. Adapter execution, before-and-after snapshots and revision-bound evidence are separate DMG-12 work.
