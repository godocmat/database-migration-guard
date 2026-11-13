"""Observe migration locks and concurrent writes on a disposable PostgreSQL cluster."""

from __future__ import annotations

import argparse
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile
import time

from corpus import ROOT, read_corpus, require, variants
from native_probe import Cluster, ERROR, command_env, execute


CASES = {
    "R04a": "AccessExclusiveLock",
    "R04b": "AccessExclusiveLock",
    "R04c": "ShareUpdateExclusiveLock",
    "R05": "ShareLock",
    "V02": "ShareRowExclusiveLock",
}
WRITES = {
    "app.accounts": "INSERT INTO app.accounts (id, email) VALUES (71, 'lock-probe@example.invalid')",
    "app.events": "INSERT INTO app.events (id, account_id) VALUES (71, 1)",
}


@contextmanager
def hold_ddl(cluster, database, source):
    sql = source.read_bytes()
    with tempfile.TemporaryFile(mode="w+b") as errors:
        session = subprocess.Popen(cluster.psql_command(database, "-q", "-f", "-"),
                                   env=command_env(), stdin=subprocess.PIPE,
                                   stdout=subprocess.DEVNULL, stderr=errors)
        try:
            session.stdin.write(b"SET idle_in_transaction_session_timeout = '10s';\n"
                                b"SET application_name = 'dmg_lock_ddl';\nBEGIN;\n"
                                + sql + b"\nSELECT 'dmg-lock-ready';\n")
            session.stdin.flush()
            deadline = time.monotonic() + 5
            while session.poll() is None and time.monotonic() < deadline:
                if cluster.query(database, "SELECT EXISTS (SELECT 1 FROM pg_stat_activity "
                                 "WHERE datname = current_database() "
                                 "AND application_name = 'dmg_lock_ddl' AND state = 'idle in transaction' "
                                 "AND query LIKE $$SELECT 'dmg-lock-ready'%$$)") == "t":
                    break
                time.sleep(0.05)
            else:
                errors.seek(0)
                raise ValueError(f"DDL session did not reach the hold point for {source.name}\n"
                                 f"{errors.read().decode('utf-8', errors='replace')}")
            yield
        finally:
            session.stdin.close()
            try:
                session.wait(timeout=3)
            except subprocess.TimeoutExpired:
                session.kill()
                session.wait(timeout=3)


def probe(cluster, manifest, label, variant, profile, major, number):
    case = label.split("/")[0]
    database = f"locks_{number}"
    cluster.query("postgres", f"CREATE DATABASE {database}")
    baseline = manifest["baselines"][profile["baseline"]]
    seed = manifest["seeds"]["clean"]
    extra = variant.get("baseline_extra", [])
    for source in [*(source for _, source in baseline), *seed, *(source for _, source in extra)]:
        cluster.psql(database, "--single-transaction", "--file", str(ROOT / source))
    require(len(variant["write"]) == 1, f"{case} needs one candidate file")
    source = ROOT / variant["write"][0][1]
    record = {"id": f"{label}/pg{major}", "source": source.relative_to(ROOT).as_posix()}
    if major == "17" and case == "V02":
        result = cluster.psql(database, "--single-transaction", "--file", str(source), check=False)
        error = ERROR.search(result.stderr)
        require(result.returncode != 0 and error and error.group(1) == "42809",
                f"unexpected result for {record['id']}\n{result.stderr}")
        return {**record, "migration_sqlstate": "42809"}

    with hold_ddl(cluster, database, source):
        observed = cluster.query(database, "SELECT n.nspname || '.' || c.relname, l.mode "
                                 "FROM pg_locks l JOIN pg_stat_activity a ON a.pid = l.pid "
                                 "JOIN pg_class c ON c.oid = l.relation "
                                 "JOIN pg_namespace n ON n.oid = c.relnamespace "
                                 "WHERE a.application_name = 'dmg_lock_ddl' "
                                 "AND a.datname = current_database() AND l.database = "
                                 "(SELECT oid FROM pg_database WHERE datname = current_database()) "
                                 "AND l.granted AND n.nspname = 'app' AND c.relkind IN ('r', 'p') "
                                 "ORDER BY 1, 2")
        locks = [line.split("|") for line in observed.splitlines()]
        tables = ["app.accounts", "app.events"] if case == "V02" else ["app.accounts"]
        expected_lock = CASES[case]
        require(all([table, expected_lock] in locks for table in tables),
                f"unexpected locks for {record['id']}: {locks}")
        writer_sqlstate = {}
        expected_state = None if case == "R04c" else "55P03"
        for table in tables:
            result = cluster.psql(database, "-c", "BEGIN; SET LOCAL lock_timeout = '500ms'; "
                                  f"{WRITES[table]}; ROLLBACK;", check=False)
            error = ERROR.search(result.stderr)
            state = error.group(1) if error else None
            require(state == expected_state and bool(result.returncode) == bool(expected_state),
                    f"unexpected concurrent write result for {record['id']} on {table}\n{result.stderr}")
            writer_sqlstate[table] = state
    return {**record, "migration_sqlstate": None, "relation_locks": locks, "writer_sqlstate": writer_sqlstate}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pg-bin", type=Path, required=True, help="directory of pinned PostgreSQL binaries")
    args = parser.parse_args()
    pg_bin = args.pg_bin.resolve()
    manifest, _ = read_corpus()
    version = execute(pg_bin / "postgres", "--version").stdout.strip().split()[-1]
    major = version.split(".")[0]
    require(manifest["postgres"].get(major) == version, f"unexpected PostgreSQL {version}")
    psql = execute(pg_bin / "psql", "--version").stdout.strip()
    require(psql.split()[-1] == version, f"psql version differs from PostgreSQL {version}")
    cases = [(label, variant) for label, variant, profile in variants(manifest)
             if profile == "plain" and label.split("/")[0] in CASES]
    require(len(cases) == len(CASES), "unexpected lock probe recipes")
    with tempfile.TemporaryDirectory(prefix="dmg02-locks-", dir="/tmp") as directory:
        cluster = Cluster(pg_bin, None, Path(directory))
        with cluster.running():
            header = {"server_version": cluster.query("postgres", "SHOW server_version"),
                      "psql": psql, "runs": len(cases),
                      "sha256": {name: hashlib.sha256(path.read_bytes()).hexdigest()
                                 for name, path in {"postgres": pg_bin / "postgres", "psql": pg_bin / "psql",
                                                    "manifest": ROOT / "manifest.json",
                                                    "input_digests": ROOT / "sha256.json"}.items()}}
            rows = [probe(cluster, manifest, label, variant, manifest["profiles"]["plain"], major, number)
                    for number, (label, variant) in enumerate(cases, 1)]
    for observation in [header, *rows]:
        print(json.dumps(observation, sort_keys=True))


if __name__ == "__main__":
    main()
