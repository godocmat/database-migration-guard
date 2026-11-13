"""Shared binary identity and hashing for local Guard packages."""

import hashlib
import re

VERSION = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+(?:-[A-Za-z0-9][A-Za-z0-9.-]*)?")
TARGETS = {
    "x86_64-pc-windows-msvc": ".exe",
    "x86_64-unknown-linux-gnu": "",
    "aarch64-apple-darwin": "",
}
MAX_BINARY_BYTES = 64 * 1024 * 1024


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def binary_name(version, target):
    return f"dmg-{version}-{target}{TARGETS[target]}"
