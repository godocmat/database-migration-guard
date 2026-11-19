"""Prepare and run a Database Migration Guard pull request check."""

from __future__ import annotations

import base64
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile
from pathlib import Path
from urllib.parse import urlsplit


SHA = re.compile(r"[a-f0-9]{40}")
DIGEST = re.compile(r"[a-f0-9]{64}")
SLUG = re.compile(r"[A-Za-z0-9_-]+/[A-Za-z0-9_.-]+")
SAFE_PATH_PART = re.compile(r"[A-Za-z0-9_.-]+")
MIB = 1024 * 1024
MAX_PREPARE_FILE_BYTES = 512 * MIB


class GateError(Exception):
    """An error whose message is safe to write to a workflow log."""


def require(condition, message):
    if not condition:
        raise GateError(message)


def file_digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def child_env(home):
    # Runtime processes inherit only path, operating-system and temp settings.
    allowed = {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR"}
    env = {name: value for name, value in os.environ.items() if name.upper() in allowed}
    env.update(HOME=str(home), USERPROFILE=str(home), XDG_CONFIG_HOME=str(home))
    return env


def git(root, *args, timeout=20, fetch_token=None, local_fetch=False, capture=True):
    env = child_env(root.parent / "home")
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_TERMINAL_PROMPT="0", GIT_OPTIONAL_LOCKS="0", GIT_NO_LAZY_FETCH="1",
               GIT_ALLOW_PROTOCOL="file" if local_fetch else "https" if fetch_token else "")
    if fetch_token:
        auth = base64.b64encode(f"x-access-token:{fetch_token}".encode("ascii")).decode("ascii")
        env.update(GIT_CONFIG_COUNT="1", GIT_CONFIG_KEY_0="http.extraHeader",
                   GIT_CONFIG_VALUE_0=f"Authorization: Basic {auth}")
    return subprocess.run(
        ["git", "--no-replace-objects", "--literal-pathspecs", "-C", str(root), *args],
        env=env, stdin=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
        stdout=subprocess.PIPE if capture else subprocess.DEVNULL, check=True, timeout=timeout,
    ).stdout


def context(environ):
    require(environ.get("GITHUB_EVENT_NAME") == "pull_request",
            "The pilot gate requires a pull_request event.")
    require(environ.get("GITHUB_SERVER_URL") == "https://github.com",
            "The pilot gate targets GitHub.com.")
    with Path(environ["GITHUB_EVENT_PATH"]).open("rb") as stream:
        event_bytes = stream.read(MIB + 1)
    require(len(event_bytes) <= MIB, "The pull request event exceeds the pilot size limit.")
    event = json.loads(event_bytes)
    pull = event["pull_request"]
    number = event["number"]
    repo = environ["GITHUB_REPOSITORY"]
    base = pull["base"]["sha"]
    head = pull["head"]["sha"]
    merge = environ["GITHUB_SHA"]
    require(type(number) is int and 0 < number < 10**10, "Invalid pull request number.")
    require(SLUG.fullmatch(repo) and repo.split("/")[-1] not in {".", ".."}
            and pull["base"]["repo"]["full_name"].lower() == repo.lower(),
            "The event base repository does not match the workflow repository.")
    require(all(isinstance(value, str) and SHA.fullmatch(value) for value in (base, head, merge)),
            "The pull request event must supply exact Git commit IDs.")
    require(base != head and merge not in {base, head}, "The pull request test merge is unavailable.")
    require(environ.get("GITHUB_REF") == f"refs/pull/{number}/merge",
            "The pull request test-merge ref is unavailable.")
    return {"repository": repo, "pull_request": number, "base_commit": base,
            "pull_head_commit": head, "test_merge_commit": merge}


def verify_merge(repo, revision):
    parents = git(repo, "rev-list", "--parents", "-n", "1", "refs/dmg/test-merge", "--")
    expected = [revision[key] for key in ("test_merge_commit", "base_commit", "pull_head_commit")]
    require(parents.split() == [value.encode("ascii") for value in expected],
            "The test merge changed or its parents differ from the event. Retry the current revision.")


def protected_blob(repo, base, path):
    parts = path.split("/")
    require(len(path) <= 512 and all(SAFE_PATH_PART.fullmatch(part) and part not in {".", ".."}
                                      for part in parts),
            "Use a portable repository-relative path for each protected input.")
    listing = git(repo, "ls-tree", "--full-tree", "-z", base, "--", path)
    fields, separator, name = listing.partition(b"\t")
    parts = fields.split()
    require(len(parts) == 3 and separator and name == path.encode("ascii") + b"\0",
            "A protected config or policy file is missing from the base commit.")
    mode, kind, oid = parts
    require(kind == b"blob"
            and mode in {b"100644", b"100755"},
            "A protected config or policy file must be an ordinary Git blob.")
    object_id = oid.decode("ascii")
    require(SHA.fullmatch(object_id), "Invalid object ID for a protected input.")
    size = int(git(repo, "cat-file", "-s", object_id))
    require(0 < size <= MIB, "A protected config or policy file exceeds the pilot size limit or is empty.")
    content = git(repo, "cat-file", "blob", object_id)
    require(len(content) == size, "The protected input did not match its Git object size.")
    return content


def download_binary(directory, url, digest):
    parsed = urlsplit(url)
    require(parsed.scheme == "https" and parsed.hostname and not parsed.username
            and not parsed.password and not parsed.fragment and not parsed.query,
            "Use an HTTPS pilot asset URL without credentials, query parameters or fragments.")
    require(DIGEST.fullmatch(digest), "Provide the independently pinned SHA-256 of the pilot binary.")
    binary = directory / "dmg"
    subprocess.run(
        ["curl", "--disable", "--silent", "--fail", "--location", "--max-redirs", "5",
         "--proto", "=https", "--proto-redir", "=https", "--max-time", "120",
         "--max-filesize", str(64 * MIB), "--output", str(binary), "--", url],
        env=child_env(directory / "home"), stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=True, timeout=130,
    )
    require(0 < binary.stat().st_size <= 64 * MIB and file_digest(binary) == digest,
            "The downloaded pilot binary failed size or checksum verification.")
    binary.chmod(0o700)
    return binary


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def fetch_merge(repo, remote, revision, *, token=None, local=False):
    git(repo, "init", "--bare", "--quiet")
    ref = f"refs/pull/{revision['pull_request']}/merge:refs/dmg/test-merge"
    git(repo, "-c", "http.followRedirects=false", "fetch", "--quiet", "--depth=2", "--no-tags",
        "--no-recurse-submodules", "--no-auto-maintenance", "--no-write-fetch-head", remote, ref,
        timeout=120, fetch_token=token, local_fetch=local, capture=False)
    verify_merge(repo, revision)


def save_protected_inputs(directory, repo, revision, config_path, policy_path):
    for role, source_path in (("config", config_path), ("policy", policy_path)):
        content = protected_blob(repo, revision["base_commit"], source_path)
        (directory / f"{role}.toml").write_bytes(content)
        revision[f"{role}_path"] = source_path
        revision[f"{role}_sha256"] = hashlib.sha256(content).hexdigest()


def prepare(environ):
    require(environ.get("RUNNER_OS") == "Linux" and environ.get("RUNNER_ARCH") == "X64",
            "The hosted pilot expects a Linux x86-64 runner.")
    # Bound pack files before Git writes them, including objects outside the migration roots.
    import resource
    soft, hard = resource.getrlimit(resource.RLIMIT_FSIZE)
    limits = [value for value in (soft, hard, MAX_PREPARE_FILE_BYTES) if value != resource.RLIM_INFINITY]
    resource.setrlimit(resource.RLIMIT_FSIZE, (min(limits), hard))
    revision = context(environ)
    directory = Path(tempfile.mkdtemp(prefix="dmg-pilot-", dir=environ["RUNNER_TEMP"]))
    (directory / "home").mkdir()
    (directory / "reports").mkdir()
    repo = directory / "objects.git"
    repo.mkdir()
    token = environ["DMG_FETCH_TOKEN"]
    require(token, "The workflow needs its built-in read-only token for the object fetch.")
    remote = f"https://github.com/{revision['repository']}.git"
    fetch_merge(repo, remote, revision, token=token)
    save_protected_inputs(directory, repo, revision, environ["DMG_CONFIG_PATH"], environ["DMG_POLICY_PATH"])
    revision["binary_sha256"] = environ["DMG_BINARY_SHA256"]
    binary = download_binary(directory, environ["DMG_BINARY_URL"], revision["binary_sha256"])
    revision["binary_name"] = binary.name
    write_json(directory / "inputs.json", revision)
    with Path(environ["GITHUB_OUTPUT"]).open("a", encoding="utf-8") as stream:
        stream.write(f"directory={directory}\n")


def verified_decision(report, revision):
    decision = report.get("decision") or {}
    status = decision.get("status")
    if (type(report.get("schema_version")) is not int or report["schema_version"] != 1
            or status not in {"pass", "denied", "incomplete"}
            or decision.get("policy_digest") != revision["policy_sha256"]):
        return None
    integrity = report["migrations"]["integrity"]
    if integrity is None:
        return status if status == "incomplete" else None
    basis = integrity["basis"]
    if (basis.get("kind") != "git" or basis.get("base_commit") != revision["base_commit"]
            or basis.get("head_commit") != revision["test_merge_commit"]):
        return None
    if status == "pass" and report["coverage"]["status"] != "complete":
        return None
    return status


def render(directory, binary, report, summary_path):
    results = directory / "reports"
    markdown = directory / "rendered.md"
    args = [str(binary), "report", "--input", str(report), "--format"]
    environment = child_env(directory / "home")
    kwargs = {"cwd": directory, "env": environment, "stdin": subprocess.DEVNULL,
              "stderr": subprocess.DEVNULL, "check": True, "timeout": 30}
    ok = True
    try:
        subprocess.run([*args, "markdown", "--output", str(markdown)],
                       stdout=subprocess.DEVNULL, **kwargs)
        require(markdown.stat().st_size <= 900 * 1024, "The rendered summary exceeds the pilot limit.")
        report_md = results / "report.md"
        markdown.replace(report_md)
        if summary_path:
            with Path(summary_path).open("ab") as summary:
                summary.write(report_md.read_bytes() + b"\n")
    except (OSError, subprocess.SubprocessError, GateError):
        ok = False
    try:
        subprocess.run([*args, "github"], **kwargs)
    except (OSError, subprocess.SubprocessError):
        ok = False
    return ok


def check(directory, summary_path=None):
    revision = json.loads((directory / "inputs.json").read_text(encoding="utf-8"))
    binary = directory / revision["binary_name"]
    config = directory / "config.toml"
    policy = directory / "policy.toml"
    reports = directory / "reports"
    scratch = directory / "check.json"
    require(all(file_digest(path) == revision[f"{role}_sha256"]
                for role, path in (("binary", binary), ("config", config), ("policy", policy))),
            "A verified scanner or protected input changed before the check.")
    verify_merge(directory / "objects.git", revision)

    try:
        raw = subprocess.run(
            [str(binary), "check", "--repo", str(directory / "objects.git"),
             "--config", str(config), "--policy", str(policy), "--base", revision["base_commit"],
             "--head", revision["test_merge_commit"], "--format", "json", "--output", str(scratch)],
            cwd=directory, env=child_env(directory / "home"), stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=120,
        ).returncode
    except (OSError, subprocess.TimeoutExpired):
        raw = None

    decision = None
    report = reports / "report.json"
    if scratch.is_file() and scratch.stat().st_size <= 64 * MIB:
        try:
            decision = verified_decision(json.loads(scratch.read_bytes()), revision)
        except (ValueError, TypeError, KeyError, AttributeError):
            pass
        if decision:
            scratch.replace(report)

    status_matches = raw == 0 and decision == "pass"
    gate_exit = 0 if status_matches else raw if raw is not None and 1 <= raw <= 125 else 3
    provenance = {"schema_version": 1, **revision, "check_exit": raw,
                  "gate_exit": gate_exit, "decision": decision,
                  "report_sha256": file_digest(report) if decision else None}
    write_json(reports / "provenance.json", provenance)

    rendered = bool(decision) and render(directory, binary, report, summary_path)
    if not rendered:
        message = "Migration Guard report rendering unavailable." if decision else (
            "Migration Guard did not produce a verified decision for this revision."
        )
        print(message, file=sys.stderr)
        if summary_path:
            try:
                with Path(summary_path).open("a", encoding="utf-8") as summary:
                    summary.write(f"\n{message}\n\nGate exit: `{gate_exit}`. See the provenance artifact.\n")
            except OSError:
                pass
    return gate_exit


def main():
    try:
        if sys.argv[1:] == ["prepare"]:
            prepare(os.environ)
            return 0
        if sys.argv[1:] == ["check"]:
            return check(Path(os.environ["DMG_GATE_STATE"]), os.environ.get("GITHUB_STEP_SUMMARY"))
        raise GateError("Use the prepare or check stage.")
    except GateError as error:
        print(str(error), file=sys.stderr)
    except (OSError, subprocess.SubprocessError, ValueError, TypeError, KeyError, AttributeError):
        print("Migration Guard could not prepare or check the exact pull request inputs.", file=sys.stderr)
    return 3


if __name__ == "__main__":
    sys.exit(main())
