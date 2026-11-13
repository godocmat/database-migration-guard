"""Run selected development fixtures on a new Unix-socket-only PostgreSQL cluster."""

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

from corpus import ROOT, file_map, inventory, read_corpus, require, variants, verify_modes, write_inputs


CASES = {"D01", "D02", "D04", "R03a", "R03b", "R04c", "T01", "T02", "T03", "T04",
         "S01", "S02", "S03", "V01", "V02"}
USER = "dmg_fixture"
PORT = 5432
ERROR = re.compile(r"ERROR:\s+([A-Z0-9]{5}): ([^\n]+)")


def command_env():
    env = {k: v for k, v in os.environ.items() if not k.startswith(("PG", "DATABASE_URL"))}
    env.update(LC_ALL="C", PGCLIENTENCODING="UTF8", PGCONNECT_TIMEOUT="5")
    return env


def execute(program, *args, cwd=None, check=True):
    result = subprocess.run([str(program), *args], cwd=cwd, env=command_env(), capture_output=True,
                            text=True, encoding="utf-8", timeout=30)
    require(not check or result.returncode == 0,
            f"{Path(program).name} failed with {result.returncode}\n{result.stdout}{result.stderr}")
    return result


class Cluster:
    def __init__(self, pg_bin, sqlx, directory):
        self.bin = pg_bin
        self.sqlx = sqlx
        self.directory = directory
        self.log = directory / "postgres.log"

    @contextmanager
    def running(self):
        data = self.directory / "data"
        execute(self.bin / "initdb", "-D", str(data), "--auth=trust", "--no-locale",
                "--encoding=UTF8", "--username", USER)
        with (data / "postgresql.conf").open("a", encoding="utf-8") as config:
            config.write(f"\nlisten_addresses = ''\nport = {PORT}\n"
                         f"unix_socket_directories = '{self.directory}'\n"
                         "log_error_verbosity = verbose\nlog_min_error_statement = error\n"
                         "statement_timeout = '8s'\nlock_timeout = '2s'\n")
        try:
            execute(self.bin / "pg_ctl", "-D", str(data), "-l", str(self.log), "-w", "-t", "15", "start")
            yield
        finally:
            if (data / "postmaster.pid").exists():
                execute(self.bin / "pg_ctl", "-D", str(data), "-w", "-t", "15", "-m", "immediate", "stop")

    def psql_command(self, database, *args):
        return [self.bin / "psql", "-X", "--no-password", "-h", str(self.directory),
                "-p", str(PORT), "-U", USER, "-d", database, "-v", "ON_ERROR_STOP=1",
                "-v", "VERBOSITY=verbose", *args]

    def psql(self, database, *args, cwd=None, check=True):
        return execute(*self.psql_command(database, *args), cwd=cwd, check=check)

    def query(self, database, sql):
        return self.psql(database, "-At", "-c", sql).stdout.strip()

    def run_sqlx(self, database, source, work, *args, check=True):
        params = urlencode({"host": str(self.directory), "port": PORT, "user": USER, "sslmode": "disable"})
        return execute(self.sqlx, "migrate", "run", "--source", source, "--no-dotenv",
                       "--connect-timeout", "5", "--database-url", f"postgres:///{database}?{params}",
                       *args, cwd=work, check=check)

    def new_log(self, offset):
        with self.log.open("rb") as log:
            log.seek(offset)
            return log.read().decode("utf-8")


def column_exists(cluster, db, table, column):
    return cluster.query(db, "SELECT EXISTS (SELECT 1 FROM information_schema.columns "
                         f"WHERE table_schema='app' AND table_name='{table}' AND column_name='{column}')") == "t"


def constraint_status(cluster, db, table, constraint):
    return cluster.query(db, "SELECT COALESCE((SELECT convalidated::text FROM pg_constraint "
                         f"WHERE conrelid='app.{table}'::regclass AND conname='{constraint}'), 'absent')")


def file_node(cluster, db):
    return int(cluster.query(db, "SELECT pg_relation_filenode('app.accounts')"))


def seed_rows(cluster, db):
    return {table: cluster.query(db, f"SELECT row_to_json(t) FROM app.{table} AS t ORDER BY id").splitlines()
            for table in ("accounts", "events")}


def plain_apply(cluster, db, migration, work):
    wrapper = ["--single-transaction"] if migration["mode"] == "psql-file-transaction" else []
    return cluster.psql(db, *wrapper, "--file", migration["path"], cwd=work, check=False)


def expected_sqlstate(case, target, seed):
    if case in {"T01", "T03", "T04"}:
        return "25001"
    if case in {"D02", "S02"}:
        return {"D02": "42P01", "S02": "42601"}[case]
    if case == "R04c" and seed == "null-email":
        return "23514"
    if target == "17":
        return {"V01": "42601", "V02": "42809"}.get(case)
    return None


def observe_version_case(cluster, db, case, seed, succeeded):
    if case == "V01":
        table, name, violation, sqlstate = "accounts", "accounts_email_nn", "email IS NULL", "23502"
        bad_insert = "INSERT INTO app.accounts (id, email) VALUES (5, NULL)"
        repair = "UPDATE app.accounts SET email = 'repaired@example.invalid' WHERE id = 4"
    else:
        table, name, violation, sqlstate = "events", "events_account_fk", "account_id = 999", "23503"
        bad_insert = "INSERT INTO app.events VALUES (5, 999)"
        repair = "UPDATE app.events SET account_id = 1 WHERE id = 4"
    state = constraint_status(cluster, db, table, name)
    remaining = int(cluster.query(db, f"SELECT count(*) FROM app.{table} WHERE {violation}"))
    require(state == ("false" if succeeded else "absent") and remaining == int(seed != "clean"),
            f"unexpected post-add state for {case}")
    observations = {"constraint_after_add": state, "violations_after_add": remaining}
    if succeeded:
        validation = f"ALTER TABLE app.{table} VALIDATE CONSTRAINT {name}"
        insert = cluster.psql(db, "-c", bad_insert, check=False)
        validate = cluster.psql(db, "-c", validation, check=False)
        insert_error = ERROR.search(insert.stderr)
        validation_error = ERROR.search(validate.stderr)
        require(insert.returncode != 0 and insert_error and insert_error.group(1) == sqlstate,
                f"{case} accepted a new violation or returned an unexpected error")
        first_validation_state = validation_error.group(1) if validation_error else None
        require((validate.returncode == 0 and first_validation_state is None) if seed == "clean"
                else (validate.returncode != 0 and first_validation_state == sqlstate),
                f"{case} returned an unexpected validation result")
        if first_validation_state:
            cluster.query(db, repair)
            cluster.query(db, validation)
        observations.update(new_violation_sqlstate=insert_error.group(1),
                            initial_validation_sqlstate=first_validation_state,
                            constraint_final=constraint_status(cluster, db, table, name))
        require(observations["constraint_final"] == "true", f"{case} never validated")
    return observations


def observe_schema(cluster, db, case, seed, succeeded, before):
    if case.startswith("T"):
        index = "accounts_code_copy_idx" if case == "T04" else "accounts_legacy_idx"
        state = cluster.query(db, "SELECT COALESCE((SELECT indisvalid::text FROM pg_index "
                              f"WHERE indexrelid=to_regclass('app.{index}')), 'absent')")
        require(state == ("true" if case == "T02" else "absent"), f"{case} has index state {state}")
        observed = {"index_valid": state}
        if case == "T04":
            observed["code_copy_exists"] = column_exists(cluster, db, "accounts", "code_copy")
            require(not observed["code_copy_exists"], "T04 did not roll back the earlier statement")
        return observed
    if case in {"D01", "D02"}:
        flag = column_exists(cluster, db, "probe", "flag")
        ignored = cluster.query(db, "SELECT to_regclass('app.ignored_sentinel') IS NOT NULL") == "t"
        require(flag == (case == "D01") and not ignored, f"{case} probe table or ignored path mismatch")
        return {"probe_flag_exists": flag, "ignored_sentinel_exists": ignored}
    if case in {"D04", "S03"}:
        label = column_exists(cluster, db, "accounts", "label")
        require(label, f"{case} did not create the label column")
        return {"label_exists": label}
    if case in {"S01", "S02"}:
        legacy = column_exists(cluster, db, "accounts", "legacy_code")
        require(not legacy, f"{case} retained legacy_code")
        observed = {"legacy_code_exists": legacy}
        if case == "S01":
            observed["note"] = cluster.query(db, "SELECT DISTINCT note FROM app.accounts")
            observed["label_exists"] = column_exists(cluster, db, "accounts", "label")
            require(observed["note"] == "é;🧪" and observed["label_exists"], "S01 source content changed")
        return observed
    if case in {"R03a", "R03b"}:
        before_node = before["heap"]
        after_node = file_node(cluster, db)
        require((after_node != before_node) == (case == "R03b"), "unexpected heap file identity")
        return {"heap_before": before_node, "heap_after": after_node}
    if case == "R04c":
        after_check = constraint_status(cluster, db, "accounts", "accounts_email_present")
        before_check = before["check"]
        after_rows = seed_rows(cluster, db)
        rows = len(after_rows["accounts"])
        require(before_check == "false" and after_check == ("true" if succeeded else "false"),
                "unexpected CHECK validation state")
        require(rows == (3 if seed == "clean" else 4) and after_rows == before["rows"],
                "rehearsal changed seed rows")
        return {"check_before": before_check, "check_after": after_check, "account_rows": rows,
                "seed_rows_unchanged": True}
    return observe_version_case(cluster, db, case, seed, succeeded)


def probe(cluster, manifest, inputs, label, variant, profile_id, target, seed, number):
    case = label.split("/")[0]
    profile = manifest["profiles"][profile_id]
    root = manifest["migration_root"]
    work = cluster.directory / f"plan-{number}"
    base = file_map(manifest["baselines"][profile["baseline"]] + variant.get("baseline_extra", []), root)
    writes = file_map(variant["write"], root)
    require(not variant.get("remove") and not base.keys() & writes.keys(), "probe needs an append-only plan")
    files = {p: inputs[source] for p, source in {**base, **writes}.items()}
    write_inputs(work, {**base, **writes}, inputs)
    migrations, _ = inventory(files, profile, root)
    verify_modes(manifest, profile_id, variant, migrations, files)
    forward = [m for m in migrations if m["direction"] != "down"]
    baseline = [m for m in forward if m["path"] in base]
    pending = [m for m in forward if m["path"] in writes]
    require([m["version"] for m in pending] == variant["discovery"]["selected"],
            "probe would run a different forward selection")

    db = f"probe_{number}"
    cluster.query("postgres", f"CREATE DATABASE {db}")
    sqlx = profile["adapter"] == "sqlx"
    if sqlx:
        cluster.run_sqlx(db, root, work, "--target-version", "1")
    else:
        require(plain_apply(cluster, db, baseline[0], work).returncode == 0, "base migration failed")
    for source in manifest["seeds"][seed]:
        cluster.psql(db, "--single-transaction", "--file", str(ROOT / source))
    if len(baseline) > 1:
        if sqlx:
            cluster.run_sqlx(db, root, work, "--target-version", str(baseline[-1]["version"]))
        else:
            for migration in baseline[1:]:
                require(plain_apply(cluster, db, migration, work).returncode == 0, "extra base migration failed")

    before = {}
    if case.startswith("R03"):
        before["heap"] = file_node(cluster, db)
    if case == "R04c":
        before.update(check=constraint_status(cluster, db, "accounts", "accounts_email_present"),
                      rows=seed_rows(cluster, db))
    offset = cluster.log.stat().st_size
    if sqlx:
        runner = cluster.run_sqlx(db, root, work, check=False)
        applied = [int(v) for v in re.findall(r"(?m)^Applied (\d+)/", runner.stdout)]
    else:
        applied = []
        for migration in pending:
            runner = plain_apply(cluster, db, migration, work)
            if runner.returncode != 0:
                break
            applied.append(migration["version"])
    server_log = cluster.new_log(offset)
    error = ERROR.search(server_log)
    sqlstate = error.group(1) if error else None
    expected_state = expected_sqlstate(case, target, seed)
    require(sqlstate == expected_state and (runner.returncode != 0) == bool(expected_state),
            f"{label} {seed} expected {expected_state}\n{runner.stdout}{runner.stderr}{server_log}")
    expected_applied = [m["version"] for m in pending] if not expected_state else [2] if case == "S02" else []
    require(applied == expected_applied, f"{label} ran {applied}, expected {expected_applied}\n{runner.stdout}")

    observed = {"id": f"{label}/pg{target}", "seed": seed, "runner_exit": runner.returncode,
                "applied_candidate_order": applied, "sqlstate": sqlstate,
                "schema": observe_schema(cluster, db, case, seed, not expected_state, before)}
    if case == "T04":
        batch = files[pending[0]["path"]].decode("utf-8").rstrip("\n")
        observed["server_logged_batch"] = batch in server_log.replace("\n\t", "\n")
        require(observed["server_logged_batch"], "server did not log the complete T04 batch")
    if error:
        observed["server_error"] = error.group(2)
        output = (runner.stderr or runner.stdout).strip().splitlines()
        if output:
            observed["runner_error"] = output[-1] if sqlx else output[0]
    if sqlx:
        rows = cluster.query(db, "SELECT version, success, encode(checksum, 'hex') "
                             "FROM _sqlx_migrations ORDER BY version")
        history = [line.split("|") for line in rows.splitlines()]
        observed["sqlx_history"] = [{"version": int(v), "success": success == "t", "sha384": sha}
                                    for v, success, sha in history]
        expected_history = [{"version": m["version"], "success": True,
                             "sha384": hashlib.sha384(files[m["path"]]).hexdigest()}
                            for m in forward if m in baseline or m["version"] in applied]
        require(observed["sqlx_history"] == expected_history, f"{label} native history differs")
    return observed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pg-bin", type=Path, required=True, help="directory of pinned PostgreSQL binaries")
    parser.add_argument("--sqlx", type=Path, required=True, help="pinned SQLx CLI executable")
    parser.add_argument("--case", choices=sorted(CASES), action="append", help="limit to these case IDs")
    args = parser.parse_args()
    pg_bin, sqlx = args.pg_bin.resolve(), args.sqlx.resolve()
    manifest, inputs = read_corpus()
    version = execute(pg_bin / "postgres", "--version").stdout.strip().split()[-1]
    target = version.split(".")[0]
    require(manifest["postgres"].get(target) == version, f"unexpected PostgreSQL {version}")
    psql = execute(pg_bin / "psql", "--version").stdout.strip()
    require(psql.split()[-1] == version, f"psql version differs from PostgreSQL {version}")
    sqlx_version = execute(sqlx, "--version").stdout.strip()
    require(sqlx_version.split()[-1] == manifest["sqlx_version"], f"unexpected {sqlx_version}")
    records = []
    with tempfile.TemporaryDirectory(prefix="dmg02-pg-", dir="/tmp") as tmp:
        cluster = Cluster(pg_bin, sqlx, Path(tmp))
        with cluster.running():
            cases = set(args.case) if args.case else CASES
            for label, variant, profile in variants(manifest):
                if label.split("/")[0] in cases:
                    for seed in variant.get("seeds", ["clean"]):
                        records.append(probe(cluster, manifest, inputs, label, variant, profile, target,
                                             seed, len(records) + 1))
            header = {"server_version": cluster.query("postgres", "SHOW server_version"),
                      "server_version_num": int(cluster.query("postgres", "SHOW server_version_num")),
                      "psql": psql, "sqlx": sqlx_version, "runs": len(records),
                      "sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest()
                                 for name, path in {"postgres": pg_bin / "postgres", "psql": pg_bin / "psql",
                                                    "sqlx": sqlx, "manifest": ROOT / "manifest.json",
                                                    "input_digests": ROOT / "sha256.json"}.items()}}
    for observation in [header, *records]:
        print(json.dumps(observation, ensure_ascii=True, sort_keys=True))


if __name__ == "__main__":
    main()
