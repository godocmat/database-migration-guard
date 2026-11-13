# Live inspection and schema comparison verification

The [Windows smoke fixture](../../fixtures/dmg-11/inspect_diff_win.py) ran the production CLI from revision `dd5d5893f48b6966fcb4275725502d1574df815c`. The Windows x86-64 debug executable used Rust 1.100.0 and the locked workspace dependencies. Its SHA-256 was `c7a25c92d23fb7a623e75bfa6d27c5088227af1df2a0ff086c57fa8fdb04f522`. The host was Windows 11 build 26200 with Python 3.11.6 and OpenSSL 3.2.1 from Git for Windows.

Each fixture creates an independent PostgreSQL cluster on a loopback port, a temporary certificate authority, a server certificate for `localhost`, and an unprivileged SCRAM login. The login has database `CONNECT` and schema `USAGE` for the observed schema. A separate read verified TLS and confirmed that a query for application rows returns SQLSTATE `42501`. An unrelated trust anchor and the numeric loopback address each caused `dmg inspect` to return unavailable evidence with exit 3.

The fixture saved real inspection artifacts for three successive synthetic schema states. Comparison identified a new table as drift. It emitted addition and removal for a renamed column without a mapping, and a rename with the reviewed mapping. The application contract continued to require the old column name and reported that regression and an observed type change as a critical target violation. The unchanged `NOT NULL` contract remained satisfied on both sides. The comparison completed with exit 0 because all requested facts were observed. Its output retained structural digests and observation IDs and omitted credentials, application data, and deparsed definition text.

A second target collection applied the documented row limit. Inspection and comparison returned exit 3. The diff retained the observed type change and treated the omitted table's presence as unknown. After schema `USAGE` was revoked, the comparison also returned exit 3, with unknown contract facts and no claimed removals or target violations. Each fixture stopped its server and removed its temporary directory.

| PostgreSQL | Windows server executable SHA-256 | TLS, complete diff, partial diff, cleanup |
| --- | --- | --- |
| 15.20 | `72a09155b36c85c5a35f1a985ca698729ed9342af5664ed2282da5ee280d6745` | Passed |
| 16.16 | `b2c24e922b8f9598ddae1ca1a056c5143a8c62087d94ab3dd76dc06f811e621f` | Passed |
| 17.12 | `70334b2428dac81d111b4e3a6cfed01568b216e4ef3c5cc879d423a0fe69c0df` | Passed |
| 18.7 | `4e67c961ac182a36c44a18fefcb5548d1ad5b40b252abb9cfa1259112d7faf75` | Passed |

## Reproduction

Build the CLI with `cargo build --locked --offline -p dmg-cli` after fetching its pinned dependencies. Use an extracted [EnterpriseDB Windows x86-64 PostgreSQL archive](https://productsdl.enterprisedb.com/download-postgresql-binaries) for each version. The server, initdb, pg_ctl, and psql executables used for the recorded results passed Windows Authenticode verification as signed by EnterpriseDB Corporation.

Run the fixture with Python 3.11 or later. Supply the extracted PostgreSQL `bin` directory, CLI executable, and OpenSSL executable. Set `TEMP` to a directory controlled by the test account to keep the temporary catalog evidence and credentials there until teardown.

~~~powershell
python -B fixtures/dmg-11/inspect_diff_win.py --release 18.7 --pg-bin C:/fixture/pgsql/bin --dmg C:/fixture/dmg.exe --openssl C:/fixture/openssl.exe
~~~

This fixture verifies catalog inspection and structural comparison on Windows. Native migration history and the Docker rehearsal worker have separate fixtures.
