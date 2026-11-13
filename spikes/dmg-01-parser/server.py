"""Exercise the brief's SQL on a private, disposable cluster using libpq."""

import argparse
from contextlib import contextmanager
import ctypes as c
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

FIXTURES = Path(__file__).resolve().parent / "fixtures"
VERSIONS = {"15.20": 150020, "16.16": 160016, "17.12": 170012, "18.7": 180007}
CASES = (
    ("quoted",), ("index",), ("updates",), ("fk-add", "fk-validate"),
    ("not-null",), ("part-fk",), ("unique",), ("do",), ("broken",),
    ("dollar",), ("eof",),
)
SETUP = """
CREATE TABLE t(id integer, v integer, fk integer);
CREATE TABLE p(id integer PRIMARY KEY);
CREATE TABLE part(fk integer) PARTITION BY RANGE(fk);
CREATE TABLE part_leaf PARTITION OF part FOR VALUES FROM (0) TO (100);
CREATE SCHEMA "App";
CREATE TABLE "App"."café"(id integer, v integer, "Old" text);
INSERT INTO p VALUES (1);
INSERT INTO t VALUES (1, NULL, 1), (2, NULL, NULL);
INSERT INTO part VALUES (1);
""".encode()


class Libpq:
    def __init__(self, path):
        lib = c.CDLL(str(path))
        ptr, text, num = c.c_void_p, c.c_char_p, c.c_int
        params = c.POINTER(text)
        functions = (
            ("PQconnectdbParams", ptr, (params, params, num)),
            ("PQstatus", num, (ptr,)),
            ("PQerrorMessage", text, (ptr,)),
            ("PQfinish", None, (ptr,)),
            ("PQsendQuery", num, (ptr, text)),
            ("PQgetResult", ptr, (ptr,)),
            ("PQresultStatus", num, (ptr,)),
            ("PQcmdStatus", text, (ptr,)),
            ("PQresultErrorField", text, (ptr, num)),
            ("PQntuples", num, (ptr,)),
            ("PQnfields", num, (ptr,)),
            ("PQgetvalue", text, (ptr, num, num)),
            ("PQclear", None, (ptr,)),
        )
        for name, result, args in functions:
            fn = getattr(lib, name)
            fn.restype = result
            fn.argtypes = args
            setattr(self, name, fn)

    @contextmanager
    def connect(self, socket, database):
        keys = (c.c_char_p * 6)(b"host", b"port", b"user", b"dbname", b"connect_timeout", None)
        vals = (c.c_char_p * 6)(os.fsencode(socket), b"55401", b"dmg01", database.encode(), b"5", None)
        conn = self.PQconnectdbParams(keys, vals, 0)
        try:
            if self.PQstatus(conn) != 0:
                raise RuntimeError(self.PQerrorMessage(conn).decode())
            yield conn
        finally:
            self.PQfinish(conn)

    def query(self, conn, sql):
        assert b"\0" not in sql
        # PQsendQuery uses one simple-protocol Query message. Drain every PGresult.
        if self.PQsendQuery(conn, sql) != 1:
            raise RuntimeError(self.PQerrorMessage(conn).decode())
        results = []
        while result := self.PQgetResult(conn):
            try:
                if self.PQresultStatus(result) in (1, 2):
                    results.append({
                        "tag": self.PQcmdStatus(result).decode(),
                        "rows": [
                            [self.PQgetvalue(result, row, col).decode() for col in range(self.PQnfields(result))]
                            for row in range(self.PQntuples(result))
                        ],
                    })
                else:
                    detail = {}
                    for key, code in (("sqlstate", "C"), ("message", "M"), ("file", "F"), ("routine", "R")):
                        value = self.PQresultErrorField(result, ord(code))
                        detail[key] = value.decode() if value else None
                    value = self.PQresultErrorField(result, ord("P"))
                    detail["position"] = int(value) if value else None
                    if value:
                        prefix = sql.decode()[:int(value) - 1]
                        detail["byte"] = len(prefix.encode())
                    results.append({"error": detail})
            finally:
                self.PQclear(result)
        assert results, "no server result"
        return results


def success(pq, conn, sql):
    results = pq.query(conn, sql)
    assert all("tag" in result for result in results), results
    return results[-1]["rows"]


def catalog(pq, conn, fixture):
    constraints = {
        "fk-add": ("t", "t_fk"), "fk-validate": ("t", "t_fk"),
        "not-null": ("t", "t_v_nn"), "part-fk": ("part", "part_fk"),
        "unique": ("t", "t_u"),
    }
    if fixture in constraints:
        table, name = constraints[fixture]
        return success(pq, conn, (
            "SELECT contype, convalidated::text FROM pg_constraint "
            f"WHERE conrelid = '{table}'::regclass AND conname = '{name}'"
        ).encode())
    if fixture == "broken":
        return success(pq, conn, b"SELECT to_regclass('t')::text")
    if fixture == "updates":
        return success(pq, conn, b"SELECT id, v FROM t ORDER BY id")
    if fixture == "index":
        return success(pq, conn, b"""
            SELECT indisunique::text, indisvalid::text, pg_get_expr(indpred, indrelid)
            FROM pg_index WHERE indexrelid = '"App".cafe_ix'::regclass
        """)
    if fixture == "quoted":
        return success(pq, conn, b"""
            SELECT attname FROM pg_attribute WHERE attrelid = '"App"."caf\xc3\xa9"'::regclass
            AND attnum > 0 AND NOT attisdropped ORDER BY attnum
        """)
    return None


def run(prefix, libpq, version):
    for key in list(os.environ):
        if key.startswith("PG"):
            del os.environ[key]
    pq = Libpq(libpq)
    with tempfile.TemporaryDirectory(prefix="dmg01-pg-") as temp:
        data, socket = Path(temp) / "data", Path(temp) / "socket"
        socket.mkdir(mode=0o700)
        initdb = prefix / "bin/initdb"
        subprocess.run([initdb, "-D", data, "--no-locale", "-E", "UTF8",
                        "--auth-local=trust", "--auth-host=reject", "-U", "dmg01"],
                       check=True, stdout=subprocess.DEVNULL, timeout=60)
        log = Path(temp) / "server.log"
        command = [
            prefix / "bin/postgres", "-D", data, "-k", socket, "-p", "55401",
            "-c", "listen_addresses=", "-c", "fsync=off", "-c", "standard_conforming_strings=on",
            "-c", "client_min_messages=warning", "-c", "statement_timeout=5000",
        ]
        with log.open("wb") as output, subprocess.Popen(
            command, stdout=output, stderr=subprocess.STDOUT
        ) as postgres:
            try:
                readiness = [
                    prefix / "bin/pg_isready", "-q", "-t", "1", "-h", socket,
                    "-p", "55401", "-U", "dmg01", "-d", "postgres",
                ]
                deadline = time.monotonic() + 15
                while postgres.poll() is None and time.monotonic() < deadline:
                    if subprocess.run(readiness, timeout=3).returncode == 0:
                        break
                    time.sleep(0.1)
                else:
                    raise RuntimeError(log.read_text())
                with pq.connect(socket, "postgres") as admin:
                    settings = success(pq, admin, b"""
                        SELECT current_setting('server_version_num'), current_setting('server_encoding'),
                               current_setting('client_encoding'), current_setting('standard_conforming_strings')
                    """)[0]
                    assert settings == [str(VERSIONS[version]), "UTF8", "UTF8", "on"], settings
                    for index, fixtures in enumerate(CASES):
                        database = f"dmg01_case_{index}"
                        success(pq, admin, f"CREATE DATABASE {database}".encode())
                        with pq.connect(socket, database) as conn:
                            success(pq, conn, SETUP)
                            for fixture in fixtures:
                                results = pq.query(conn, (FIXTURES / f"{fixture}.sql").read_bytes())
                                print(json.dumps({
                                    "case": fixture, "server_version_num": int(settings[0]),
                                    "server_encoding": settings[1], "client_encoding": settings[2],
                                    "standard_conforming_strings": settings[3],
                                    "protocol": "simple", "results": results,
                                    "observation": catalog(pq, conn, fixture),
                                }, ensure_ascii=False, sort_keys=True))
            finally:
                if postgres.poll() is None:
                    try:
                        # Fast shutdown with a bounded wait for our own child.
                        postgres.send_signal(signal.SIGINT)
                        postgres.wait(timeout=30)
                    except subprocess.TimeoutExpired as exc:
                        postgres.send_signal(signal.SIGQUIT)
                        try:
                            postgres.wait(timeout=5)
                        except subprocess.TimeoutExpired:
                            postgres.kill()
                            postgres.wait(timeout=5)
                        raise RuntimeError(log.read_text()) from exc

if __name__ == "__main__":
    cli = argparse.ArgumentParser(description=__doc__)
    cli.add_argument("prefix", type=Path, help="Path to the exact PostgreSQL install")
    cli.add_argument("version", choices=VERSIONS)
    cli.add_argument("--libpq", type=Path, help="Path to libpq, if it differs from lib/libpq.so")
    args = cli.parse_args()
    run(args.prefix.resolve(), args.libpq or args.prefix.resolve() / "lib/libpq.so", args.version)
