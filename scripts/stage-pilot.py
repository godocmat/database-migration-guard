"""Stage a verified local Database Migration Guard pilot executable."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import tempfile
from pathlib import Path

from package_common import MAX_BINARY_BYTES, TARGETS, VERSION, binary_name, sha256


def command(*args, cwd=None):
    return subprocess.run(args, cwd=cwd, check=True, timeout=30, stdin=subprocess.DEVNULL,
                          stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True).stdout.strip()


def stage(source, output, target, binary):
    source = source.resolve(strict=True)
    binary = binary.resolve(strict=True)
    output = output.absolute()
    output_parent = output.parent.resolve(strict=True)
    if output.exists() or output.is_symlink():
        raise ValueError("Choose a new output directory for the pilot package.")
    if not binary.is_file() or not 0 < binary.stat().st_size <= MAX_BINARY_BYTES:
        raise ValueError("The pilot binary must be an ordinary file of at most 64 MiB.")

    commit = command("git", "rev-parse", "--verify", "HEAD^{commit}", cwd=source)
    if not re.fullmatch(r"[a-f0-9]{40}|[a-f0-9]{64}", commit):
        raise ValueError("The source checkout did not supply an exact commit ID.")
    if command("git", "status", "--porcelain", "--untracked-files=normal", cwd=source):
        raise ValueError("Stage the pilot from a clean checkout of the reviewed build revision.")
    lock = source / "Cargo.lock"
    toolchain = source / "rust-toolchain.toml"
    if not lock.is_file() or not toolchain.is_file():
        raise ValueError("The source checkout must contain Cargo.lock and rust-toolchain.toml.")

    with tempfile.TemporaryDirectory(prefix="dmg-stage-", dir=output_parent) as scratch:
        work = Path(scratch)
        bundle = work / "package"
        bundle.mkdir()
        suffix = TARGETS[target]
        executable = bundle / f"dmg{suffix}"
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        fields = command(str(executable), "--version", cwd=work).split()
        if len(fields) != 2 or fields[0] != "dmg" or not VERSION.fullmatch(fields[1]):
            raise ValueError("The staged executable did not report the expected dmg version.")
        version = fields[1]
        executable = executable.rename(bundle / binary_name(version, target))
        digest = sha256(executable)

        manifest = {"schema_version": 1, "name": "dmg", "version": version,
                    "target": target, "source_commit": commit,
                    "cargo_lock_sha256": sha256(lock), "rust_toolchain_sha256": sha256(toolchain),
                    "binary": {"name": executable.name, "sha256": digest, "bytes": executable.stat().st_size}}
        (bundle / "package.json").write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n",
                                             encoding="utf-8", newline="\n")
        (bundle / "SHA256SUMS").write_text(f"{digest}  {executable.name}\n", encoding="ascii", newline="\n")
        if output.exists() or output.is_symlink():
            raise ValueError("The pilot package destination was created while staging.")
        bundle.rename(output)
    return manifest


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--source", type=Path, default=Path.cwd(), help="clean source checkout used for the build")
    parser.add_argument("--target", choices=TARGETS, required=True, help="native build target")
    parser.add_argument("--binary", type=Path, required=True, help="locally built dmg executable")
    parser.add_argument("--output", type=Path, required=True, help="new directory for the staged package")
    args = parser.parse_args()
    try:
        manifest = stage(args.source, args.output, args.target, args.binary)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Pilot staging failed: {error}\n")
    print(f"Staged {manifest['binary']['name']}")
    print(f"SHA-256 {manifest['binary']['sha256']}")


if __name__ == "__main__":
    main()
