"""Verify Goose v3.30.1 SQL migration semantics on disposable PostgreSQL."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
from urllib.parse import urlencode


GOOSE = "v3.30.1"
POSTGRES = {"15.20", "16.16", "17.12", "18.7"}
ROLE = "dmg_fixture"
HISTORY = "public.goose_db_version"
BASE = "-- +goose Up\nCREATE TABLE public.probe (id integer PRIMARY KEY);\n"
ADD_FIRST = "ALTER TABLE public.probe ADD COLUMN first_value text;"
ADD_SECOND = "ALTER TABLE public.probe ADD COLUMN second_value text;"
INDEX = "CREATE INDEX CONCURRENTLY probe_id_idx ON public.probe (id);"
SQLSTATE = re.compile(r"ERROR:\s+([A-Z0-9]{5}):")
HISTORY_INITIALIZATION = 'ERROR:  42P01: relation "goose_db_version" does not exist'
SIMPLE_QUERY = re.compile(r"LOG:\s+00000: statement: (.*(?:\n\t[^\n]*)*)")


def require(condition, detail):
    if not condition:
        raise RuntimeError(detail)


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Probe:
    def __init__(self, goose, pg_bin, scratch):
        self.goose = goose
        self.pg = pg_bin
        self.scratch = scratch
        self.data = scratch / "data"
        self.log = scratch / "postgres.log"
        self.env = {
            "HOME": str(scratch),
            "PATH": os.defpath,
            "LC_ALL": "C.UTF-8",
            "TZ": "UTC",
            "DMG_GOOSE_VALUE": "synthetic",
        }

    def run(self, executable, *args, check=True):
        result = subprocess.run(
            [executable, *args], cwd=self.scratch, env=self.env, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, encoding="utf-8", timeout=30,
        )
        if check and result.returncode:
            raise RuntimeError(f"{Path(executable).name} exited {result.returncode}\n"
                               f"{result.stdout}{result.stderr}")
        return result

    @contextmanager
    def running(self):
        self.run(self.pg / "initdb", "-D", self.data, "--auth-local=trust", "--auth-host=reject",
                 "--no-locale", "--encoding=UTF8", f"--username={ROLE}")
        escaped = str(self.scratch).replace("\\", "\\\\").replace("'", "''")
        with (self.data / "postgresql.conf").open("a", encoding="utf-8") as config:
            config.write(f"\nlisten_addresses = ''\nunix_socket_directories = '{escaped}'\n"
                         "unix_socket_permissions = 0700\n"
                         "log_error_verbosity = verbose\nlog_min_error_statement = error\n"
                         "log_statement = 'all'\nstatement_timeout = '8s'\nlock_timeout = '2s'\n")
        ctl = self.pg / "pg_ctl"
        try:
            self.run(ctl, "-D", self.data, "-l", self.log, "-w", "-t", "15", "start")
            yield
        finally:
            if (self.data / "postmaster.pid").exists():
                self.run(ctl, "-D", self.data, "-w", "-t", "15", "-m", "immediate", "stop")

    def sql(self, database, statement):
        return self.run(
            self.pg / "psql", "-X", "-At", "--no-password", "-v", "ON_ERROR_STOP=1",
            "-h", self.scratch, "-U", ROLE, "-d", database, "-c", statement,
        ).stdout.strip()

    def fixture(self, name, files):
        self.sql("postgres", f'CREATE DATABASE "{name}"')
        directory = self.scratch / name
        for path, contents in files.items():
            output = directory / path
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_bytes(contents.encode("utf-8"))
        return directory

    def migrate(self, directory, command="up"):
        parameters = urlencode({"host": str(self.scratch), "user": ROLE, "sslmode": "disable"})
        url = f"postgres:///{directory.name}?{parameters}"
        offset = self.log.stat().st_size
        run = self.run(self.goose, "-env", "none", "-no-color", "-dir", directory,
                       "postgres", url, command, check=False)
        with self.log.open("rb") as stream:
            stream.seek(offset)
            log = stream.read().decode("utf-8")
        return run, log

    def history(self, directory):
        db = directory.name
        if not self.sql(db, f"SELECT to_regclass('{HISTORY}')"):
            return None
        return [json.loads(line) for line in self.sql(
            db, f"SELECT json_build_array(version_id, is_applied) FROM {HISTORY} ORDER BY id",
        ).splitlines()]

    def state(self, directory):
        db = directory.name
        columns = self.sql(db, "SELECT attname FROM pg_attribute "
                           "WHERE attrelid = to_regclass('public.probe') "
                           "AND attnum > 0 AND NOT attisdropped ORDER BY attnum").splitlines()
        index = self.sql(db, "SELECT indisvalid::text FROM pg_index "
                         "WHERE indexrelid = to_regclass('public.probe_id_idx')")
        return {"history": self.history(directory), "columns": columns,
                "index_valid": {"": None, "true": True, "false": False}[index]}


def evidence(directory, run, log, diagnostic=None):
    result = {"case": directory.name, "exit": run.returncode, "sqlstates": SQLSTATE.findall(log.replace(HISTORY_INITIALIZATION, "")),
              "input_sha256": {p.relative_to(directory).as_posix(): digest(p)
                               for p in sorted(directory.rglob("*")) if p.is_file()}}
    if HISTORY_INITIALIZATION in log:
        result["history_initialization_sqlstate"] = "42P01"
    if diagnostic is not None:
        require(diagnostic in run.stdout + run.stderr,
                f"{directory.name}: expected {diagnostic!r}\n{run.stdout}{run.stderr}")
        result["cli_diagnostic"] = diagnostic
    return result


def order_and_history(probe):
    name = "10_add_note.sql"
    contents = ("-- +goose uP\r\nALTER TABLE public.probe ADD COLUMN note text DEFAULT 'é;🧪';\r\n"
                "INSERT INTO public.probe (id) VALUES (1);\r\n"
                "-- +goose dOwN\r\nALTER TABLE public.probe DROP COLUMN note;\r\n")
    directory = probe.fixture("numeric_up_down", {
        "2_base.sql": BASE,
        "+3_.sql": "-- +goose Up\nSELECT 1;\n",
        name: contents,
        "4_ignored.SQL": "SELECT 1 / 0;\n",
        "child/5_ignored.sql": "-- +goose Up\nSELECT 1 / 0;\n",
    })
    up, log = probe.migrate(directory)
    applied = evidence(directory, up, log)
    after_up = probe.state(directory)
    require(up.returncode == 0 and not applied["sqlstates"], f"{up.stdout}{up.stderr}\n{log}")
    require(after_up == {"history": [[0, True], [2, True], [3, True], [10, True]],
                         "columns": ["id", "note"], "index_valid": None}, after_up)
    require(probe.sql(directory.name, "SELECT note FROM public.probe") == "é;🧪",
            "Unicode default was not retained")

    # Already-applied SQL is identified by version. Changing the Up bytes does not rerun it.
    (directory / name).write_bytes(contents.replace("DEFAULT 'é;🧪'", "DEFAULT 'edited'").encode("utf-8"))
    repeat, log = probe.migrate(directory)
    repeated = evidence(directory, repeat, log)
    require(repeat.returncode == 0 and not repeated["sqlstates"], f"{repeat.stdout}{repeat.stderr}")
    require(applied["input_sha256"][name] != repeated["input_sha256"][name], "digest did not change")
    require(probe.state(directory) == after_up, "history changed on reapply")
    require(probe.sql(directory.name, "SELECT note FROM public.probe") == "é;🧪",
            "Goose reran changed Up SQL")

    down, log = probe.migrate(directory, "down")
    after_down = probe.state(directory)
    require(down.returncode == 0 and not SQLSTATE.findall(log), f"{down.stdout}{down.stderr}")
    require(after_down == {"history": [[0, True], [2, True], [3, True]],
                           "columns": ["id"], "index_valid": None}, after_down)
    return {**applied, "after_up": after_up, "changed_file_sha256": repeated["input_sha256"][name],
            "repeat_up_exit": repeat.returncode, "down_exit": down.returncode, "after_down": after_down}


def discovery_errors(probe):
    cases = [
        ("duplicate", {"02_a.sql": BASE, "2_b.sql": BASE}, "duplicate version 2", 2, None),
        ("malformed_name", {"missing_prefix.sql": BASE},
         "failed to parse version from migration file", 1, None),
        ("unregistered_go", {"1_fixture.go": "package migrations\n"},
         "Go functions must be registered", 1, [[0, True]]),
    ]
    for label, files, diagnostic, exit_code, history in cases:
        directory = probe.fixture(label, files)
        run, log = probe.migrate(directory)
        result = evidence(directory, run, log, diagnostic)
        state = probe.state(directory)
        require(run.returncode == exit_code and not result["sqlstates"], result)
        require(state == {"history": history, "columns": [], "index_valid": None}, state)
        yield {**result, **state}


def transactions(probe):
    annotation = "-- +goose NO TRANSACTION"
    prefix = f"{annotation}\n-- +goose Up\n"
    block_sql = f"{ADD_FIRST}\n{INDEX}"
    grouped = f"-- +goose StatementBegin\n{block_sql}\n-- +goose StatementEnd\n"
    inline_sql = f"{ADD_FIRST} {INDEX}"
    commented = f"{ADD_FIRST} -- end of first query"
    no_space = f"{ADD_FIRST}-- continues the query buffer"
    cases = [
        ("default_wrapped", f"-- +goose Up\n{block_sql}\n",
         ["25001"], ["id"], None, [ADD_FIRST, INDEX]),
        ("autocommit_lines", f"{prefix}{commented}\n{INDEX}\n",
         [], ["id", "first_value"], True, [commented, INDEX]),
        ("same_line_group", f"{prefix}{inline_sql}\n",
         ["25001"], ["id"], None, [inline_sql]),
        ("attached_comment_group", f"{prefix}{no_space}\n{INDEX}\n",
         ["25001"], ["id"], None, [f"{no_space}\n{INDEX}"]),
        ("annotated_group", f"{prefix}{grouped}",
         ["25001"], ["id"], None, [block_sql]),
        ("group_after_commit", f"{prefix}{ADD_SECOND}\n{grouped}",
         ["25001"], ["id", "second_value"], None, [ADD_SECOND, block_sql]),
        ("directive_in_down", f"-- +goose Up\n{ADD_FIRST}\n{INDEX}\n-- +goose Down\n{annotation}\n",
         [], ["id", "first_value"], True, [ADD_FIRST, INDEX]),
        ("annotated_unterminated", f"{prefix}-- +goose StatementBegin\n{INDEX.removesuffix(';')}\n"
         "-- +goose StatementEnd\n", [], ["id"], True, [INDEX.removesuffix(";")]),
    ]
    for label, contents, states, columns, index, queries in cases:
        directory = probe.fixture(label, {"1_base.sql": BASE})
        base, _ = probe.migrate(directory)
        require(base.returncode == 0, f"{base.stdout}{base.stderr}")
        (directory / "2_change.sql").write_bytes(contents.encode("utf-8"))
        run, log = probe.migrate(directory)
        result = evidence(directory, run, log)
        state = probe.state(directory)
        history = [[0, True], [1, True]] if states else [[0, True], [1, True], [2, True]]
        require(run.returncode == int(bool(states)) and result["sqlstates"] == states,
                f"{result}\n{run.stdout}{run.stderr}\n{log}")
        require(state == {"history": history, "columns": columns, "index_valid": index}, state)
        observed_queries = [query.replace("\n\t", "\n") for query in SIMPLE_QUERY.findall(log)
                            if query.startswith(("ALTER TABLE public.probe", "CREATE INDEX CONCURRENTLY"))]
        require(observed_queries == queries, f"{label}: observed {observed_queries!r}\n{log}")
        yield {**result, **state, "simple_queries": observed_queries}


def optional_down(probe):
    directory = probe.fixture("optional_down", {"1_base.sql": BASE})
    up, log = probe.migrate(directory)
    result = evidence(directory, up, log)
    after_up = probe.state(directory)
    require(up.returncode == 0 and not result["sqlstates"]
            and after_up == {"history": [[0, True], [1, True]], "columns": ["id"], "index_valid": None},
            f"{up.stdout}{up.stderr}\n{after_up}")
    down, log = probe.migrate(directory, "down")
    after_down = probe.state(directory)
    require(down.returncode == 0 and not SQLSTATE.findall(log)
            and after_down == {"history": [[0, True]], "columns": ["id"], "index_valid": None},
            f"{down.stdout}{down.stderr}\n{after_down}")
    return {**result, "after_up": after_up, "down_exit": down.returncode, "after_down": after_down}


def annotations(probe):
    for label, contents, diagnostic in [
        ("unknown_annotation", "-- +goose Up\n-- +goose invalid\nSELECT 1;\n", "not supported"),
        ("indented_annotation", " -- +goose Up\nSELECT 1;\n", "contains leading whitespace"),
        ("uppercase_marker", "-- +GOOSE Up\nSELECT 1;\n", "unexpected state 0"),
        ("missing_statement_end", "-- +goose Up\n-- +goose StatementBegin\nSELECT 1;\n",
         "missing '-- +goose StatementEnd'"),
        ("missing_semicolon", "-- +goose Up\nSELECT 1\n", "missing semicolon"),
    ]:
        directory = probe.fixture(label, {"1_case.sql": contents})
        run, log = probe.migrate(directory)
        result = evidence(directory, run, log, diagnostic)
        state = probe.state(directory)
        require(run.returncode == 1 and not result["sqlstates"] and state["history"] == [[0, True]],
                f"{result}\n{state}\n{run.stdout}{run.stderr}")
        yield {**result, "history": state["history"]}

    directory = probe.fixture("envsub", {"1_case.sql":
        "-- +goose Up\nCREATE TABLE public.probe (id integer, value text);\n"
        "-- +goose ENVSUB ON\nINSERT INTO public.probe VALUES (1, '${DMG_GOOSE_VALUE}');\n"
        "-- +goose ENVSUB OFF\nINSERT INTO public.probe VALUES (2, '${DMG_GOOSE_VALUE}');\n"})
    run, log = probe.migrate(directory)
    result = evidence(directory, run, log)
    values = probe.sql(directory.name, "SELECT value FROM public.probe ORDER BY id").splitlines()
    history = probe.history(directory)
    require(run.returncode == 0 and not result["sqlstates"] and values == ["synthetic", "${DMG_GOOSE_VALUE}"]
            and history == [[0, True], [1, True]], f"{result}\n{values}\n{run.stdout}{run.stderr}")
    yield {**result, "values": values, "history": history}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--goose", type=Path, required=True)
    parser.add_argument("--pg-bin", type=Path, required=True)
    args = parser.parse_args()
    goose = args.goose.resolve(strict=True)
    pg_bin = args.pg_bin.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dmg-goose-", dir="/tmp") as directory:
        probe = Probe(goose, pg_bin, Path(directory))
        cli_version = probe.run(goose, "--version").stdout.strip()
        require(cli_version == f"goose version: {GOOSE}", f"unexpected Goose version: {cli_version}")
        server_version = probe.run(pg_bin / "postgres", "--version").stdout.split()[-1]
        psql_version = probe.run(pg_bin / "psql", "--version").stdout.split()[-1]
        require(server_version in POSTGRES and psql_version == server_version,
                f"unexpected server/client versions: {server_version} / {psql_version}")
        with probe.running():
            require(probe.sql("postgres", "SHOW server_version") == server_version, "wrong server")
            results = [order_and_history(probe), optional_down(probe), *discovery_errors(probe), *transactions(probe),
                       *annotations(probe)]
        header = {"goose": GOOSE, "postgres": server_version, "cases": len(results),
                  "sha256": {"goose": digest(goose), "postgres": digest(pg_bin / "postgres"),
                             "psql": digest(pg_bin / "psql")}}
    for row in [header, *results]:
        print(json.dumps(row, sort_keys=True))


if __name__ == "__main__":
    main()
