"""Verify a pinned Windows release through install, upgrade, and rollback."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import sys
import tempfile

from package_common import MAX_BINARY_BYTES, binary_name, sha256


ROOT = Path(__file__).resolve().parents[1]
TARGET = "x86_64-pc-windows-msvc"
MAX_MANIFEST_BYTES = 64 * 1024


def digest(value):
    if not re.fullmatch(r"[0-9a-f]{64}", value):
        raise argparse.ArgumentTypeError("Expected a lowercase SHA-256 digest.")
    return value


def run(*command, env, cwd):
    subprocess.run(command, env=env, cwd=cwd, check=True, timeout=300, stdin=subprocess.DEVNULL)


def install(source, expected, active):
    if source.is_symlink() or not source.is_file() or not 0 < source.stat().st_size <= MAX_BINARY_BYTES:
        raise ValueError("The install input must be a regular executable within the size limit.")
    staged = active.with_name("staged.exe")
    shutil.copyfile(source, staged)
    if sha256(staged) != expected:
        raise ValueError("The copied executable differs from its SHA-256 pin.")
    staged.replace(active)


def verify(args, work):
    system = Path(os.environ["SystemRoot"])
    git = args.git.resolve(strict=True)
    if not args.git.is_absolute() or git.name.lower() != "git.exe" or not git.is_file():
        raise ValueError("Supply the absolute path to the reviewed Git executable.")
    env = {name: os.environ[name] for name in ("SystemRoot", "WINDIR", "COMSPEC") if name in os.environ}
    env.update(
        PATH=os.pathsep.join(map(str, (git.parent, system / "System32", system))),
        HOME=str(work), USERPROFILE=str(work), TMP=str(work), TEMP=str(work), TMPDIR=str(work),
        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0",
    )

    def python(script, *parameters):
        run(sys.executable, "-B", ROOT / script, *parameters, env=env, cwd=work)

    package = args.package.resolve(strict=True)
    python("scripts/stage-release.py", "verify", "--package", package,
           "--manifest-sha256", args.manifest_sha256)
    with (package / "package.json").open("rb") as stream:
        raw = stream.read(MAX_MANIFEST_BYTES + 1)
    if len(raw) > MAX_MANIFEST_BYTES or hashlib.sha256(raw).hexdigest() != args.manifest_sha256:
        raise ValueError("package.json differs from its trusted pin.")
    manifest = json.loads(raw)
    if manifest["target"] != TARGET:
        raise ValueError("The candidate package must target Windows x86-64.")
    candidate = manifest["binary"]["sha256"]
    if candidate == args.previous_sha256:
        raise ValueError("Use distinct candidate and rollback executables.")

    print(f"Windows {platform.win32_ver()[1]} {platform.machine()}", flush=True)
    print(f"Candidate manifest SHA-256 {args.manifest_sha256}", flush=True)
    run(git, "--version", env=env, cwd=work)
    active = work / "dmg.exe"
    phases = (
        ("install", args.previous, args.previous_sha256),
        ("upgrade", package / binary_name(manifest["version"], TARGET), candidate),
        ("rollback", args.previous, args.previous_sha256),
    )
    for phase, binary, expected in phases:
        install(binary, expected, active)
        print(f"{phase} SHA-256 {expected}", flush=True)
        run(active, "--version", env=env, cwd=work)
        python("integrations/github/tests/replay.py", "--binary", active)
        if phase == "upgrade":
            python("scripts/smoke-release.py", active)
            python("integrations/gitlab/tests/replay.py", "--binary", active)
        if sha256(active) != expected:
            raise ValueError("The installed executable changed during verification.")
        print(f"Verified {phase}.", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", required=True, type=Path)
    parser.add_argument("--manifest-sha256", required=True, type=digest)
    parser.add_argument("--previous", required=True, type=Path)
    parser.add_argument("--previous-sha256", required=True, type=digest)
    parser.add_argument("--git", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path, help="existing directory for temporary test files")
    args = parser.parse_args()
    if platform.system() != "Windows" or platform.machine().upper() != "AMD64":
        parser.error("The install verification requires native Windows x86-64.")
    try:
        scratch = args.scratch.resolve(strict=True)
        with tempfile.TemporaryDirectory(prefix="dmg-windows-install-", dir=scratch) as directory:
            verify(args, Path(directory))
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Windows package verification failed. {error}\n")


if __name__ == "__main__":
    main()
