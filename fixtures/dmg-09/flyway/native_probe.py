"""Run synthetic Flyway OSS migrations on a disposable PostgreSQL cluster."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import zlib


FLYWAY = "13.12.2"
SERVERS = {"15.20", "16.16", "17.12", "18.7"}
ROLE = "dmg_fixture"
BASE = {
    "V1__base.sql": (
        "CREATE TABLE tx_probe (id integer PRIMARY KEY);\n"
        "INSERT INTO tx_probe (id) VALUES (1);\n"
    )
}
ALTER = "ALTER TABLE tx_probe ADD COLUMN observed integer;\n"
INDEX = "CREATE INDEX CONCURRENTLY tx_probe_observed_idx ON tx_probe (id);\n"
FAIL = "INSERT INTO tx_probe (id) VALUES (1);\n"


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def require(condition, label, actual):
    if not condition:
        raise AssertionError(f"{label}: {json.dumps(actual, ensure_ascii=False)}")


def run(command, *, cwd, env, check=True):
    result = subprocess.run(
        list(map(str, command)),
        cwd=cwd,
        env=env,
        stdin=subprocess.DEVNULL,
        capture_output=True,
        text=True,
        encoding="utf-8",
        timeout=45,
    )
    if check and result.returncode:
        raise RuntimeError(f"{Path(command[0]).name}: {result.stdout}\n{result.stderr}")
    return result


def write_inputs(directory, files):
    for name, contents in files.items():
        target = directory / name
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(contents.encode("utf-8"))


def inventory(directory):
    return {
        path.relative_to(directory).as_posix(): sha256(path.read_bytes())
        for path in sorted(directory.rglob("*"))
        if path.is_file()
    }


def native_checksum(sql):
    """Flyway CRC32 for an untemplated UTF-8 file with ordinary line endings."""
    contents = sql.removeprefix("\ufeff").replace("\r\n", "\n").replace("\r", "\n")
    unsigned = zlib.crc32(contents.replace("\n", "").encode("utf-8"))
    return unsigned if unsigned < (1 << 31) else unsigned - (1 << 32)


class Probe:
    def __init__(self, pg_bin, flyway, scratch):
        self.bin = pg_bin
        self.flyway = flyway
        self.scratch = scratch
        self.data = scratch / "pgdata"
        self.password = secrets.token_urlsafe(32)
        self.env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(scratch),
            "LC_ALL": "C.UTF-8",
            "TZ": "UTC",
        }
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]
        self.records = []

    def pg(self, program, *args):
        return run([self.bin / program, *args], cwd=self.scratch, env=self.env).stdout.strip()

    @contextmanager
    def cluster(self):
        server_version = self.pg("postgres", "--version").split()[-1]
        require(server_version in SERVERS, "PostgreSQL binary pin", server_version)
        self.server = server_version
        pwfile = self.scratch / "init-password"
        pwfile.touch(mode=0o600)
        try:
            pwfile.write_text(self.password + "\n", encoding="ascii")
            self.pg(
                "initdb", "-D", self.data, "--auth-local=trust", "--auth-host=scram-sha-256",
                "--no-locale", "--encoding=UTF8", "--username", ROLE, "--pwfile", pwfile,
            )
        finally:
            pwfile.unlink()
        config = self.data / "postgresql.conf"
        with config.open("a", encoding="utf-8") as stream:
            stream.write(
                f"\nlisten_addresses = '127.0.0.1'\nport = {self.port}\n"
                f"unix_socket_directories = '{self.scratch.as_posix().replace(chr(39), chr(39) * 2)}'\n"
                "statement_timeout = '3500ms'\nlock_timeout = '2000ms'\n"
                "log_error_verbosity = verbose\n"
            )
        try:
            self.pg("pg_ctl", "-D", self.data, "-l", self.scratch / "postgres.log",
                    "-w", "-t", "15", "start")
            yield self
        finally:
            if (self.data / "postmaster.pid").is_file():
                self.pg("pg_ctl", "-D", self.data, "-w", "-t", "15", "-m", "immediate", "stop")

    def psql(self, database, sql):
        return self.pg(
            "psql", "-X", "-w", "-h", self.scratch, "-p", self.port, "-U", ROLE, "-d",
            database, "-v", "ON_ERROR_STOP=1", "-At", "-c", sql,
        )

    def create_case(self, case, files):
        self.psql("postgres", f"CREATE DATABASE {case}")
        directory = self.scratch / case
        directory.mkdir()
        write_inputs(directory, files)
        return directory

    def history(self, database):
        if not self.psql(database, "SELECT to_regclass('flyway_schema_history')"):
            return []
        return json.loads(self.psql(database, """
            SELECT COALESCE(json_agg(row_to_json(history) ORDER BY installed_rank), '[]'::json)
            FROM (
                SELECT installed_rank, version, description, script, checksum, success
                FROM flyway_schema_history
            ) AS history
        """))

    def flyway_run(self, database, directory, step, *settings, operation="migrate", snapshot=None):
        fixed = [
            "-outputType=json", f"-url=jdbc:postgresql://127.0.0.1:{self.port}/{database}",
            f"-user={ROLE}", "-locations=filesystem:.",
            "-cleanDisabled=true", "-connectRetries=0",
        ]
        result = run(
            [self.flyway, *fixed, *settings, operation],
            cwd=directory,
            env={**self.env, "FLYWAY_PASSWORD": self.password},
            check=False,
        )
        try:
            native = json.loads(result.stdout)
        except json.JSONDecodeError as error:
            raise RuntimeError(f"Flyway produced no valid JSON for {database}/{step}: {result.stderr}") from error
        if version := native.get("flywayVersion"):
            require(version == FLYWAY, "Flyway version pin", version)
        error = native.get("error") or {}
        error_text = error.get("message") or native.get("exception") or ""
        record = {
            "server": self.server,
            "flyway": FLYWAY,
            "case": database,
            "step": step,
            "operation": operation,
            "settings": list(settings),
            "input_sha256": inventory(directory),
            "exit": result.returncode,
            "migrations_executed": native.get("migrationsExecuted"),
            "error_code": error.get("errorCode"),
            "sqlstate": error.get("sqlState"),
            "error_kind": next(
                (tag for tag, phrase in (
                    ("mixed_statement_modes", "Detected both transactional and non-transactional statements"),
                    ("duplicate_version", "Found more than one migration with version"),
                    ("duplicate_repeatable", "Found more than one repeatable migration"),
                ) if phrase.lower() in error_text.lower()),
                None,
            ),
            "history": self.history(database),
        }
        if snapshot is not None:
            record["state"] = snapshot(database)
        self.records.append(record)
        return record

    def ordering(self):
        case = "ordering"
        repeat = "R__alpha_z.sql"
        v2 = "V2__next.sql"
        files = {
            "V01.0__base.sql": (
                "CREATE TABLE ordered (rank integer GENERATED ALWAYS AS IDENTITY, marker text);\n"
                "INSERT INTO ordered (marker) VALUES ('base');\n"
            ),
            "V1_2.00__early.sql": "INSERT INTO ordered (marker) VALUES ('1.2');\n",
            "V1.10__later.sql": "INSERT INTO ordered (marker) VALUES ('1.10');\n",
            v2: "INSERT INTO ordered (marker) VALUES ('2');\n-- original bytes\n",
            "V10__last.SQL": "INSERT INTO ordered (marker) VALUES ('10');\n",
            repeat: "INSERT INTO ordered (marker) VALUES ('alpha z');\n",
            "R__alpha0.sql": "INSERT INTO ordered (marker) VALUES ('alpha0');\n",
        }
        directory = self.create_case(case, files)

        def markers(database):
            return json.loads(self.psql(database, "SELECT json_agg(marker ORDER BY rank) FROM ordered"))

        initial = self.flyway_run(case, directory, "initial", snapshot=markers)
        expected_order = [
            "V01.0__base.sql", "V1_2.00__early.sql", "V1.10__later.sql", v2, "V10__last.SQL",
            repeat, "R__alpha0.sql",
        ]
        require(
            initial["exit"] == 0 and initial["migrations_executed"] == 7
            and [row["script"] for row in initial["history"]] == expected_order
            and [row["version"] for row in initial["history"]] == ["01.0", "1.2.00", "1.10", "2", "10", None, None]
            and [row["checksum"] for row in initial["history"]] == [
                native_checksum(files[filename]) for filename in expected_order
            ]
            and initial["state"] == ["base", "1.2", "1.10", "2", "10", "alpha z", "alpha0"],
            "initial version and repeatable order", initial,
        )
        write_inputs(directory, {v2: files[v2].replace("\n", "\r\n")})
        same = self.flyway_run(case, directory, "versioned_lf_to_crlf", snapshot=markers)
        require(
            same["exit"] == 0 and same["migrations_executed"] == 0 and same["history"] == initial["history"]
            and same["input_sha256"][v2] != initial["input_sha256"][v2]
            and same["state"] == initial["state"],
            "native checksum and Guard digest differ in newline handling", same,
        )
        update = "INSERT INTO ordered (marker) VALUES ('alpha z revised');\n"
        write_inputs(directory, {repeat: update})
        changed = self.flyway_run(case, directory, "repeatable_changed", snapshot=markers)
        require(
            changed["exit"] == 0 and changed["migrations_executed"] == 1
            and changed["history"][:-1] == initial["history"]
            and changed["history"][-1]["script"] == repeat
            and changed["history"][-1]["checksum"] == native_checksum(update)
            and changed["state"] == initial["state"] + ["alpha z revised"],
            "changed repeatable", changed,
        )

    def duplicate(self, case, files, expected):
        directory = self.create_case(case, files)
        result = self.flyway_run(case, directory, "resolve", operation="info")
        require(result["exit"] != 0 and result["error_kind"] == expected,
                f"{case} conflict", result)

    def transaction_state(self, database):
        return json.loads(self.psql(database, """
            SELECT json_build_object(
                'column_exists', EXISTS (
                    SELECT 1 FROM pg_attribute
                    WHERE attrelid = 'tx_probe'::regclass AND attname = 'observed' AND NOT attisdropped
                ),
                'index_valid', (
                    SELECT indisvalid FROM pg_index
                    WHERE indexrelid = to_regclass('tx_probe_observed_idx')
                )
            )
        """))

    def transaction(self, case, sql, *, settings=(), sidecar=None, expected):
        directory = self.create_case(case, BASE)
        setup = self.flyway_run(case, directory, "setup")
        require(setup["exit"] == 0 and setup["migrations_executed"] == 1, "transaction baseline", setup)
        name = "V2__candidate.sql"
        files = {name: sql}
        if sidecar is not None:
            files[name + ".conf"] = f"executeInTransaction={str(sidecar).lower()}\n"
        write_inputs(directory, files)
        result = self.flyway_run(case, directory, "candidate", *settings, snapshot=self.transaction_state)
        rows = [row["success"] for row in result["history"] if row["script"] == name]
        actual = (
            result["exit"] == 0, result["sqlstate"], result["error_kind"], result["state"]["column_exists"],
            result["state"]["index_valid"], rows,
        )
        require(actual == expected, f"{case} expected {expected}", result)

    def transactions(self):
        session_lock = "-postgresql.transactional.lock=false"
        self.transaction(
            "file_default", ALTER + FAIL, expected=(False, "23505", None, False, None, []),
        )
        self.transaction(
            "global_autocommit", ALTER + FAIL, settings=("-executeInTransaction=false",),
            expected=(False, "23505", None, True, None, []),
        )
        self.transaction(
            "autocommit_session_lock", ALTER + FAIL, settings=(session_lock, "-executeInTransaction=false"),
            expected=(False, "23505", None, True, None, [False]),
        )
        self.transaction(
            "auto_concurrent", INDEX, settings=(session_lock,),
            expected=(True, None, None, False, True, [True]),
        )
        self.transaction(
            "transactional_lock", INDEX,
            expected=(False, "55P03", None, False, False, []),
        )
        self.transaction(
            "mixed_default", ALTER + INDEX, settings=(session_lock,),
            expected=(False, None, "mixed_statement_modes", False, None, []),
        )
        self.transaction(
            "mixed_enabled", ALTER + INDEX + FAIL, settings=(session_lock, "-mixed=true"),
            expected=(False, "23505", None, True, True, [False]),
        )
        self.transaction(
            "sidecar_false", ALTER + INDEX, settings=(session_lock,), sidecar=False,
            expected=(True, None, None, True, True, [True]),
        )
        self.transaction(
            "sidecar_true", INDEX, settings=(session_lock,), sidecar=True,
            expected=(False, "25001", None, False, None, []),
        )

    def checks(self):
        version = run(
            [self.flyway, "-v"], cwd=self.scratch, env=self.env,
        ).stdout
        require(f"Flyway OSS Edition {FLYWAY} by Redgate" in version, "Flyway edition and pin", version)
        self.ordering()
        self.duplicate(
            "duplicate_version",
            {"V01.2__one.sql": "SELECT 1;\n", "V1_02.0__two.sql": "SELECT 2;\n"},
            "duplicate_version",
        )
        self.duplicate(
            "duplicate_repeatable",
            {"R__shared name.sql": "SELECT 1;\n", "R__shared_name.sql": "SELECT 2;\n"},
            "duplicate_repeatable",
        )
        self.transactions()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--flyway", type=Path, required=True)
    parser.add_argument("--pg-bin", type=Path, required=True)
    args = parser.parse_args()
    flyway = args.flyway.resolve(strict=True)
    pg_bin = args.pg_bin.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dmg-flyway-") as scratch:
        probe = Probe(pg_bin, flyway, Path(scratch))
        with probe.cluster():
            probe.checks()
    for record in probe.records:
        print(json.dumps(record, ensure_ascii=False, sort_keys=True))


if __name__ == "__main__":
    main()
