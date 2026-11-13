"""Stage the locked Wolfi package archive for a Linux x86-64 Guard image."""

from __future__ import annotations

import argparse
from pathlib import Path
import re
import shutil
import tempfile
from urllib.request import urlopen

from package_common import sha256

LOCKS = Path(__file__).resolve().parents[1] / "integrations" / "wolfi"
REPOSITORY = "https://packages.wolfi.dev/os/x86_64/"
INDEX = "APKINDEX.tar.gz"
LOCK_LINE = re.compile(r"([0-9a-f]{64})  (APKINDEX\.tar\.gz|[a-z0-9][a-z0-9._+-]*\.apk)")
MAX_ARCHIVE_BYTES = 64 * 1024 * 1024


def read_lock(role):
    lines = (LOCKS / "cli.sha256").read_text(encoding="ascii").splitlines()
    if role == "gitlab":
        lines.extend((LOCKS / "gitlab-extra.sha256").read_text(encoding="ascii").splitlines())
    entries = {}
    for line in lines:
        match = LOCK_LINE.fullmatch(line)
        if not match or match[2] in entries:
            raise ValueError("Invalid entry in the Wolfi package lock.")
        digest, name = match.groups()
        entries[name] = digest
    if INDEX not in entries or len(entries) < 2:
        raise ValueError("The Wolfi lock needs an index and package archives.")
    return entries


def stage(role, output, snapshot):
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError("Choose a new output directory for the container packages.")
    entries = read_lock(role)
    with tempfile.TemporaryDirectory(prefix="dmg-wolfi-", dir=output.parent) as scratch:
        stage_root = Path(scratch) / "runtime-packages"
        packages = stage_root / "x86_64"
        packages.mkdir(parents=True)
        for name, expected in entries.items():
            destination = packages / name
            if snapshot is None:
                with urlopen(REPOSITORY + name, timeout=60) as data:
                    destination.write_bytes(data.read(MAX_ARCHIVE_BYTES + 1))
            else:
                source = snapshot / "x86_64" / name
                if not source.is_file() or source.stat().st_size > MAX_ARCHIVE_BYTES:
                    raise ValueError(f"Expected a bounded regular archive at {source}.")
                shutil.copyfile(source, destination)
            if destination.stat().st_size > MAX_ARCHIVE_BYTES or sha256(destination) != expected:
                raise ValueError(f"SHA-256 verification failed for {name}. Use the reviewed snapshot.")
        (packages / "SHA256SUMS").write_text(
            "".join(f"{digest}  {name}\n" for name, digest in entries.items()), encoding="ascii", newline="\n",
        )
        if output.exists() or output.is_symlink():
            raise ValueError("Choose a new output directory for the container packages.")
        stage_root.rename(output)
    print(f"Staged {len(entries) - 1} locked packages and the signed index in {output}.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("role", choices=("cli", "gitlab"))
    parser.add_argument("output", type=Path)
    parser.add_argument("--from-dir", type=Path, help="Read the retained snapshot instead of downloading it.")
    args = parser.parse_args()
    try:
        stage(args.role, args.output, args.from_dir)
    except (OSError, ValueError) as error:
        parser.exit(1, f"Could not stage container packages. {error}\n")


if __name__ == "__main__":
    main()
