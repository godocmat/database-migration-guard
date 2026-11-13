"""Verify Supabase CLI 3.0.0 against a temporary local PostgreSQL cluster."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import socket
import subprocess
import tempfile


CLI_VERSION = "3.0.0"
ROLE = "dmg_fixture"
HISTORY = "supabase_migrations.schema_migrations"
BASE = "CREATE TABLE public.probe (id integer PRIMARY KEY);"
ADD_FIRST = "ALTER TABLE public.probe ADD COLUMN first_value text;"
ADD_SECOND = "ALTER TABLE public.probe ADD COLUMN second_value text;"
DIVIDE = "SELECT 1 / 0;"
OPT_OUT = "-- pg-delta: transaction=false"
SQLSTATE = re.compile(r"ERROR:\s+([0-9A-Z]{5}):")
GENERATED_NAME = re.compile(r"[0-9]{14}_dmg_fixture\.sql")


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def file_sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


class Probe:
    def __init__(self, cli, pg_bin, scratch):
        self.cli = cli
        self.pg_bin = pg_bin
        self.scratch = scratch
        self.data = scratch / "data"
        self.log = scratch / "postgres.log"
        self.password = secrets.token_urlsafe(24)
        self.env = {
            "PATH": os.defpath,
            "HOME": str(scratch),
            "XDG_CONFIG_HOME": str(scratch / "config"),
            "XDG_CACHE_HOME": str(scratch / "cache"),
            "LC_ALL": "C.UTF-8",
            "TZ": "UTC",
            "PGCONNECT_TIMEOUT": "5",
        }
        with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as sock:
            sock.bind(("127.0.0.1", 0))
            self.port = sock.getsockname()[1]

    def run(self, executable, *args, cwd=None, check=True):
        try:
            result = subprocess.run(
                [executable, *args], cwd=cwd or self.scratch, env=self.env,
                stdin=subprocess.DEVNULL, capture_output=True, text=True, encoding="utf-8", timeout=40,
            )
        except subprocess.TimeoutExpired:
            raise RuntimeError(f"{Path(executable).name} timed out") from None
        if check and result.returncode:
            detail = (result.stdout + result.stderr).replace(self.password, "<local password>")
            raise RuntimeError(f"{Path(executable).name} exited {result.returncode}\n{detail}")
        return result

    @contextmanager
    def running(self):
        password_file = self.scratch / "password"
        password_file.write_text(self.password + "\n", encoding="ascii")
        password_file.chmod(0o600)
        self.run(
            self.pg_bin / "initdb", "-D", str(self.data), "--auth-local=trust",
            "--auth-host=scram-sha-256", f"--pwfile={password_file}", "--no-locale",
            "--encoding=UTF8", f"--username={ROLE}",
        )
        password_file.unlink()
        socket_path = str(self.scratch).replace("\\", "\\\\").replace("'", "''")
        with (self.data / "postgresql.conf").open("a", encoding="utf-8") as config:
            config.write(
                f"\nlisten_addresses = '127.0.0.1'\nport = {self.port}\n"
                f"unix_socket_directories = '{socket_path}'\n"
                "log_error_verbosity = verbose\nlog_min_error_statement = error\n"
                "statement_timeout = '10s'\nlock_timeout = '3s'\n"
            )
        ctl = self.pg_bin / "pg_ctl"
        try:
            self.run(ctl, "-D", str(self.data), "-l", str(self.log), "-w", "-t", "15", "start")
            yield
        finally:
            if (self.data / "postmaster.pid").exists():
                self.run(ctl, "-D", str(self.data), "-w", "-t", "15", "-m", "immediate", "stop")

    def sql(self, database, statement):
        return self.run(
            self.pg_bin / "psql", "-X", "-At", "--no-password", "-v", "ON_ERROR_STOP=1",
            "-h", str(self.scratch), "-p", str(self.port), "-U", ROLE, "-d", database,
            "-c", statement,
        ).stdout.strip()

    def create_project(self, case):
        self.sql("postgres", f'CREATE DATABASE "{case}"')
        root = self.scratch / case
        root.mkdir()
        return root

    def migration(self, project, filename, content):
        self.run(self.cli, "migration", "new", "dmg_fixture", cwd=project)
        directory = project / "supabase" / "migrations"
        generated, = directory.glob("*_dmg_fixture.sql")
        require(GENERATED_NAME.fullmatch(generated.name), "unexpected generated migration path")
        staged = directory / filename
        generated.rename(staged)
        staged.write_bytes(content if isinstance(content, bytes) else content.encode("utf-8"))
        return staged

    def migrate(self, project, command, *flags, check=True):
        url = f"postgresql://{ROLE}:{self.password}@127.0.0.1:{self.port}/{project.name}?sslmode=disable"
        return self.run(
            self.cli, "migration", command, f"--remote={url}", "--output-format", "json",
            *flags, cwd=project, check=check,
        )

    def apply(self, project, check=True):
        return self.migrate(project, "up", "--yes", check=check)

    def history(self, project):
        return [
            json.loads(row)
            for row in self.sql(project.name, f"SELECT row_to_json(h) FROM {HISTORY} h ORDER BY version").splitlines()
        ]

    def columns(self, project):
        return self.sql(
            project.name,
            "SELECT attname FROM pg_attribute "
            "WHERE attrelid = 'public.probe'::regclass AND attnum > 0 AND NOT attisdropped "
            "ORDER BY attnum",
        ).splitlines()

    def valid_index(self, project):
        return self.sql(
            project.name, "SELECT coalesce(bool_and(indisvalid), false) FROM pg_index "
            "WHERE indexrelid = to_regclass('public.probe_first_idx')",
        ) == "t"

    def sqlstates_after(self, offset):
        with self.log.open("rb") as log:
            log.seek(offset)
            return SQLSTATE.findall(log.read().decode("utf-8"))


def applied_filenames(result):
    return [Path(path).name for path in json.loads(result.stdout)["applied"]]


def discovery(probe):
    project = probe.create_project("lexical")
    first = probe.migration(project, "01_first.sql", BASE + "\n")
    probe.migration(project, "1_add_first.sql", ADD_FIRST + "\n")
    probe.migration(project, "10_add_second.sql", ADD_SECOND + "\n")
    probe.migration(project, "2_looks.down.sql", "ALTER TABLE public.probe ADD COLUMN last_value text;\n")
    directory = first.parent
    (directory / "misc.sql").write_text(DIVIDE, encoding="utf-8")
    (directory / "3_upper.SQL").write_text(DIVIDE, encoding="utf-8")
    (directory / "4_directory.sql").mkdir()
    (directory / "4_directory.sql" / "3_nested.sql").write_text(DIVIDE, encoding="utf-8")

    applied = applied_filenames(probe.apply(project))
    versions = ["01", "1", "10", "2"]
    require(applied == ["01_first.sql", "1_add_first.sql", "10_add_second.sql", "2_looks.down.sql"],
            f"unexpected apply order: {applied}")
    history = probe.history(project)
    listed = json.loads(probe.migrate(project, "list").stdout)["migrations"]
    require([row["local"] for row in listed] == [row["remote"] for row in listed] == versions,
            f"unexpected listed versions: {listed}")
    require([row["version"] for row in history] == versions, f"unexpected native history: {history}")
    require(all(set(row) == {"version", "name", "statements"} for row in history), "unexpected history shape")
    require(history[0]["statements"] == [BASE.removesuffix(";")], "expected statement text without semicolon")

    original_digest = file_sha256(first)
    first.write_bytes((BASE + "\n" + DIVIDE + "\n").encode("utf-8"))
    modified_digest = file_sha256(first)
    reapplied = applied_filenames(probe.apply(project))
    require(original_digest != modified_digest and not reapplied and probe.history(project) == history,
            "existing version was reapplied or changed history")
    columns = probe.columns(project)
    require(columns == ["id", "first_value", "second_value", "last_value"], f"unexpected columns: {columns}")
    return {
        "case": project.name, "applied": applied, "versions": versions, "columns": columns,
        "history_fields": sorted(history[0]), "history_first_statements": history[0]["statements"],
        "original_sha256": original_digest, "edited_sha256": modified_digest, "reapplied": reapplied,
        "generated_filename_pattern": GENERATED_NAME.pattern,
    }


def legacy_init(probe, first_is_legacy):
    case = "legacy_first" if first_is_legacy else "legacy_later"
    project = probe.create_project(case)
    legacy = "20211208000000_init.sql"
    cutoff = "20211209000000_init.sql"
    old = probe.migration(project, legacy, "CREATE TABLE public.legacy_probe (id integer);\n")
    probe.migration(project, cutoff, BASE + "\n")
    if not first_is_legacy:
        (old.parent / "!readme").write_text("ignored\n", encoding="ascii")
    applied = applied_filenames(probe.apply(project))
    expected = [cutoff] if first_is_legacy else [legacy, cutoff]
    require(applied == expected, f"unexpected init handling in {case}: {applied}")
    return {"case": case, "applied": applied}


def duplicate(probe):
    project = probe.create_project("duplicate")
    probe.migration(project, "01_first.sql", BASE + "\n")
    probe.migration(project, "01_second.sql", ADD_FIRST + "\n")
    offset = probe.log.stat().st_size
    result = probe.apply(project, check=False)
    states = probe.sqlstates_after(offset)
    history = probe.history(project)
    columns = probe.columns(project)
    require(result.returncode == 1 and states == ["42P01", "23505"] and columns == ["id"],
            f"unexpected duplicate handling: exit {result.returncode}, SQLSTATE {states}, {columns}")
    require([(row["version"], row["name"]) for row in history] == [("01", "first")],
            f"unexpected duplicate history: {history}")
    return {"case": project.name, "exit": result.returncode, "sqlstate": states[-1], "columns": columns,
            "initial_history_lookup_sqlstate": states[0], "history_versions": [row["version"] for row in history]}


def failing_candidate(probe, case, content, sqlstate, first_survives=False, index_survives=False):
    project = probe.create_project(case)
    probe.migration(project, "01_base.sql", BASE + "\n")
    require(applied_filenames(probe.apply(project)) == ["01_base.sql"], f"baseline failed for {case}")
    migration = probe.migration(project, "02_candidate.sql", content)
    offset = probe.log.stat().st_size
    result = probe.apply(project, check=False)
    states = probe.sqlstates_after(offset)
    columns = probe.columns(project)
    index = probe.valid_index(project)
    versions = [row["version"] for row in probe.history(project)]
    expected_columns = ["id", "first_value"] if first_survives else ["id"]
    require(
        result.returncode == 1 and states == [sqlstate] and columns == expected_columns
        and index == index_survives and versions == ["01"],
        f"unexpected {case}: exit {result.returncode}, SQLSTATE {states}, columns {columns}, "
        f"valid index {index}, history {versions}",
    )
    return {
        "case": case, "exit": result.returncode, "sqlstate": states[0], "columns": columns,
        "valid_index": index, "history_versions": versions, "candidate_sha256": file_sha256(migration),
    }


def observe(probe):
    yield discovery(probe)
    yield legacy_init(probe, True)
    yield legacy_init(probe, False)
    yield duplicate(probe)
    ordinary = f"{ADD_FIRST}\n{DIVIDE}\n"
    yield failing_candidate(probe, "ordinary_batch", ordinary, "22012")
    yield failing_candidate(probe, "exact_opt_out", f"{OPT_OUT}\n{ordinary}", "22012", first_survives=True)
    bom = "\ufeff" + f"{OPT_OUT}\n{ordinary}".replace("\n", "\r\n")
    yield failing_candidate(probe, "bom_crlf_opt_out", bom, "22012", first_survives=True)
    yield failing_candidate(probe, "late_opt_out", f"\n{OPT_OUT}\n{ordinary}", "22012")
    yield failing_candidate(probe, "near_opt_out", f"{OPT_OUT} \n{ordinary}", "22012")

    index = "/* leading comment */ CREATE\nUNIQUE INDEX CONCURRENTLY probe_first_idx ON public.probe (first_value);"
    yield failing_candidate(
        probe, "automatic_split", f"{ADD_FIRST}\n{index}\n{ADD_SECOND}\n{DIVIDE}\n", "22012",
        first_survives=True, index_survives=True,
    )
    yield failing_candidate(
        probe, "vacuum_split", f"{ADD_FIRST}\nVACUUM (ANALYZE) public.probe;\n{ADD_SECOND}\n{DIVIDE}\n",
        "22012", first_survives=True,
    )
    internal_comment = "CREATE /* internal comment */ INDEX CONCURRENTLY probe_first_idx ON public.probe (first_value);"
    yield failing_candidate(probe, "internal_comment", f"{ADD_FIRST}\n{internal_comment}\n", "25001")
    yield failing_candidate(
        probe, "explicit_transaction",
        f"{ADD_FIRST}\nBEGIN;\n{ADD_SECOND}\n{index}\nCOMMIT;\n", "25001", first_survives=True,
    )
    yield failing_candidate(
        probe, "implicit_savepoint",
        f"{ADD_FIRST}\nSAVEPOINT before_add;\nROLLBACK TO SAVEPOINT before_add;\n{ADD_SECOND}\n",
        "25P01",
    )


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--supabase", required=True, type=Path, help="Supabase CLI 3.0.0 executable")
    parser.add_argument("--pg-bin", required=True, type=Path, help="PostgreSQL 17.12 or 18.7 bin directory")
    args = parser.parse_args()
    cli = args.supabase.resolve(strict=True)
    pg_bin = args.pg_bin.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dmg-supabase-") as directory:
        probe = Probe(cli, pg_bin, Path(directory))
        version = probe.run(cli, "--version").stdout.strip()
        postgres = probe.run(pg_bin / "postgres", "--version").stdout.strip()
        require(version == CLI_VERSION, f"expected Supabase CLI {CLI_VERSION}, observed {version}")
        require(postgres in {"postgres (PostgreSQL) 17.12", "postgres (PostgreSQL) 18.7"},
                f"unexpected PostgreSQL version: {postgres}")
        with probe.running():
            cases = list(observe(probe))
        print(json.dumps({"supabase": version, "supabase_binary_sha256": file_sha256(cli),
                          "postgres": postgres, "postgres_binary_sha256": file_sha256(pg_bin / "postgres")},
                         sort_keys=True))
        for case in cases:
            print(json.dumps(case, sort_keys=True))


if __name__ == "__main__":
    main()
