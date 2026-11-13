"""Replay GitLab jobs using disposable Git commits and an actual dmg executable."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch


GITLAB = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(GITLAB))
import gate


ROOT = GITLAB.parents[1]
EXAMPLES = ROOT / "examples" / "github"
CONFIG = (EXAMPLES / "dmg.toml").read_bytes()
POLICY = (EXAMPLES / "policy.toml").read_bytes()
TOKEN = "synthetic.fixture-token"
REMOTE = "https://gitlab.example.invalid/team/subgroup/application.git"


def git(repo, *args):
    environment = gate.child_env(repo.parent)
    environment.update(
        GIT_AUTHOR_NAME="DMG fixture", GIT_AUTHOR_EMAIL="dmg@example.invalid",
        GIT_COMMITTER_NAME="DMG fixture", GIT_COMMITTER_EMAIL="dmg@example.invalid",
    )
    return subprocess.run(
        ["git", "-C", str(repo), *args], env=environment, stdin=subprocess.DEVNULL,
        capture_output=True, check=True, timeout=20,
    ).stdout.decode("ascii").strip()


def write(repo, path, value):
    target = repo / path
    target.parent.mkdir(parents=True, exist_ok=True)
    target.write_bytes(value)


def commit(repo, message):
    git(repo, "add", "--all")
    git(repo, "commit", "--quiet", "--no-gpg-sign", "-m", message)
    return git(repo, "rev-parse", "HEAD")


def fixture(scratch, case, binary_pin, *, advisory=False, config=CONFIG, protected_blob=True):
    upstream = scratch / "upstream"
    upstream.mkdir()
    git(upstream, "init", "--quiet", "--initial-branch=main", "--object-format=sha1")
    write(upstream, ".dmg/dmg.toml", config)
    write(upstream, ".dmg/policy.toml", POLICY)
    if not protected_blob:
        oid = git(upstream, "hash-object", "-w", ".dmg/policy.toml")
        git(upstream, "add", "--all")
        git(upstream, "update-index", "--cacheinfo", f"120000,{oid},.dmg/policy.toml")
        git(upstream, "commit", "--quiet", "--no-gpg-sign", "-m", "Non-ordinary policy blob")
        base = git(upstream, "rev-parse", "HEAD")
    else:
        write(upstream, "migrations/001_base.sql", b"CREATE TABLE public.demo (id bigint);\n")
        base = commit(upstream, "Protected target")
    git(upstream, "switch", "--quiet", "-c", "feature")
    if not advisory:
        write(upstream, ".dmg/dmg.toml", b"invalid candidate configuration\n")
    write(upstream, ".dmg/policy.toml", b"invalid candidate policy\n")
    write(upstream, ".gitlab-ci.yml", b"candidate-controlled job configuration\n")
    marker = scratch / "candidate-ran"
    write(upstream, "gate.py", f"from pathlib import Path\nPath({str(marker)!r}).touch()\n".encode())
    change = {
        "pass": b"ALTER TABLE public.demo ADD COLUMN created_at timestamp;\n",
        "denied": b"DROP TABLE public.demo;\n",
        "incomplete": b"ALTER TABLE public.demo ADD COLUMN;\n",
    }
    write(upstream, "migrations/002_change.sql", change[case])
    source = commit(upstream, "Candidate migration")
    tree = git(upstream, "rev-parse", "HEAD^{tree}")
    merged = git(upstream, "commit-tree", tree, "-p", base, "-p", source, "-m", "Pipeline merge")
    git(upstream, "update-ref", "refs/pipelines/21", merged)

    workspace = scratch / "job"
    workspace.mkdir()
    env = {
        "CI_PIPELINE_SOURCE": "merge_request_event",
        "CI_MERGE_REQUEST_EVENT_TYPE": "merged_result",
        "CI_SERVER_URL": "https://gitlab.example.invalid",
        "CI_PROJECT_URL": REMOTE.removesuffix(".git"),
        "CI_PROJECT_PATH": "team/subgroup/application",
        "CI_PROJECT_ID": "17",
        "CI_MERGE_REQUEST_PROJECT_ID": "17",
        "CI_MERGE_REQUEST_SOURCE_PROJECT_ID": "17",
        "CI_MERGE_REQUEST_PROJECT_PATH": "team/subgroup/application",
        "CI_MERGE_REQUEST_SOURCE_PROJECT_PATH": "team/subgroup/application",
        "CI_MERGE_REQUEST_SOURCE_BRANCH_NAME": "feature",
        "CI_MERGE_REQUEST_TARGET_BRANCH_NAME": "main",
        "CI_MERGE_REQUEST_TARGET_BRANCH_PROTECTED": "true",
        "CI_MERGE_REQUEST_TARGET_BRANCH_SHA": base,
        "CI_MERGE_REQUEST_SOURCE_BRANCH_SHA": source,
        "CI_MERGE_REQUEST_REF_PATH": "refs/merge-requests/31/head",
        "CI_MERGE_REQUEST_IID": "31",
        "CI_PIPELINE_ID": "21",
        "CI_COMMIT_SHA": merged,
        "CI_JOB_TOKEN": TOKEN,
        "DMG_GITLAB_ORIGIN": "https://gitlab.example.invalid",
        "DMG_GITLAB_PROJECT_ID": "17",
        "DMG_GITLAB_PROJECT_PATH": "team/subgroup/application",
        "DMG_GITLAB_TARGET_BRANCH": "main",
        "DMG_BINARY_SHA256": binary_pin,
        "DMG_CODE_QUALITY": "true",
    }
    if advisory:
        env.update(
            CI_MERGE_REQUEST_EVENT_TYPE="detached", CI_MERGE_REQUEST_PROJECT_ID="18",
            CI_MERGE_REQUEST_PROJECT_PATH="team/parent", CI_MERGE_REQUEST_DIFF_BASE_SHA=base,
            CI_COMMIT_SHA=source, CI_MERGE_REQUEST_TARGET_BRANCH_SHA="",
            CI_MERGE_REQUEST_SOURCE_BRANCH_SHA="",
        )
    return upstream, workspace, env


class Replay(unittest.TestCase):
    binary: Path
    binary_pin: str
    code_quality: bool

    def prepare(self, case, **kwargs):
        directory = tempfile.TemporaryDirectory(prefix="dmg-gitlab-replay-")
        self.addCleanup(directory.cleanup)
        scratch = Path(directory.name).resolve()
        upstream, workspace, env = fixture(scratch, case, self.binary_pin, **kwargs)
        if not self.code_quality:
            env["DMG_CODE_QUALITY"] = "false"
        return upstream, workspace, env

    def run_fixture(self, upstream, workspace, env, mode="protected"):
        with patch.dict(os.environ, {"CI_JOB_TOKEN": TOKEN, "PGPASSWORD": "fixture-pg-secret"}):
            status = gate.run_job(env, workspace, mode, binary=self.binary, local_remote=upstream.as_uri())
        self.assertFalse((workspace.parent / 'candidate-ran').exists())
        self.assertEqual(list(workspace.iterdir()), [workspace / gate.ARTIFACTS])
        artifacts = workspace / gate.ARTIFACTS
        for path in artifacts.iterdir():
            content = path.read_bytes()
            self.assertNotIn(TOKEN.encode(), content)
            self.assertNotIn(b"fixture-pg-secret", content)
            self.assertNotIn(b"candidate-controlled job configuration", content)
        return status, artifacts

    def test_default_exit_statuses_and_base_policy(self):
        for case, expected in (("pass", 0), ("denied", 1), ("incomplete", 3)):
            with self.subTest(case=case):
                upstream, workspace, env = self.prepare(case)
                status, artifacts = self.run_fixture(upstream, workspace, env)
                self.assertEqual(status, expected)
                report_path = artifacts / "report.json"
                provenance = json.loads((artifacts / "provenance.json").read_bytes())
                self.assertEqual(provenance["check_exit"], expected)
                self.assertEqual(provenance["decision"], case)
                self.assertEqual(provenance["base_commit"], env["CI_MERGE_REQUEST_TARGET_BRANCH_SHA"])
                self.assertEqual(provenance["scan_commit"], env["CI_COMMIT_SHA"])
                self.assertEqual(provenance["config_sha256"], hashlib.sha256(CONFIG).hexdigest())
                self.assertEqual(provenance["policy_sha256"], hashlib.sha256(POLICY).hexdigest())
                self.assertEqual(provenance["report_sha256"], gate.digest_file(report_path))
                self.assertGreater((artifacts / "report.md").stat().st_size, 0)
                if self.code_quality:
                    issues = json.loads((artifacts / "gl-code-quality-report.json").read_bytes())
                    report = json.loads(report_path.read_bytes())
                    self.assertTrue(provenance["code_quality_rendered"])
                    self.assertEqual(
                        [issue["fingerprint"] for issue in issues],
                        [finding["fingerprint"] for finding in report["findings"]],
                    )
                    if case == "denied":
                        self.assertTrue(issues)
                        self.assertEqual(issues[0]["location"]["path"], "migrations/002_change.sql")
                        self.assertEqual(issues[0]["location"]["lines"]["begin"], 1)

    def test_missing_artifact_blocks_a_pass_and_preserves_denial(self):
        original_replace = Path.replace

        def refuse_report(source, destination):
            if Path(destination).name == "report.json":
                raise OSError("synthetic artifact write failure")
            return original_replace(source, destination)

        for case, expected in (("pass", 3), ("denied", 1)):
            with self.subTest(case=case):
                upstream, workspace, env = self.prepare(case)
                with patch.object(Path, "replace", refuse_report):
                    status, artifacts = self.run_fixture(upstream, workspace, env)
                self.assertEqual(status, expected)
                self.assertFalse((artifacts / "report.json").exists())
                self.assertTrue((artifacts / "report.md").is_file())
                provenance = json.loads((artifacts / "provenance.json").read_bytes())
                self.assertEqual(provenance["gate_exit"], expected)
                self.assertIsNone(provenance["report_sha256"])

    def test_wrong_report_basis_cannot_pass(self):
        upstream, workspace, env = self.prepare("pass")
        real_run = gate.run_cli

        def change_basis(binary, work, *args, **kwargs):
            result = real_run(binary, work, *args, **kwargs)
            if args[0] == "check":
                output = Path(args[-1])
                document = json.loads(output.read_bytes())
                document["migrations"]["integrity"]["basis"]["head_commit"] = "a" * 40
                output.write_text(json.dumps(document), encoding="utf-8")
            return result

        with patch.object(gate, "run_cli", change_basis):
            self.assert_incomplete(upstream, workspace, env)

    def test_custom_denial_exit_is_canonical_for_ci(self):
        config = CONFIG + b"\n[exit_codes]\ndenied = 42\nincomplete = 99\n"
        upstream, workspace, env = self.prepare("denied", config=config)
        status, artifacts = self.run_fixture(upstream, workspace, env)
        self.assertEqual(status, 1)
        result = json.loads((artifacts / "provenance.json").read_bytes())
        self.assertEqual(result["check_exit"], 42)
        self.assertEqual(result["gate_exit"], 1)

    def test_fork_advisory_has_no_policy_decision(self):
        upstream, workspace, env = self.prepare("denied", advisory=True)
        status, artifacts = self.run_fixture(upstream, workspace, env, "advisory")
        self.assertEqual(status, 0)
        report = json.loads((artifacts / "report.json").read_bytes())
        provenance = json.loads((artifacts / "provenance.json").read_bytes())
        self.assertIsNone(report["decision"])
        self.assertTrue(report["findings"])
        self.assertEqual(provenance["mode"], "advisory")
        self.assertNotIn("policy_sha256", provenance)

    def assert_incomplete(self, upstream, workspace, env):
        status, artifacts = self.run_fixture(upstream, workspace, env)
        self.assertEqual(status, 3)
        self.assertFalse((artifacts / "report.json").exists())
        self.assertTrue((artifacts / "report.md").is_file())
        provenance = json.loads((artifacts / "provenance.json").read_bytes())
        self.assertEqual(provenance["gate_exit"], 3)
        self.assertIsNone(provenance["report_sha256"])

    def test_fails_closed_on_bad_metadata(self):
        for field, value in (
            ("CI_MERGE_REQUEST_EVENT_TYPE", "detached"),
            ("CI_MERGE_REQUEST_EVENT_TYPE", "merge_train"),
            ("CI_MERGE_REQUEST_TARGET_BRANCH_PROTECTED", "false"),
            ("CI_MERGE_REQUEST_SOURCE_PROJECT_ID", "18"),
            ("CI_MERGE_REQUEST_IID", "../31"),
            ("CI_MERGE_REQUEST_REF_PATH", "refs/merge-requests/999/head"),
            ("CI_PROJECT_PATH", "team/../application"),
            ("CI_PROJECT_URL", "https://wrong.example.invalid/team/application"),
            ("CI_SERVER_URL", "http://gitlab.example.invalid"),
            ("CI_COMMIT_SHA", "feature"),
        ):
            with self.subTest(field=field, value=value):
                upstream, workspace, env = self.prepare("pass")
                env[field] = value
                with patch.object(gate, "fetch_revisions", side_effect=AssertionError("Unexpected fetch")):
                    self.assert_incomplete(upstream, workspace, env)

    def test_fails_closed_on_wrong_commit_or_parents(self):
        for kind in ("missing", "one-parent", "reversed", "stale-base", "stale-source"):
            with self.subTest(kind=kind):
                upstream, workspace, env = self.prepare("pass")
                target = env["CI_MERGE_REQUEST_TARGET_BRANCH_SHA"]
                source = env["CI_MERGE_REQUEST_SOURCE_BRANCH_SHA"]
                if kind == "missing":
                    env["CI_COMMIT_SHA"] = "d" * 40
                elif kind in {"one-parent", "reversed"}:
                    tree = git(upstream, "rev-parse", "HEAD^{tree}")
                    parents = ("-p", source) if kind == "one-parent" else ("-p", source, "-p", target)
                    env["CI_COMMIT_SHA"] = git(upstream, "commit-tree", tree, *parents, "-m", "Wrong merge")
                    git(upstream, "update-ref", "refs/pipelines/21", env["CI_COMMIT_SHA"])
                else:
                    branch = "main" if kind == "stale-base" else "feature"
                    git(upstream, "switch", "--quiet", branch)
                    write(upstream, "update.txt", b"new branch tip\n")
                    commit(upstream, "Branch advanced")
                self.assert_incomplete(upstream, workspace, env)

    def test_fails_closed_on_input_or_binary_verification(self):
        upstream, workspace, env = self.prepare("pass", protected_blob=False)
        self.assert_incomplete(upstream, workspace, env)
        upstream, workspace, env = self.prepare("pass")
        env["DMG_POLICY_PATH"] = "../policy.toml"
        self.assert_incomplete(upstream, workspace, env)
        upstream, workspace, env = self.prepare("pass")
        env["DMG_BINARY_SHA256"] = "0" * 64
        with patch.object(gate, "fetch_revisions", side_effect=AssertionError("Unexpected fetch")):
            self.assert_incomplete(upstream, workspace, env)

    def test_artifact_collision_does_not_follow_existing_path(self):
        upstream, workspace, env = self.prepare("pass")
        artifacts = workspace / gate.ARTIFACTS
        artifacts.mkdir()
        marker = artifacts / "report.json"
        marker.write_bytes(b"preexisting")
        with self.assertRaises(gate.GateError):
            gate.run_job(env, workspace, "protected", binary=self.binary, local_remote=upstream.as_uri())
        self.assertEqual(marker.read_bytes(), b"preexisting")

    def test_fetch_token_is_confined_to_git_child(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            with patch.object(gate.subprocess, "run") as run, patch.dict(os.environ, {
                "CI_JOB_TOKEN": TOKEN, "GIT_CONFIG_COUNT": "10",
                "GIT_DIR": "unexpected", "HTTPS_PROXY": "https://invalid.invalid",
                "PGPASSWORD": "fixture-pg-secret",
            }):
                gate.git(directory, "fetch", REMOTE, token=TOKEN, capture=False)
                args, options = run.call_args
                environment = options["env"]
                self.assertNotIn(TOKEN, " ".join(args[0]))
                self.assertTrue(environment["GIT_CONFIG_VALUE_0"].startswith("Authorization: Basic "))
                self.assertEqual(environment["GIT_CONFIG_COUNT"], "1")
                self.assertEqual(environment["GIT_ALLOW_PROTOCOL"], "https")
                for variable in ("CI_JOB_TOKEN", "GIT_DIR", "HTTPS_PROXY", "PGPASSWORD"):
                    self.assertNotIn(variable, environment)
                gate.run_cli(self.binary, directory, "--version")
                cli_environment = run.call_args.kwargs["env"]
                self.assertNotIn("GIT_CONFIG_VALUE_0", cli_environment)
                self.assertEqual(cli_environment["GIT_ALLOW_PROTOCOL"], "")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--without-code-quality", action="store_true", help="replay an older binary")
    args = parser.parse_args()
    Replay.binary = args.binary.resolve(strict=True)
    Replay.binary_pin = gate.digest_file(Replay.binary)
    Replay.code_quality = not args.without_code_quality
    return not unittest.TextTestRunner(verbosity=2).run(
        unittest.defaultTestLoader.loadTestsFromTestCase(Replay)
    ).wasSuccessful()


if __name__ == "__main__":
    sys.exit(main())
