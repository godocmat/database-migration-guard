"""Run the local competitor pilot on the public development corpus."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
import re
import tempfile
import time
from urllib.parse import urlencode

from corpus import git, materialize, read_corpus, require
from native_probe import Cluster, PORT, USER, execute, plain_apply


CASES = (
    "R01/drop-column", "R01/truncate", "R03a/nullable-add", "R03a/constant-default",
    "R03b/volatile-default", "R05/regular-index", "R07/new-relation",
)
SQUAWK_SHA256 = "b4cda2069a3ca5b75b47f025b569df9e6a762262b9258bc4db3f43381f8544d8"
ATLAS_SHA256 = "10d7913e3dce43ab99b8d71534a4cbadaf11a16dc293adf3b91d10e83a0ac70b"
PROJECT = r"""schema_version = 1
target = {target}
roots = ["migrations"]
search_path = ["public"]

[adapter]
kind = "plain"
filename_pattern = '^([0-9]+)_[a-z][a-z0-9_]*\.sql$'
version_order = "positive_integer"
transaction = "file"
"""
POLICY = """schema_version = 1
deny_at = "high"

[integrity]
require_git_base = true
deny_modified = true
deny_missing = true
"""
ATLAS_INDEX_POLICY = """lint {
  concurrent_index {
    error = true
  }
}

env "pilot" {}
"""
TIMEOUT_RULES = "require-lock-timeout,require-statement-timeout"
LOG_PREFIX = "db=%d tx=%x "


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def binary_pin(path, version_arg="--version", expected_digest=None):
    digest = sha256(path.read_bytes())
    require(expected_digest is None or digest == expected_digest, f"{path.name} SHA-256 differed from the release")
    return {"version": execute(path, version_arg).stdout.splitlines()[0], "sha256": digest}


def run(tool, *args, cwd):
    start = time.monotonic()
    result = execute(tool, *args, cwd=cwd, check=False)
    return result.stdout, {"exit": result.returncode, "stderr": result.stderr.strip(),
                           "elapsed_ms": round(1000 * (time.monotonic() - start), 1)}


def guard_check(dmg, metadata, repo, config, policy):
    commits = metadata["git"]
    stdout, result = run(dmg, "check", "--repo", ".", "--config", str(config), "--policy", str(policy),
                         "--base", commits["base"], "--head", commits["head"], "--format", "json", cwd=repo)
    report = json.loads(stdout)
    require(report["target"] == metadata["target_major"], "Guard target differed")
    require(report["migrations"]["selected_forwards"] == metadata["selected_forward_paths"],
            "Guard selection differed")
    inventory = report["migrations"]["inventory"]
    require([(m["path"], m["digest"], m["execution"]["transaction_wrapper"]) for m in inventory]
            == [(m["path"], metadata["files"]["candidate"][m["path"]]["sha256"], "file")
                for m in metadata["candidate"]], "Guard inventory or runner differed")
    return {"tool": "guard", **result, "selected": report["migrations"]["selected_forwards"],
            "report_version": report["schema_version"], "ruleset_version": report["ruleset_version"],
            "coverage": report["coverage"], "decision": report["decision"],
            "findings": report["findings"], "relation_locks": report.get("relation_locks", [])}


def squawk_check(squawk, metadata, repo, config, policy_pass, *options):
    paths = metadata["selected_forward_paths"]
    stdout, result = run(squawk, "--config", str(config), "--pg-version", metadata["target_version"],
                         "--assume-in-transaction", "--reporter", "json", *options, *paths, cwd=repo)
    findings = json.loads(stdout)
    require(all(f["file"].replace("\\", "/") in paths for f in findings), "Squawk location outside selection")
    return {"tool": "squawk", "policy_pass": policy_pass, **result, "harness_selected": paths, "findings": findings}


def log_replay(cluster, offset, database):
    pattern = (rf"^db={database} tx=(\d+) LOG:\s+00000: statement: "
               r"(CREATE SCHEMA app|CREATE TABLE app\.[a-z0-9_]+|ALTER TABLE app\.[a-z0-9_]+|"
               r"CREATE INDEX [a-z0-9_]+|TRUNCATE(?: TABLE)? app\.[a-z0-9_]+|BEGIN|COMMIT)")
    return [{"statement_start": stmt, "transaction_id_assigned": tx != "0"}
            for tx, stmt in re.findall(pattern, cluster.new_log(offset), re.MULTILINE)]


def fresh_database(cluster, database):
    cluster.query("postgres", f"DROP DATABASE IF EXISTS {database}")
    cluster.query("postgres", f"CREATE DATABASE {database}")
    cluster.query("postgres", f"ALTER DATABASE {database} SET log_statement = 'all'")
    return cluster.log.stat().st_size


def atlas_check(atlas, cluster, metadata, repo, selection, policy_pass="default", options=()):
    database = "atlas_dev"
    offset = fresh_database(cluster, database)
    params = urlencode({"host": str(cluster.directory), "port": PORT, "user": USER, "sslmode": "disable"})
    stdout, result = run(atlas, "migrate", "lint", "--dir", "file://migrations",
                         "--dev-url", f"postgres:///{database}?{params}", "--format", "{{ json . }}",
                         *selection, *options, cwd=repo)
    report = json.loads(stdout)
    selected = []
    for file in report["Files"]:
        path = f"migrations/{file['Name']}"
        digest = sha256(file.pop("Text").encode("utf-8"))
        require(digest == metadata["files"]["candidate"][path]["sha256"], "Atlas reported different bytes")
        file["sha256"] = digest
        selected.append(path)
    require(selected == metadata["selected_forward_paths"], "Atlas selection differed")
    replay = log_replay(cluster, offset, database)
    require([row["statement_start"] for row in replay[:4]] == [
        "CREATE SCHEMA app", "CREATE TABLE app.accounts", "CREATE TABLE app.events",
        "CREATE TABLE app.events_1_999"], f"Atlas replayed a different baseline or order. {replay!r}")
    return {"tool": "atlas_community", "policy_pass": policy_pass, "selection": selection[0], **result,
            "selected": selected,
            "steps": [{"Name": step["Name"], "Text": step["Text"]} for step in report["Steps"]],
            "files": report["Files"], "replay": replay}


def native_file_control(cluster, metadata, repo):
    database = "native_file"
    offset = fresh_database(cluster, database)
    exits = [plain_apply(cluster, database, m, repo).returncode for m in metadata["candidate"]]
    require(exits == [0, 0], "psql file-transaction control failed")
    replay = log_replay(cluster, offset, database)
    require(replay[-4:] == [
        {"statement_start": "BEGIN", "transaction_id_assigned": False},
        {"statement_start": "CREATE TABLE app.probe", "transaction_id_assigned": False},
        {"statement_start": "CREATE INDEX probe_flag_idx", "transaction_id_assigned": True},
        {"statement_start": "COMMIT", "transaction_id_assigned": True},
    ], "psql did not run both candidate statements in the file transaction")
    return {"tool": "psql_file_transaction", "exits": exits, "replay": replay}


def emit(record):
    print(json.dumps(record, ensure_ascii=False, separators=(",", ":")))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for path in ("dmg", "squawk", "atlas", "pg17-bin", "pg18-bin"):
        parser.add_argument(f"--{path}", required=True, type=Path)
    args = parser.parse_args()
    dmg, squawk, atlas = (p.resolve() for p in (args.dmg, args.squawk, args.atlas))
    manifest, inputs = read_corpus()
    pins = {"guard": binary_pin(dmg), "squawk": binary_pin(squawk, expected_digest=SQUAWK_SHA256),
            "atlas_community": binary_pin(atlas, "version", ATLAS_SHA256)}
    require(pins["guard"]["version"] == "dmg 0.1.0", "expected Guard 0.1.0")
    require(pins["squawk"]["version"] == "squawk 2.72.0", "expected Squawk 2.72.0")
    require(pins["atlas_community"]["version"] == "atlas community version v1.3.0", "expected Atlas Community v1.3.0")
    with tempfile.TemporaryDirectory(prefix="dmgcmp-test-") as test_directory:
        stdout, test = run(atlas, "migrate", "test", cwd=Path(test_directory))
    emit({"tools": pins, "atlas_migrate_test": {**test, "stdout": stdout.strip()}})
    for major, pg_bin in ((17, args.pg17_bin.resolve()), (18, args.pg18_bin.resolve())):
        server = binary_pin(pg_bin / "postgres")
        psql = binary_pin(pg_bin / "psql")
        require(server["version"].split()[-1] == manifest["postgres"][str(major)], "wrong PostgreSQL minor")
        require(psql["version"].split()[-1] == manifest["postgres"][str(major)], "wrong psql minor")
        emit({"target_major": major, "postgres": server, "psql": psql})
        with tempfile.TemporaryDirectory(prefix="dmgcmp-") as tmp:
            work = Path(tmp)
            cluster = Cluster(pg_bin, None, work)
            config, policy, sq_config = (work / name for name in ("dmg.toml", "policy.toml", "squawk.toml"))
            config.write_text(PROJECT.format(target=major), encoding="utf-8")
            policy.write_text(POLICY, encoding="utf-8")
            sq_config.write_bytes(b"")
            with cluster.running():
                require(cluster.query("postgres", "SHOW server_version").split()[0] == manifest["postgres"][str(major)],
                        "wrong PostgreSQL dev server minor")
                cluster.query("postgres", f"ALTER SYSTEM SET log_line_prefix = '{LOG_PREFIX}'")
                cluster.query("postgres", "SELECT pg_reload_conf()")
                for index, case in enumerate(CASES):
                    run_id = f"{case}/plain/pg{major}"
                    directory = work / str(index)
                    metadata = materialize(manifest, inputs, run_id, directory)
                    repo = directory / "repo"
                    require(all(m["mode"] == "psql-file-transaction" for m in metadata["candidate"]),
                            "expected a plain file-transaction fixture")
                    git(repo, "branch", "pilot-base", metadata["git"]["base"])
                    execute(atlas, "migrate", "hash", "--dir", "file://migrations", cwd=repo)
                    for path, file in metadata["files"]["candidate"].items():
                        require(sha256((repo / path).read_bytes()) == file["sha256"], "fixture bytes changed")
                    emit({"case": run_id, "git": metadata["git"], "files": metadata["files"],
                          "atlas_sum_sha256": sha256((repo / "migrations" / "atlas.sum").read_bytes()),
                          "expected": metadata["expected"], "transaction": "psql-file-transaction"})
                    emit({"case": run_id, **guard_check(dmg, metadata, repo, config, policy)})
                    emit({"case": run_id, **squawk_check(squawk, metadata, repo, sq_config, "default")})
                    emit({"case": run_id, **squawk_check(squawk, metadata, repo, sq_config, "exclude_timeouts",
                                                          "--exclude", TIMEOUT_RULES)})
                    by_git = atlas_check(atlas, cluster, metadata, repo, ("--git-base", "pilot-base"))
                    emit({"case": run_id, **by_git})
                    latest = atlas_check(atlas, cluster, metadata, repo, ("--latest", "1"))
                    require(by_git["files"] == latest["files"] and by_git["exit"] == latest["exit"]
                            and by_git["replay"] == latest["replay"], "Atlas lint selection modes differed")
                    emit({"case": run_id, **latest})
                    if case == "R05/regular-index":
                        (repo / "atlas.hcl").write_text(ATLAS_INDEX_POLICY, encoding="utf-8")
                        emit({"case": run_id, **atlas_check(atlas, cluster, metadata, repo, ("--git-base", "pilot-base"),
                                                             "concurrent_index_error", ("--env", "pilot"))})
                    if case == "R07/new-relation":
                        require(by_git["replay"][4:] == [
                            {"statement_start": "CREATE TABLE app.probe", "transaction_id_assigned": False},
                            {"statement_start": "CREATE INDEX probe_flag_idx", "transaction_id_assigned": False},
                        ], "Atlas R07 replay transaction behavior differed from the pinned observation")
                        emit({"case": run_id, **native_file_control(cluster, metadata, repo)})


if __name__ == "__main__":
    main()
