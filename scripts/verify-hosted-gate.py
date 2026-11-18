"""Check retained GitHub or GitLab pilot artifacts against independently recorded inputs."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess


MIB = 1024 * 1024
EXIT_CODES = {"pass": 0, "denied": 1, "incomplete": 3}
GIT_ENV = {
    name: value for name, value in os.environ.items()
    if name.upper() in {"PATH", "PATHEXT", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR"}
}
GIT_ENV.update(
    GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_ALLOW_PROTOCOL="",
    GIT_TERMINAL_PROMPT="0", GIT_NO_LAZY_FETCH="1", GIT_OPTIONAL_LOCKS="0",
)


class EvidenceError(ValueError):
    """An acceptance mismatch with a reviewer-facing message."""


def require(condition, message):
    if not condition:
        raise EvidenceError(message)


def checked_fields(document, label, **expected):
    require(isinstance(document, dict), f"{label} must be a JSON object.")
    for key, value in expected.items():
        actual = document.get(key)
        require(
            key in document and type(actual) is type(value) and actual == value,
            f"{label} field {key} does not match {value!r}.",
        )


def sha1(value):
    if re.fullmatch(r"[0-9a-f]{40}", value) is None:
        raise argparse.ArgumentTypeError("Supply a full lowercase Git SHA-1 commit ID.")
    return value


def digest(value):
    if re.fullmatch(r"[0-9a-f]{64}", value) is None:
        raise argparse.ArgumentTypeError("Supply a lowercase SHA-256 digest.")
    return value


def repo_path(value):
    if len(value) > 512 or any(
        part in {".", ".."} or not re.fullmatch(r"[A-Za-z0-9_.-]+", part)
        for part in value.split("/")
    ):
        raise argparse.ArgumentTypeError("Supply a portable repository-relative path.")
    return value


def git(repo, *args):
    try:
        return subprocess.run(
            ["git", "--no-replace-objects", "--literal-pathspecs", "-C", str(repo), *args],
            env=GIT_ENV, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
            check=True, timeout=20,
        ).stdout
    except subprocess.SubprocessError as error:
        raise EvidenceError(f"Git {args[0]} failed. Retain the exact local commits before verifying.") from error


def git_content(repo, kind, oid):
    size = int(git(repo, "cat-file", "-s", oid))
    require(0 < size <= MIB, f"Git {kind} {oid} is empty or exceeds 1 MiB.")
    data = git(repo, "cat-file", kind, oid)
    require(len(data) == size, f"Git {kind} {oid} did not match its recorded size.")
    return data


def blob_digest(repo, commit, path):
    listing = git(repo, "ls-tree", "--full-tree", "-z", commit, "--", path)
    fields, _, name = listing.partition(b"\t")
    fields = fields.split()
    require(
        len(fields) == 3 and fields[0] in {b"100644", b"100755"} and fields[1] == b"blob"
        and name == path.encode("ascii") + b"\0",
        f"Expected an ordinary Git blob for {path} in {commit}.",
    )
    return hashlib.sha256(git_content(repo, "blob", fields[2].decode("ascii"))).hexdigest()


def verify_revisions(repo, base, source, scan, advisory):
    for oid in dict.fromkeys((base, source, scan)):
        require(git(repo, "cat-file", "-t", oid).strip() == b"commit", f"{oid} is not a Git commit.")
    if advisory:
        require(
            scan == source and base != source,
            "The fork scan must use the source commit and a distinct diff base.",
        )
        return
    require(len({base, source, scan}) == 3, "The target, source and merge commits must be distinct.")
    header = git_content(repo, "commit", scan).split(b"\n\n", 1)[0]
    parents = [line.removeprefix(b"parent ") for line in header.split(b"\n") if line.startswith(b"parent ")]
    require(
        parents == [base.encode("ascii"), source.encode("ascii")],
        "The merge commit must have the pinned target and source parents in that order.",
    )


def unique_object(pairs):
    result = dict(pairs)
    require(len(result) == len(pairs), "An artifact contains duplicate JSON fields.")
    return result


def artifact(directory, name, limit):
    path = directory / name
    info = path.lstat()
    require(
        stat.S_ISREG(info.st_mode) and 0 < info.st_size <= limit,
        f"Expected a regular {name} within its size limit.",
    )
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    require(0 < len(data) <= limit, f"Expected {name} within its size limit.")
    try:
        document = json.loads(data, object_pairs_hook=unique_object)
    except EvidenceError:
        raise
    except (ValueError, RecursionError) as error:
        raise EvidenceError(f"{name} must contain valid JSON.") from error
    return data, document


def verify_report(report, args, coverage, decision, policy_sha256):
    checked_fields(report, "Report", schema_version=1)
    checked_fields(report["coverage"], "Report coverage", status=coverage)
    migrations = report["migrations"]
    checked_fields(
        migrations["integrity"]["basis"], "Report integrity basis",
        kind="git", base_commit=args.base, head_commit=args.scan,
    )
    selected = migrations["selected_forwards"]
    require(
        isinstance(selected, list) and args.migration in selected,
        "The report did not select the expected pilot migration.",
    )
    matching = [item for item in migrations["inventory"] if item["path"] == args.migration]
    require(len(matching) == 1, "Expected one inventory entry for the pilot migration.")
    checked_fields(matching[0], "Pilot migration", digest=blob_digest(args.repo, args.scan, args.migration))

    if decision is None:
        checked_fields(report, "Fork report", decision=None)
    else:
        checked_fields(report["decision"], "Report decision", status=decision, policy_digest=policy_sha256)
    if decision == "denied":
        require(any(
            finding["rule_id"] == "PG001" and finding["span"]["path"] == args.migration
            for finding in report["findings"]
        ), "Expected a PG001 finding on the denied migration.")
    if coverage == "incomplete":
        require(any(
            gap["kind"] == "parse_error" and gap["scope"]["kind"] == "span"
            and gap["scope"]["span"]["path"] == args.migration
            for gap in report["coverage"]["gaps"]
        ), "Expected incomplete coverage from a parse error in the pilot migration.")


def verify(args):
    github = args.provider == "github"
    advisory = args.provider == "gitlab-fork"
    head_key, scan_key = ("pull_head_commit", "test_merge_commit") if github else ("source_commit", "scan_commit")
    coverage = "incomplete" if args.expect == "incomplete" else "complete"
    decision = None if advisory else args.expect
    code = EXIT_CODES[args.expect]

    _, provenance = artifact(args.artifacts, "provenance.json", 64 * 1024)
    checked_fields(
        provenance, "Provenance", schema_version=1, base_commit=args.base,
        binary_sha256=args.binary_sha256, check_exit=code, gate_exit=code,
        decision=decision, **{head_key: args.source, scan_key: args.scan},
    )
    if not github:
        checked_fields(
            provenance, "GitLab provenance",
            mode="advisory" if advisory else "protected", coverage=coverage,
        )

    verify_revisions(args.repo, args.base, args.source, args.scan, advisory)
    config_commit = args.scan if advisory else args.base
    checked_fields(
        provenance, "Provenance", config_path=args.config,
        config_sha256=blob_digest(args.repo, config_commit, args.config),
    )
    policy_sha256 = None
    if advisory:
        require(
            not {"policy_path", "policy_sha256"}.intersection(provenance),
            "Fork evidence must omit protected policy fields.",
        )
    else:
        policy_sha256 = blob_digest(args.repo, args.base, args.policy)
        checked_fields(provenance, "Provenance", policy_path=args.policy, policy_sha256=policy_sha256)

    data, report = artifact(args.artifacts, "report.json", 64 * MIB)
    checked_fields(provenance, "Provenance", report_sha256=hashlib.sha256(data).hexdigest())
    verify_report(report, args, coverage, decision, policy_sha256)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("provider", choices=("github", "gitlab", "gitlab-fork"))
    parser.add_argument("--repo", required=True, type=Path, help="local repository containing the verified commits")
    parser.add_argument(
        "--artifacts", required=True, type=Path, help="directory containing report.json and provenance.json",
    )
    parser.add_argument("--base", required=True, type=sha1, help="target commit, or the fork diff base")
    parser.add_argument("--source", required=True, type=sha1, help="pull or merge request source commit")
    parser.add_argument("--scan", required=True, type=sha1, help="checked merge commit, or the fork source commit")
    parser.add_argument("--binary-sha256", required=True, type=digest)
    parser.add_argument("--expect", choices=EXIT_CODES, required=True)
    parser.add_argument("--config", type=repo_path, default=".dmg/dmg.toml")
    parser.add_argument("--policy", type=repo_path, default=".dmg/policy.toml", help="protected cases only")
    parser.add_argument("--migration", type=repo_path, default="migrations/002_candidate.sql")
    args = parser.parse_args()
    if args.provider == "gitlab-fork" and args.expect == "denied":
        parser.error("Use pass or incomplete for a fork advisory scan.")
    try:
        verify(args)
    except (KeyError, TypeError, AttributeError):
        parser.exit(1, "Evidence check failed. A required artifact field is missing or malformed.\n")
    except (EvidenceError, OSError) as error:
        parser.exit(1, f"Evidence check failed. {error}\n")
    print(f"Verified {args.provider} {args.expect} evidence for {args.scan}.")


if __name__ == "__main__":
    main()
