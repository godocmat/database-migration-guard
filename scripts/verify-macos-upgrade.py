"""Verify a pinned macOS arm64 package and rollback under a process network sandbox."""

from __future__ import annotations

import argparse
import json
from pathlib import Path
import platform
import re
import shutil
import socket
import subprocess
import sys
import tarfile
import tempfile

from package_common import MAX_BINARY_BYTES, sha256


ROOT = Path(__file__).resolve().parents[1]
TARGET = "aarch64-apple-darwin"
PROFILE = "(version 1)\n(allow default)\n(deny network*)"
SANDBOX = "/usr/bin/sandbox-exec"
PROBE = """
import errno
import socket
import sys

try:
    with socket.create_connection(("127.0.0.1", int(sys.argv[1])), timeout=3):
        raise SystemExit("The sandbox allowed a loopback connection.")
except OSError as error:
    if error.errno not in (errno.EACCES, errno.EPERM):
        raise
    print(f"The same loopback connection was denied in the sandbox (errno {error.errno}).")
"""


def pinned(pattern, label):
    def parse(value):
        if not re.fullmatch(pattern, value):
            raise argparse.ArgumentTypeError(f"Expected {label} in lowercase hexadecimal.")
        return value
    return parse


def run(*command, work, capture=False):
    result = subprocess.run(
        command, cwd=work, check=True, timeout=300, stdin=subprocess.DEVNULL,
        capture_output=capture, text=True,
    )
    return result.stdout


def sandbox(*command, work, capture=False):
    return run(SANDBOX, "-p", PROFILE, *command, work=work, capture=capture)


def verify_pin(path, expected, limit=MAX_BINARY_BYTES):
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= limit:
        raise ValueError(f"{path.name} must be a regular file within the size limit.")
    if sha256(path) != expected:
        raise ValueError(f"{path.name} differs from its trusted SHA-256 pin.")


def extract_candidate(args, package):
    verify_pin(args.archive, args.archive_sha256, MAX_BINARY_BYTES + 16 * 1024 * 1024)
    package.mkdir()
    with tarfile.open(args.archive, "r:gz") as archive:
        members = []
        names = set()
        for member in archive:
            if (
                len(members) == 4 or not member.isfile() or member.name in names
                or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9.-]*", member.name)
                or not 0 < member.size <= MAX_BINARY_BYTES
            ):
                raise ValueError("Unexpected file in the native package archive.")
            members.append(member)
            names.add(member.name)
        if len(members) != 4:
            raise ValueError("The native package archive must contain four files.")
        archive.extractall(package, members, filter="data")


def require_network_denial(work):
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as listener:
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(3)
        address = listener.getsockname()
        with socket.create_connection(address, timeout=3), listener.accept()[0]:
            print("The loopback connection succeeded before applying the sandbox.", flush=True)
        sandbox(sys.executable, "-B", "-c", PROBE, str(address[1]), work=work)


def verify(args, work):
    package = work / "candidate"
    extract_candidate(args, package)
    verify_pin(args.previous, args.previous_sha256)
    print(f"macOS {platform.mac_ver()[0]} {platform.machine()}", flush=True)
    run("/usr/bin/sw_vers", work=work)
    run("git", "--version", work=work)
    run(sys.executable, "--version", work=work)
    print(f"Candidate archive SHA-256 {args.archive_sha256}", flush=True)
    print(f"Candidate manifest SHA-256 {args.manifest_sha256}", flush=True)
    require_network_denial(work)

    def python(script, *parameters):
        sandbox(sys.executable, "-B", ROOT / script, *parameters, work=work)

    python("scripts/stage-release.py", "verify", "--package", package,
           "--manifest-sha256", args.manifest_sha256)
    manifest = json.loads((package / "package.json").read_bytes())
    if manifest["target"] != TARGET or manifest["source_commit"] != args.source_commit:
        raise ValueError("The candidate must match the macOS arm64 target and reviewed commit.")
    candidate_digest = manifest["binary"]["sha256"]
    if candidate_digest == args.previous_sha256:
        raise ValueError("Use distinct candidate and rollback executables.")

    active = work / "dmg"
    staged = work / "dmg.next"
    for phase, source, expected in (
        ("install", args.previous, args.previous_sha256),
        ("upgrade", package / manifest["binary"]["name"], candidate_digest),
        ("rollback", args.previous, args.previous_sha256),
    ):
        shutil.copyfile(source, staged)
        verify_pin(staged, expected)
        staged.chmod(0o755)
        staged.replace(active)
        print(f"{phase} executable SHA-256 {expected}", flush=True)
        if sandbox("/usr/bin/lipo", "-archs", active, work=work, capture=True).strip() != "arm64":
            raise ValueError("The installed executable must be an arm64 Mach-O.")
        sandbox("/usr/bin/otool", "-L", active, work=work)
        sandbox(active, "--version", work=work)
        python("integrations/github/tests/replay.py", "--binary", active)
        if phase == "upgrade":
            python("scripts/smoke-release.py", active)
            python("integrations/gitlab/tests/replay.py", "--binary", active)
        verify_pin(active, expected)
        print(f"Verified {phase}.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    digest = pinned(r"[0-9a-f]{64}", "a SHA-256 digest")
    parser.add_argument("--archive", required=True, type=Path)
    parser.add_argument("--archive-sha256", required=True, type=digest)
    parser.add_argument("--manifest-sha256", required=True, type=digest)
    parser.add_argument("--source-commit", required=True, type=pinned(r"[0-9a-f]{40}|[0-9a-f]{64}", "a Git commit"))
    parser.add_argument("--previous", required=True, type=Path)
    parser.add_argument("--previous-sha256", required=True, type=digest)
    parser.add_argument("--scratch", required=True, type=Path, help="existing directory for temporary test files")
    args = parser.parse_args()
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        parser.error("The install verification requires native macOS arm64.")
    try:
        args.archive = args.archive.absolute()
        args.previous = args.previous.absolute()
        with tempfile.TemporaryDirectory(prefix="dmg-macos-install-", dir=args.scratch.resolve(strict=True)) as directory:
            verify(args, Path(directory))
    except (OSError, ValueError, KeyError, TypeError, tarfile.TarError, subprocess.SubprocessError) as error:
        parser.exit(1, f"macOS package verification failed. {error}\n")


if __name__ == "__main__":
    main()