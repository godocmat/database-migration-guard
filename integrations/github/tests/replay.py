"""Replay the pilot gate against disposable Git commits and a real local dmg binary."""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path


GITHUB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(GITHUB))
import gate


ROOT = GITHUB.parents[1]
EXAMPLE = ROOT / "examples" / "github"


def git(repo, *args, data=None):
    environment = gate.child_env(repo.parent)
    environment.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
                       GIT_AUTHOR_NAME="DMG fixture", GIT_AUTHOR_EMAIL="dmg@example.invalid",
                       GIT_COMMITTER_NAME="DMG fixture", GIT_COMMITTER_EMAIL="dmg@example.invalid")
    return subprocess.run(["git", "-C", str(repo), *args], input=data, env=environment,
                          capture_output=True, check=True, timeout=20).stdout.decode("ascii").strip()


def write(repo, path, value):
    file = repo / path
    file.parent.mkdir(parents=True, exist_ok=True)
    file.write_bytes(value)


def commit(repo, message):
    git(repo, "add", ".")
    git(repo, "commit", "--quiet", "--no-gpg-sign", "-m", message)
    return git(repo, "rev-parse", "HEAD")


def check_fixture(binary, scratch, number, kind, expected_exit):
    upstream = scratch / f"source-{number}"
    upstream.mkdir()
    git(upstream, "init", "--quiet", "--initial-branch=base", "--object-format=sha1")
    write(upstream, ".dmg/dmg.toml", (EXAMPLE / "dmg.toml").read_bytes())
    write(upstream, ".dmg/policy.toml", (EXAMPLE / "policy.toml").read_bytes())
    write(upstream, "migrations/001_base.sql", b"CREATE TABLE public.demo (id bigint);\n")
    base = commit(upstream, "Protected base")

    git(upstream, "switch", "--quiet", "-c", "candidate")
    # Both config and policy are valid only in the protected base.
    write(upstream, ".dmg/dmg.toml", b"Candidate draft configuration\n")
    write(upstream, ".dmg/policy.toml", b"Candidate draft policy\n")
    sql = {
        "denied": b"DROP TABLE public.demo;\n",
        "incomplete": b"ALTER TABLE public.demo ADD COLUMN;\n",
        "pass": b"ALTER TABLE public.demo ADD COLUMN created_at timestamp;\n",
    }[kind]
    write(upstream, "migrations/002_change.sql", sql)
    head = commit(upstream, "Candidate migration")
    tree = git(upstream, "rev-parse", "HEAD^{tree}")
    merge = git(upstream, "commit-tree", tree, "-p", base, "-p", head, "-m", "Test merge")
    git(upstream, "update-ref", f"refs/pull/{number}/merge", merge)

    directory = scratch / f"run-{number}"
    directory.mkdir()
    for name in ("objects.git", "reports", "home"):
        (directory / name).mkdir()
    event = {"number": number, "pull_request": {"base": {"sha": base, "repo": {"full_name": "example/pilot"}},
                                                 "head": {"sha": head}}}
    event_path = directory / "event.json"
    gate.write_json(event_path, event)
    revision = gate.context({"GITHUB_EVENT_NAME": "pull_request", "GITHUB_SERVER_URL": "https://github.com",
                             "GITHUB_EVENT_PATH": str(event_path), "GITHUB_REPOSITORY": "example/pilot",
                             "GITHUB_SHA": merge, "GITHUB_REF": f"refs/pull/{number}/merge"})
    repo = directory / "objects.git"
    gate.fetch_merge(repo, upstream.as_uri(), revision, local=True)
    assert git(repo, "rev-parse", "--is-bare-repository") == "true"
    assert not (repo / "migrations").exists()
    assert not (repo / "config.toml").exists()
    assert not (repo / "FETCH_HEAD").exists()

    gate.save_protected_inputs(directory, repo, revision, ".dmg/dmg.toml", ".dmg/policy.toml")
    assert (directory / "policy.toml").read_bytes() == (EXAMPLE / "policy.toml").read_bytes()
    local_binary = directory / binary.name
    shutil.copy2(binary, local_binary)
    local_binary.chmod(0o700)
    revision.update(binary_name=local_binary.name, binary_sha256=gate.file_digest(local_binary))
    gate.write_json(directory / "inputs.json", revision)
    summary = directory / "step-summary.md"
    assert gate.check(directory, summary) == expected_exit

    reports = directory / "reports"
    report = json.loads((reports / "report.json").read_bytes())
    provenance = json.loads((reports / "provenance.json").read_bytes())
    assert provenance["base_commit"] == base and provenance["pull_head_commit"] == head
    assert provenance["test_merge_commit"] == merge and provenance["binary_sha256"] == revision["binary_sha256"]
    assert provenance["decision"] == kind and provenance["check_exit"] == provenance["gate_exit"] == expected_exit
    assert provenance["report_sha256"] == gate.file_digest(reports / "report.json")
    assert provenance["policy_sha256"] == report["decision"]["policy_digest"]
    assert provenance["config_sha256"] == gate.file_digest(directory / "config.toml")
    assert (reports / "report.md").read_bytes() in summary.read_bytes()
    if kind == "denied":
        assert any(finding["rule_id"] == "PG001" for finding in report["findings"])

    # An event whose advertised parents differ is never accepted as the scan pair.
    try:
        gate.verify_merge(repo, {**revision, "pull_head_commit": base})
    except gate.GateError:
        pass
    else:
        raise AssertionError("A mismatched event parent was accepted")
    print(f"Verified {kind} with original exit {expected_exit}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    with tempfile.TemporaryDirectory(prefix="dmg-gate-replay-") as work:
        for number, (kind, exit_code) in enumerate((("denied", 1), ("pass", 0), ("incomplete", 3)), start=1):
            check_fixture(binary, Path(work), number, kind, exit_code)


if __name__ == "__main__":
    main()
