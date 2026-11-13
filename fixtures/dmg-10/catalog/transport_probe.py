"""Run the Rust transport assertions against private disposable PostgreSQL clusters."""

from __future__ import annotations

import argparse
from pathlib import Path
import subprocess
import tempfile

from native_probe import ADMIN, READER, Cluster, sha256

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pg-bin", required=True, type=Path)
    parser.add_argument("--test-binary", required=True, type=Path)
    args = parser.parse_args()
    pg_bin = args.pg_bin.resolve(strict=True)
    test_binary = args.test_binary.resolve(strict=True)
    version = subprocess.run(
        [pg_bin / "postgres", "--version"], check=True, text=True,
        capture_output=True, timeout=5,
    ).stdout.strip()
    if not version.endswith("18.7"):
        raise RuntimeError("expected the PostgreSQL 18.7 fixture binaries")

    with tempfile.TemporaryDirectory(prefix="dmg-transport-") as directory:
        scratch = Path(directory)
        tls_path = scratch / "tls"
        no_tls_path = scratch / "no-tls"
        tls_path.mkdir()
        no_tls_path.mkdir()
        tls = Cluster(pg_bin, tls_path, tls=True)
        no_tls = Cluster(pg_bin, no_tls_path, tls=False)
        with tls.running(), no_tls.running():
            (no_tls.data / "pg_hba.conf").write_text(
                "local all all trust\n"
                f"host postgres {READER} 127.0.0.1/32 scram-sha-256\n"
                "host all all 127.0.0.1/32 reject\n", encoding="ascii",
            )
            no_tls.psql(f"ALTER ROLE {READER} PASSWORD '{tls.password}'")
            with (no_tls.data / "postgresql.conf").open("a", encoding="utf-8") as conf:
                conf.write("\nlisten_addresses = '127.0.0.1'\nssl = off\n")
            no_tls.run(pg_bin / "pg_ctl", "-D", str(no_tls.data), "-w",
                       "-t", "15", "-l", "postgres.log", "restart")
            control = no_tls.run(
                pg_bin / "psql", "-XqAtw", "-U", READER, "-d", "postgres",
                "-h", "127.0.0.1", "-p", str(no_tls.port),
                "-c", "SHOW ssl", check=False,
                extra_env={"PGSSLMODE": "disable", "PGPASSWORD": tls.password},
            )
            if control.returncode or control.stdout.strip() != "off":
                raise RuntimeError("could not establish the non-TLS control session")

            result = subprocess.run(
                [test_binary, "transport::tests::native_tls_read_only_transport",
                 "--ignored", "--exact", "--nocapture"],
                check=False, text=True, capture_output=True, timeout=20,
                env={
                    **tls.env,
                    "DMG_TEST_POSTGRES_PORT": str(tls.port),
                    "DMG_TEST_POSTGRES_NO_TLS_PORT": str(no_tls.port),
                    "DMG_TEST_POSTGRES_CA": str(tls_path / "ca.crt"),
                    "DMG_TEST_POSTGRES_WRONG_CA": str(tls_path / "untrusted.crt"),
                    "DMG_TEST_POSTGRES_PASSWORD": tls.password,
                },
            )
            output = result.stdout + result.stderr
            if tls.password in output:
                raise RuntimeError("transport assertion output exposed a protected value")
            print(output, end="")
            if result.returncode or "1 passed" not in output:
                raise RuntimeError("the Rust transport assertion did not pass")

    print(version)
    print(f"postgres SHA-256 {sha256(pg_bin / 'postgres')}")


if __name__ == "__main__":
    main()
