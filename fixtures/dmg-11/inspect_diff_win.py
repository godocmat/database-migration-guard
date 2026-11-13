"""Verify inspection, schema diff, and application contracts against disposable Windows PostgreSQL."""

from __future__ import annotations

import argparse
import hashlib
import ipaddress
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile


ADMIN = "dmg_diff_setup"
READER = "dmg_diff_reader"
SCHEMA = "review"
DEFINITION_MARKER = "fixture_definition_value"
ROW_MARKER = "fixture_row_value"
VERSIONS = {"15.20": 150020, "16.16": 160016, "17.12": 170012, "18.7": 180007}
SETUP = """
CREATE ROLE dmg_diff_reader LOGIN INHERIT NOSUPERUSER NOCREATEDB NOCREATEROLE
    NOREPLICATION NOBYPASSRLS;
REVOKE CONNECT, TEMPORARY ON DATABASE postgres FROM PUBLIC;
GRANT CONNECT ON DATABASE postgres TO dmg_diff_reader;
CREATE SCHEMA review;
CREATE SCHEMA concealed;
GRANT USAGE ON SCHEMA review TO dmg_diff_reader;
CREATE TABLE review."Orders" (
    id bigint NOT NULL, "Note" text DEFAULT 'fixture_definition_value'
);
CREATE TABLE review."Registry" (id bigint NOT NULL);
CREATE TABLE concealed.private_fixture (id bigint NOT NULL);
INSERT INTO review."Orders" VALUES (1, 'fixture_row_value');
"""


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def read_json(path):
    return json.loads(path.read_bytes())


def save_json(path, value):
    path.write_text(json.dumps(value, indent=2) + "\n", encoding="utf-8")


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Fixture:
    def __init__(self, root, pg_bin, binary, openssl, server_version):
        self.root = root
        self.data = root / "data"
        self.pg_bin = pg_bin
        self.binary = binary
        self.openssl = openssl
        self.server_version = server_version
        self.major = server_version // 10_000
        self.setup_password = secrets.token_hex(24)
        self.reader_password = secrets.token_hex(24)
        self.env = {key: os.environ[key] for key in ("SystemRoot", "SystemDrive") if key in os.environ}
        self.env.update({
            "TEMP": str(root), "TMP": str(root), "PGCONNECT_TIMEOUT": "4",
            "PATH": str(pg_bin) + os.pathsep + str(Path(self.env["SystemRoot"]) / "System32"),
        })
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
            listener.bind(("127.0.0.1", 0))
            self.port = listener.getsockname()[1]

    def run(self, executable, *args, sql=None, env=None, exit_code=0, capture=True):
        result = subprocess.run(
            [executable, *args], cwd=self.root, env={**self.env, **(env or {})},
            input=sql, stdin=subprocess.DEVNULL if sql is None else None,
            text=True, encoding="utf-8", timeout=30,
            stdout=subprocess.PIPE if capture else subprocess.DEVNULL,
            stderr=subprocess.PIPE if capture else subprocess.DEVNULL,
            creationflags=subprocess.CREATE_NO_WINDOW,
        )
        if result.returncode != exit_code:
            stderr = (result.stderr or "").replace(self.setup_password, "<fixture password>")
            stderr = stderr.replace(self.reader_password, "<fixture password>")
            raise RuntimeError(
                f"{Path(executable).name} exited {result.returncode}, expected {exit_code}\n{stderr}"
            )
        return result

    def certificates(self):
        for ca in ("fixture-ca", "other-ca"):
            self.run(
                self.openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
                "-subj", f"/CN={ca}", "-keyout", f"{ca}.key", "-out", f"{ca}.crt",
            )
        self.run(
            self.openssl, "req", "-newkey", "rsa:2048", "-nodes", "-subj", "/CN=localhost",
            "-keyout", "server.key", "-out", "server.csr",
        )
        (self.root / "server.ext").write_text(
            "subjectAltName = DNS:localhost\nextendedKeyUsage = serverAuth\n", encoding="ascii",
        )
        self.run(
            self.openssl, "x509", "-req", "-days", "1", "-in", "server.csr", "-CA",
            "fixture-ca.crt", "-CAkey", "fixture-ca.key", "-CAcreateserial", "-extfile",
            "server.ext", "-out", "server.crt",
        )

    def start(self):
        self.certificates()
        bootstrap = self.root / "bootstrap-password"
        bootstrap.write_text(self.setup_password + "\n", encoding="ascii")
        self.run(
            self.pg_bin / "initdb.exe", "-D", self.data, "--auth=scram-sha-256", "--encoding=UTF8",
            "--locale=C", "--no-sync", f"--username={ADMIN}", f"--pwfile={bootstrap}",
        )
        bootstrap.unlink()
        (self.root / "reader-password").write_bytes(self.reader_password.encode("ascii"))
        for item in ("server.key", "server.crt"):
            (self.root / item).replace(self.data / item)
        with (self.data / "postgresql.conf").open("a", encoding="ascii") as config:
            config.write(
                f"\nlisten_addresses = '127.0.0.1'\nport = {self.port}\n"
                "ssl = on\nautovacuum = off\nlog_statement = 'none'\n"
                "log_min_error_statement = 'panic'\n",
            )
        (self.data / "pg_hba.conf").write_text(
            f"hostssl postgres {ADMIN},{READER} 127.0.0.1/32 scram-sha-256\n"
            "host all all 127.0.0.1/32 reject\n",
            encoding="ascii",
        )
        # A PostgreSQL child can retain inherited capture handles on Windows.
        self.run(
            self.pg_bin / "pg_ctl.exe", "start", "-D", self.data, "-l", self.root / "postgres.log",
            "-w", "-t", "15", capture=False,
        )
        self.psql(SETUP + f"ALTER ROLE {READER} PASSWORD '{self.reader_password}';")

    def stop(self):
        pid_file = self.data / "postmaster.pid"
        if pid_file.is_file():
            try:
                self.run(self.pg_bin / "pg_ctl.exe", "stop", "-D", self.data, "-m", "fast", "-w", "-t", "10")
            except (RuntimeError, subprocess.TimeoutExpired):
                if pid_file.is_file():
                    self.run(
                        self.pg_bin / "pg_ctl.exe", "stop", "-D", self.data, "-m", "immediate",
                        "-w", "-t", "10",
                    )
        require(not pid_file.exists(), "fixture PostgreSQL process did not stop")

    def psql(self, sql, user=ADMIN, exit_code=0):
        return self.run(
            self.pg_bin / "psql.exe", "-XqAtw", "-v", "ON_ERROR_STOP=1", "-v", "VERBOSITY=verbose",
            sql=sql, exit_code=exit_code,
            env={
                "PGHOST": "localhost", "PGHOSTADDR": "127.0.0.1", "PGPORT": str(self.port),
                "PGDATABASE": "postgres", "PGUSER": user, "PGSSLMODE": "verify-full",
                "PGSSLROOTCERT": str(self.root / "fixture-ca.crt"),
                "PGPASSWORD": self.reader_password if user == READER else self.setup_password,
            },
        )

    def inspect(self, name, *, row_limit=2048, ca="fixture-ca.crt", host="localhost", exit_code=0):
        config = self.root / f"{name}.toml"
        config.write_text(
            f'schema_version = 1\ntarget_id = "fixture-{name}"\ntarget = {self.major}\n'
            f'schemas = ["{SCHEMA}"]\nhost = "{host}"\nport = {self.port}\ndatabase = "postgres"\n'
            f'user = "{READER}"\nca_file = "{ca}"\npassword_file = "reader-password"\n'
            f'definitions = true\nrow_limit = {row_limit}\n',
            encoding="ascii",
        )
        path = self.root / f"{name}.json"
        result = self.run(self.binary, "inspect", "--connection", config, "--output", path, exit_code=exit_code)
        require(not result.stdout and not result.stderr, f"{name} inspection produced terminal output")
        self.verify_redaction(path)
        evidence = read_json(path)
        require(evidence["observation"]["target_id"] == f"fixture-{name}", "inspection lost target ID")
        return evidence

    def diff(self, target, name, *args, exit_code=0):
        path = self.root / f"{name}.json"
        result = self.run(
            self.binary, "diff", "--expected", "expected.json", "--current", "current.json",
            "--target", f"{target}.json", "--application-contract", "contract.json",
            "--format", "json", "--output", path, *args, exit_code=exit_code,
        )
        require(not result.stdout and not result.stderr, f"{name} diff produced terminal output")
        self.verify_redaction(path, include_definition=True)
        return read_json(path)

    def verify_redaction(self, path, *, include_definition=False):
        markers = [self.setup_password, self.reader_password, ROW_MARKER, ADMIN, READER, "private_fixture"]
        if include_definition:
            markers.append(DEFINITION_MARKER)
        raw = path.read_bytes()
        require(all(marker.encode("ascii") not in raw for marker in markers), "artifact disclosed fixture data")


def column(name, type_name, access, nullability):
    return {
        "name": name, "access": access, "nullability": nullability,
        "data_type": {
            "name": {"schema": "pg_catalog", "name": type_name},
            "type_modifier": -1, "array_dimensions": 0,
        },
    }


def contract_and_rename(fixture):
    save_json(fixture.root / "contract.json", {
        "schema_version": 1, "application": "fixture-orders", "version": "1.2",
        "critical": True,
        "tables": [
            {
                "table": {"schema": SCHEMA, "name": "Orders"}, "kind": "ordinary",
                "columns": [column("id", "int8", "read", "not_null"), column("Note", "text", "read_write", "nullable")],
            },
            {"table": {"schema": SCHEMA, "name": "ZDrift"}},
        ],
    })
    old = {"kind": "column", "schema": SCHEMA, "name": "Orders", "member": "Note"}
    save_json(fixture.root / "rename.json", {
        "schema_version": 1, "renames": [{"from": old, "to": {**old, "member": "Description"}}],
    })


def check_complete(fixture):
    expected = fixture.inspect("expected")
    require(expected["observation"]["server_version_num"] == fixture.server_version, "unexpected server version")
    require(DEFINITION_MARKER.encode("ascii") in (fixture.root / "expected.json").read_bytes(), "definition not captured")
    fixture.psql('CREATE TABLE review."ZDrift" (id integer);')
    current = fixture.inspect("current")
    require(current["scope"] == [{"schema": SCHEMA, "state": "visible"}], "authorized scope not visible")
    fixture.psql(
        'ALTER TABLE review."Orders" RENAME COLUMN "Note" TO "Description";\n'
        'ALTER TABLE review."Orders" ALTER COLUMN id TYPE integer;\n',
    )
    fixture.inspect("target")
    contract_and_rename(fixture)

    unmapped = fixture.diff("target", "unmapped")
    require({change["kind"] for change in unmapped["current_to_target"]["changes"]} ==
            {"added", "removed", "modified"}, "unmapped column change inferred a rename")
    mapped = fixture.diff("target", "mapped", "--proposed-renames", "rename.json")
    require(mapped["coverage"] == "complete", "complete inputs became incomplete")
    before = mapped["expected_to_current"]["after"]
    after = mapped["current_to_target"]["before"]
    require(before == after and before["observation"]["target_id"] == "fixture-current"
            and before["structural_digest"], "comparisons lost the current observation or digest")
    drift = mapped["expected_to_current"]["changes"]
    require(len(drift) == 1 and drift[0]["kind"] == "added" and drift[0]["object"]["identity"]["name"] == "ZDrift",
            "known drift missing from the expected-to-current comparison")
    rename, = [change for change in mapped["current_to_target"]["changes"] if change["kind"] == "renamed"]
    require(rename["from"]["identity"]["member"] == "Note" and rename["to"]["identity"]["member"] == "Description",
            "reviewed rename lost exact column identities")
    assessment, = mapped["application_contracts"]
    require(assessment["current"]["status"] == "satisfied" and assessment["target"]["status"] == "violated",
            "application contract did not regress after its schema changed")
    require(assessment["critical_target_violation"], "application violation lost critical marker")
    require(any(check.get("column") == "Note" and check["transition"] == "regression"
                and check["target"].get("reason", {}).get("kind") == "missing_column"
                for check in assessment["checks"]), "reviewed rename hid the old-name application regression")
    require(any(check.get("column") == "id" and check["transition"] == "regression"
                and check["target"].get("reason", {}).get("kind") == "column_type_mismatch"
                for check in assessment["checks"]), "application contract missed the observed type regression")
    require(any(check.get("column") == "id" and check["fact"]["kind"] == "nullability"
                and check["current"]["status"] == check["target"]["status"] == "satisfied"
                for check in assessment["checks"]), "column NOT NULL was not preserved")


def check_partial(fixture):
    partial = fixture.inspect("truncated", row_limit=2, exit_code=3)
    require(any(gap["field"] == "relations" and gap["reason"] == "truncated"
                for gap in partial["snapshot"]["gaps"]), "relation truncation lacked a typed gap")
    diff = fixture.diff("truncated", "truncated-diff", exit_code=3)
    require(diff["coverage"] == "incomplete", "truncated comparison had complete coverage")
    changes = diff["current_to_target"]["changes"]
    require(any(change["kind"] == "modified" and "data_type" in change["fields"] for change in changes),
            "truncation hid an observed type change")
    require(all(change["kind"] != "removed" for change in changes), "truncation invented a removal")
    assessment, = diff["application_contracts"]
    require(assessment["target"]["coverage"] == "incomplete" and assessment["critical_target_violation"],
            "partial comparison lost a known application violation or marked coverage complete")
    require(any(check["table"]["name"] == "ZDrift" and check["target"]["status"] == "unknown"
                for check in assessment["checks"]), "omitted table was treated as missing or present")

    fixture.psql(f"REVOKE USAGE ON SCHEMA {SCHEMA} FROM {READER};")
    denied = fixture.inspect("denied", exit_code=3)
    require(denied["scope"] == [{"schema": SCHEMA, "state": "denied"}], "denied schema was not reported")
    diff = fixture.diff("denied", "denied-diff", exit_code=3)
    require(diff["coverage"] == "incomplete" and not diff["current_to_target"]["changes"],
            "denied schema produced known target changes")
    assessment, = diff["application_contracts"]
    require(assessment["target"]["status"] == "unknown" and not assessment["critical_target_violation"],
            "denied schema produced a false application violation")


def verify(fixture):
    fixture.start()
    probe = fixture.psql(
        "SELECT ssl FROM pg_catalog.pg_stat_ssl WHERE pid = pg_backend_pid();", user=READER,
    )
    require(probe.stdout.strip() == "t", "restricted fixture connection did not use TLS")
    denied = fixture.psql('SELECT "Note" FROM review."Orders";', user=READER, exit_code=3)
    require(not denied.stdout and "42501" in denied.stderr, "fixture reader could read an application row")
    for label, options in [("wrong-ca", {"ca": "other-ca.crt"}), ("wrong-name", {"host": "127.0.0.1"})]:
        evidence = fixture.inspect(label, exit_code=3, **options)
        require(evidence["snapshot"] is None and evidence["observation"]["server_version_num"] is None,
                "TLS validation failure returned an observed schema")
        require(evidence["scope"] == [{"schema": SCHEMA, "state": "unavailable"}],
                "TLS validation failure lost the unavailable scope")
    check_complete(fixture)
    check_partial(fixture)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pg-bin", type=Path, required=True)
    parser.add_argument("--release", choices=VERSIONS, required=True)
    parser.add_argument("--dmg", type=Path, required=True)
    parser.add_argument("--openssl", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this fixture requires Windows executables")
    addresses = socket.getaddrinfo("localhost", 0, type=socket.SOCK_STREAM)
    require(addresses and all(ipaddress.ip_address(address[4][0]).is_loopback for address in addresses),
            "localhost did not resolve exclusively to loopback")
    pg_bin = args.pg_bin.resolve(strict=True)
    dmg = args.dmg.resolve(strict=True)
    openssl = args.openssl.resolve(strict=True)
    parent = Path(tempfile.gettempdir()).resolve(strict=True)
    root = Path(tempfile.mkdtemp(prefix="dmg-inspect-diff-", dir=parent)).resolve(strict=True)
    require(root.parent == parent and root.name.startswith("dmg-inspect-diff-"), "unsafe fixture path")
    fixture = Fixture(root, pg_bin, dmg, openssl, VERSIONS[args.release])
    try:
        verify(fixture)
    finally:
        fixture.stop()
        shutil.rmtree(root)
    require(not root.exists(), "fixture directory was not removed")
    print(json.dumps({
        "postgresql": args.release, "postgres_sha256": sha256(fixture.pg_bin / "postgres.exe"),
        "dmg_sha256": sha256(fixture.binary), "restricted_tls": "passed", "complete_diff": "passed",
        "partial_diff": "passed", "cleanup": "passed",
    }, sort_keys=True))


if __name__ == "__main__":
    main()
