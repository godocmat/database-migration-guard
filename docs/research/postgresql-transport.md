# PostgreSQL inspection transport

The `dmg-inspect` crate connects to an explicitly authorized PostgreSQL TCP endpoint. `InspectionTarget::new` accepts one host, port, database, role, password, and PEM CA bundle from protected configuration. It verifies the server certificate against that bundle and the configured DNS name or IP. The driver requires TLS before authenticating. Connection errors expose an operation stage and, when supplied by PostgreSQL, a validated SQLSTATE. The public transport probe returns the server version number and block size after verifying a read-only repeatable-read transaction and its effective search path. [Rustls connector](https://docs.rs/tokio-postgres-rustls/0.14.0/tokio_postgres_rustls/struct.MakeRustlsConnect.html) · [PostgreSQL TLS verification](https://www.postgresql.org/docs/18/libpq-ssl.html) · [Transaction isolation](https://www.postgresql.org/docs/18/sql-set-transaction.html)

The crate-internal `with_read_only` method gives fixed catalog and history readers one transaction and its server information. Defaults are 5 seconds overall, 1 second per statement, 100 milliseconds for lock waits, and 3 seconds idle in a transaction. A single client deadline covers name resolution, TCP and TLS setup, the transaction, and rollback. The connection driver runs inline and closes its socket when the operation ends or reaches its deadline. The platform resolver can finish an in-progress DNS lookup independently. Server timeouts bound database work if a disconnected backend has yet to observe the closed socket. [Tokio deadline](https://docs.rs/tokio/1.54.1/tokio/time/fn.timeout.html) · [PostgreSQL timeouts](https://www.postgresql.org/docs/18/runtime-config-client.html#RUNTIME-CONFIG-CLIENT-STATEMENT)

## Verification

Rust 1.100.0 workspace formatting, tests, and Clippy passed on native Windows x86-64. The transport crate also passed formatting, tests, Clippy, and a disposable PostgreSQL 18.7 probe on Ubuntu 24.04 x86-64 under WSL2 with loopback networking only. The probe used the OpenSSL-enabled `postgres` binary with SHA-256 `f96e4f0bf7fdbca133f9ad9a74e8cf4c13fa511aa592d4472a9e4887db90ffc4`.

- The authorized CA and DNS name accepted a restricted SCRAM login. The same listener rejected the wrong CA and an IP missing from the leaf certificate's SAN. An incorrect password returned `28P01`.
- A PostgreSQL server with TLS disabled accepted the plaintext control and rejected the transport. The TLS-only fixture rejected an explicit plaintext client with `28000`. The local protocol control verified socket closure after TLS refusal and after a stalled negotiation.
- The verified read-only transaction rejected DDL with `25006`. The server statement timeout returned `57014`. The shorter client deadline interrupted a longer statement, and the backend disappeared after cleanup. A subsequent probe succeeded.

To reproduce, use a local PostgreSQL 18.7 build with TLS, OpenSSL, Python 3.11 or newer, and a loopback address for `localhost`. Build the test binary and use the executable path printed by Cargo. The probe creates and tears down its own servers, CA, and restricted role. [Native fixture](../../fixtures/dmg-10/catalog/transport_probe.py)

```sh
cargo test --locked -p dmg-inspect --no-run
python3 -B fixtures/dmg-10/catalog/transport_probe.py \
  --pg-bin /path/to/postgresql-18.7-tls/bin --test-binary /path/to/dmg_inspect-test
```
