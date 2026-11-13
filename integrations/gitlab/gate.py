"""Run the GitLab merge gate or the fork's advisory migration scan."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import tomllib
from pathlib import Path
from urllib.parse import urlsplit


MIB = 1024 * 1024
MAX_REPORT = 64 * MIB
MAX_MARKDOWN = 900 * 1024
ARTIFACTS = "dmg-reports"
BINARY = Path("/opt/dmg/dmg")
SHA = re.compile(r"[a-f0-9]{40}")
DIGEST = re.compile(r"[a-f0-9]{64}")
ID = re.compile(r"[1-9][0-9]{0,19}")
PART = re.compile(r"[A-Za-z0-9_.-]{1,128}")
ORIGIN = re.compile(r"https://[a-z0-9][a-z0-9.-]{0,252}(?::[0-9]{1,5})?")
TOKEN = re.compile(r"[A-Za-z0-9._+/=-]{1,8192}")
LOCAL_REFS = {
    "base_commit": "refs/dmg/base",
    "source_commit": "refs/dmg/source",
    "scan_commit": "refs/dmg/scan",
}


class GateError(Exception):
    """An error with a fixed, safe diagnostic."""


def require(condition, message):
    if not condition:
        raise GateError(message)


def portable_path(value):
    return len(value) <= 512 and all(
        PART.fullmatch(part) and part not in {".", ".."} for part in value.split("/")
    )


def project_path(value):
    return "/" in value and portable_path(value)


def build_context(environ, mode):
    require(environ.get("CI_PIPELINE_SOURCE") == "merge_request_event",
            "Migration Guard requires a merge request pipeline.")
    origin = environ.get("DMG_GITLAB_ORIGIN", "")
    require(ORIGIN.fullmatch(origin) and environ.get("CI_SERVER_URL") == origin,
            "The pipeline server does not match the pinned HTTPS GitLab origin.")
    url = urlsplit(origin)
    require(url.hostname and all(
        re.fullmatch(r"[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?", label)
        for label in url.hostname.split(".")
    ) and (url.port is None or 1 <= url.port <= 65535), "Invalid GitLab origin.")

    project = environ.get("CI_PROJECT_PATH", "")
    project_id = environ.get("CI_PROJECT_ID", "")
    source_project_id = environ.get("CI_MERGE_REQUEST_SOURCE_PROJECT_ID", "")
    mr_project_id = environ.get("CI_MERGE_REQUEST_PROJECT_ID", "")
    pipeline_id = environ.get("CI_PIPELINE_ID", "")
    merge_request = environ.get("CI_MERGE_REQUEST_IID", "")
    require(project_path(project) and all(
        ID.fullmatch(value) for value in (
            project_id, source_project_id, mr_project_id, pipeline_id, merge_request
        )
    ) and environ.get("CI_PROJECT_URL") == f"{origin}/{project}",
        "Invalid GitLab project or pipeline identity.")
    require(environ.get("CI_MERGE_REQUEST_REF_PATH")
            == f"refs/merge-requests/{merge_request}/head",
            "The merge request ref does not match the event.")

    scan = environ.get("CI_COMMIT_SHA", "")
    kind = environ.get("CI_MERGE_REQUEST_EVENT_TYPE")
    branch_fields = {}
    if mode == "protected":
        require(kind == "merged_result",
                "The protected check requires a merged-results pipeline.")
        require(project_id == source_project_id == mr_project_id
                == environ.get("DMG_GITLAB_PROJECT_ID")
                and project == environ.get("CI_MERGE_REQUEST_PROJECT_PATH")
                == environ.get("CI_MERGE_REQUEST_SOURCE_PROJECT_PATH")
                == environ.get("DMG_GITLAB_PROJECT_PATH"),
                "The protected check requires a same-project merge request.")
        target = environ.get("DMG_GITLAB_TARGET_BRANCH", "")
        source_branch = environ.get("CI_MERGE_REQUEST_SOURCE_BRANCH_NAME", "")
        require(portable_path(target) and portable_path(source_branch)
                and source_branch != target
                and environ.get("CI_MERGE_REQUEST_TARGET_BRANCH_NAME") == target
                and environ.get("CI_MERGE_REQUEST_TARGET_BRANCH_PROTECTED") == "true",
                "The merge request does not target the pinned protected branch.")
        base = environ.get("CI_MERGE_REQUEST_TARGET_BRANCH_SHA", "")
        source = environ.get("CI_MERGE_REQUEST_SOURCE_BRANCH_SHA", "")
        require(SHA.fullmatch(source) and len({base, source, scan}) == 3,
                "The merge request does not supply a distinct two-parent merged result.")
        branch_fields = {"target_branch": target, "source_branch": source_branch}
    else:
        require(mode == "advisory" and kind == "detached"
                and project_id == source_project_id != mr_project_id
                and project == environ.get("CI_MERGE_REQUEST_SOURCE_PROJECT_PATH"),
                "The advisory scan runs only in the source project of a fork merge request.")
        base = environ.get("CI_MERGE_REQUEST_DIFF_BASE_SHA", "")
        source = scan

    require(SHA.fullmatch(base) and SHA.fullmatch(scan),
            "The pipeline requires full SHA-1 commit IDs.")
    return {
        "mode": mode,
        "project": project,
        "project_id": int(project_id),
        "merge_request_iid": int(merge_request),
        "pipeline_id": int(pipeline_id),
        "base_commit": base,
        "source_commit": source,
        "scan_commit": scan,
        **branch_fields,
    }, f"{origin}/{project}.git"


def child_env(home):
    allowed = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR"}
    env = {key: value for key, value in os.environ.items() if key.upper() in allowed}
    env.update(HOME=str(home), USERPROFILE=str(home), XDG_CONFIG_HOME=str(home),
               LC_ALL="C.UTF-8", GIT_CONFIG_GLOBAL=os.devnull, GIT_CONFIG_NOSYSTEM="1",
               GIT_TERMINAL_PROMPT="0", GIT_NO_LAZY_FETCH="1", GIT_ALLOW_PROTOCOL="")
    return env


def git(repo, *args, timeout=20, token=None, local=False, capture=True):
    env = child_env(repo.parent / "home")
    if token:
        auth = base64.b64encode(f"gitlab-ci-token:{token}".encode("ascii")).decode("ascii")
        env.update(GIT_ALLOW_PROTOCOL="https", GIT_CONFIG_COUNT="1",
                   GIT_CONFIG_KEY_0="http.extraHeader",
                   GIT_CONFIG_VALUE_0=f"Authorization: Basic {auth}")
    elif local:
        env["GIT_ALLOW_PROTOCOL"] = "file"
    return subprocess.run(
        ["git", "--no-replace-objects", "--literal-pathspecs", "-C", str(repo), *args],
        env=env, stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL, timeout=timeout,
    ).stdout


def fetch_revisions(repo, remote, revision, *, token=None, local=False):
    git(repo, "init", "--bare", "--quiet", "--template=", "--object-format=sha1")
    refspecs = [f"{revision['scan_commit']}:{LOCAL_REFS['scan_commit']}"]
    protected = revision["mode"] == "protected"
    if protected:
        for field, branch in (("base_commit", "target_branch"), ("source_commit", "source_branch")):
            ref = f"refs/heads/{revision[branch]}"
            git(repo, "check-ref-format", ref)
            refspecs.append(f"{ref}:{LOCAL_REFS[field]}")
    else:
        refspecs.append(f"{revision['base_commit']}:{LOCAL_REFS['base_commit']}")
    git(repo, "-c", "http.followRedirects=false", "-c", "fetch.fsckObjects=true",
        "fetch", "--atomic", "--quiet", "--depth=2", "--no-tags", "--no-recurse-submodules",
        "--no-auto-maintenance", "--no-write-fetch-head", remote, *refspecs,
        timeout=120, token=token, local=local, capture=False)

    checked = LOCAL_REFS if protected else ("base_commit", "scan_commit")
    for field in checked:
        actual = git(repo, "rev-parse", "--verify", f"{LOCAL_REFS[field]}^{{commit}}").strip()
        require(actual == revision[field].encode("ascii"),
                "A pipeline branch or commit changed. Run the check for the current revision.")
    if protected:
        commit_ref = LOCAL_REFS["scan_commit"]
        require(int(git(repo, "cat-file", "-s", commit_ref)) <= MIB,
                "The merged-results commit exceeds the commit metadata limit.")
        line = git(repo, "rev-list", "--parents", "-n", "1", commit_ref, "--")
        expected = [revision[key].encode("ascii") for key in (
            "scan_commit", "base_commit", "source_commit"
        )]
        require(line.split() == expected,
                "The merged-results commit does not have the expected target and source parents.")


def blob(repo, revision, path):
    require(portable_path(path), "Use portable repository-relative config and policy paths.")
    listing = git(repo, "ls-tree", "--full-tree", "-z", revision, "--", path)
    fields, separator, name = listing.partition(b"\t")
    parts = fields.split()
    require(len(parts) == 3 and separator and name == path.encode("ascii") + b"\0",
            "A configured Guard input is missing from the selected commit.")
    mode, kind, oid = parts
    require(kind == b"blob" and mode in {b"100644", b"100755"} and SHA.fullmatch(oid.decode("ascii")),
            "The selected Guard input must be an ordinary Git blob.")
    object_id = oid.decode("ascii")
    size = int(git(repo, "cat-file", "-s", object_id))
    require(0 < size <= MIB, "A Guard input is empty or exceeds the input size limit.")
    data = git(repo, "cat-file", "blob", object_id)
    require(len(data) == size, "A Guard input size does not match its Git object.")
    return data


def digest_file(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def install_binary(binary, expected, work):
    require(binary.is_absolute() and DIGEST.fullmatch(expected),
            "The runner requires an absolute dmg executable and a pinned SHA-256.")
    info = binary.lstat()
    require(stat.S_ISREG(info.st_mode) and 0 < info.st_size <= MAX_REPORT,
            "The pinned dmg executable is missing or exceeds its size limit.")
    target = work / ("dmg.exe" if os.name == "nt" else "dmg")
    shutil.copyfile(binary, target)
    require(digest_file(target) == expected, "The dmg executable failed SHA-256 verification.")
    target.chmod(0o700)
    return target


def run_cli(binary, work, *args, timeout=120):
    return subprocess.run(
        [str(binary), *args], cwd=work, env=child_env(work / "home"),
        stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        timeout=timeout, check=False,
    ).returncode


def verified_report(path, revision, policy_digest):
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= MAX_REPORT:
        return None
    try:
        report = json.loads(path.read_bytes())
        coverage = report["coverage"]["status"]
        integrity = report["migrations"]["integrity"]
        decision = report["decision"]
        require(type(report["schema_version"]) is int and report["schema_version"] == 1
                and coverage in {"complete", "incomplete"}, "Invalid Guard report.")
        if integrity is not None:
            basis = integrity["basis"]
            require(basis["kind"] == "git" and basis["base_commit"] == revision["base_commit"]
                    and basis["head_commit"] == revision["scan_commit"],
                    "The report integrity basis does not match the pipeline.")
        if policy_digest is None:
            require(decision is None and (integrity is not None or coverage == "incomplete"),
                    "Unexpected advisory report decision or provenance.")
            return None, coverage
        status = decision["status"]
        require(decision["policy_digest"] == policy_digest and status in {
            "pass", "denied", "incomplete"
        }, "The report decision does not match the protected policy.")
        if status != "incomplete":
            require(coverage == "complete" and integrity is not None,
                    "The report lacks required revision or coverage evidence.")
        return status, coverage
    except (GateError, AttributeError, KeyError, TypeError, ValueError):
        return None


def run_check(binary, work, repo, revision, config_data, policy_data):
    config = work / "dmg.toml"
    config.write_bytes(config_data)
    args = ["--repo", str(repo), "--config", str(config), "--base", revision["base_commit"],
            "--head", revision["scan_commit"], "--format", "json"]
    policy_digest = None
    command = "scan"
    if policy_data is not None:
        command = "check"
        policy = work / "policy.toml"
        policy.write_bytes(policy_data)
        policy_digest = hashlib.sha256(policy_data).hexdigest()
        args.extend(["--policy", str(policy)])

    raw_report = work / "check.json"
    raw = run_cli(binary, work, command, *args, "--output", str(raw_report))
    verified = verified_report(raw_report, revision, policy_digest)
    if verified is None:
        return raw, 3, None, None, None

    decision, coverage = verified
    codes = tomllib.loads(config_data.decode("utf-8")).get("exit_codes", {})
    expected_code = {
        "pass": 0,
        "denied": codes.get("denied", 1),
        "incomplete": codes.get("incomplete", 3),
    }
    status = decision or ("pass" if coverage == "complete" else "incomplete")
    if type(expected_code[status]) is not int or raw != expected_code[status]:
        return raw, 3, None, None, None
    return raw, {"pass": 0, "denied": 1, "incomplete": 3}[status], decision, coverage, raw_report


def render(binary, work, report, format_name, destination, limit):
    temporary = work / ("rendered-" + destination.name)
    try:
        status = run_cli(binary, work, "report", "--input", str(report), "--format", format_name,
                         "--output", str(temporary), timeout=30)
        if status == 0 and temporary.is_file() and 0 < temporary.stat().st_size <= limit:
            temporary.replace(destination)
            return True
    except (OSError, subprocess.SubprocessError):
        pass
    return False


def write_fallback_markdown(path, message, gate_exit):
    path.write_text(
        f"# Database Migration Guard\n\n{message}\n\nGate exit {gate_exit}. See provenance.json.\n",
        encoding="utf-8",
    )


def run_job(environ, workspace, mode, *, binary=BINARY, local_remote=None):
    """Run in a runner-owned empty workspace. File transport is only used by replay."""
    require(workspace.is_absolute() and workspace.is_dir()
            and workspace.resolve() == workspace and not workspace.is_symlink(),
            "Use the runner's fresh absolute job workspace.")
    artifacts = workspace / ARTIFACTS
    try:
        artifacts.mkdir(mode=0o700)
    except FileExistsError:
        raise GateError("The artifact directory must start empty.") from None

    summary = {"schema_version": 1, "mode": mode, "gate_exit": 3, "check_exit": None,
               "decision": None, "coverage": None, "report_sha256": None}
    message = "Migration Guard did not produce a verified report."
    with tempfile.TemporaryDirectory(prefix="dmg-", dir=workspace) as scratch:
        work = Path(scratch)
        (work / "home").mkdir()
        repo = work / "objects.git"
        repo.mkdir()
        try:
            revision, remote = build_context(environ, mode)
            summary.update(revision)
            token = environ.get("CI_JOB_TOKEN", "")
            require(local_remote is not None or TOKEN.fullmatch(token),
                    "The job needs its built-in token for the repository fetch.")
            expected = environ.get("DMG_BINARY_SHA256", "")
            executable = install_binary(binary, expected, work)
            summary["binary_sha256"] = expected
            if local_remote is None:
                fetch_revisions(repo, remote, revision, token=token)
            else:
                fetch_revisions(repo, local_remote, revision, local=True)
            config_path = environ.get("DMG_CONFIG_PATH", ".dmg/dmg.toml")
            config_revision = revision["base_commit" if mode == "protected" else "scan_commit"]
            config = blob(repo, config_revision, config_path)
            summary.update(config_path=config_path, config_sha256=hashlib.sha256(config).hexdigest())
            policy = None
            if mode == "protected":
                policy_path = environ.get("DMG_POLICY_PATH", ".dmg/policy.toml")
                policy = blob(repo, revision["base_commit"], policy_path)
                summary.update(policy_path=policy_path, policy_sha256=hashlib.sha256(policy).hexdigest())
            raw, gate_exit, decision, coverage, report = run_check(
                executable, work, repo, revision, config, policy
            )
            summary.update(check_exit=raw, gate_exit=gate_exit, decision=decision, coverage=coverage)
            if report is not None:
                dest = artifacts / "report.json"
                report.replace(dest)
                summary["report_sha256"] = digest_file(dest)
                if not render(executable, work, dest, "markdown", artifacts / "report.md", MAX_MARKDOWN):
                    message = "Migration Guard could not render the verified report as Markdown."
                if environ.get("DMG_CODE_QUALITY") == "true":
                    summary["code_quality_rendered"] = render(
                        executable, work, dest, "gitlab", artifacts / "gl-code-quality-report.json",
                        MAX_REPORT,
                    )
        except GateError as error:
            message = str(error)
        except (OSError, subprocess.SubprocessError, TypeError, ValueError, AttributeError):
            message = "Migration Guard could not prepare or check the exact pipeline inputs."

    markdown = artifacts / "report.md"
    if not markdown.is_file():
        if summary["gate_exit"] == 0:
            summary["gate_exit"] = 3
        write_fallback_markdown(markdown, message, summary["gate_exit"])
        print(message, file=sys.stderr)
    (artifacts / "provenance.json").write_text(
        json.dumps(summary, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    return summary["gate_exit"]


def main():
    try:
        require(len(sys.argv) == 2 and sys.argv[1] in {"protected", "advisory"},
                "Select the protected or advisory GitLab job.")
        require(sys.platform == "linux" and os.uname().machine == "x86_64",
                "The GitLab integration requires a Linux x86-64 runner.")
        # Limit each file the job or a descendant writes, including Git pack files.
        import resource
        limit = 512 * MIB
        soft, hard = resource.getrlimit(resource.RLIMIT_FSIZE)
        limits = [value for value in (soft, hard, limit) if value != resource.RLIM_INFINITY]
        resource.setrlimit(resource.RLIMIT_FSIZE, (min(limits), hard))
        return run_job(os.environ, Path(os.environ["CI_PROJECT_DIR"]), sys.argv[1])
    except GateError as error:
        print(str(error), file=sys.stderr)
    except (OSError, KeyError, ValueError):
        print("Migration Guard could not start the GitLab job.", file=sys.stderr)
    return 3


if __name__ == "__main__":
    sys.exit(main())
