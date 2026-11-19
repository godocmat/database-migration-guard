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
NATIVE_RELEASE_FORMATS = {
    "x86_64-unknown-linux-gnu": ".tar.gz",
    "x86_64-pc-windows-msvc": ".zip",
}
DISTRIBUTION_DOCUMENTS = {
    "license": ("LICENSE", 64 * 1024),
    "notices": ("THIRD-PARTY-NOTICES.txt", 16 * 1024 * 1024),
    "rust_stdlib_copyright": ("RUST-STDLIB-COPYRIGHT.html", 4 * 1024 * 1024),
}


def sha256(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def binary_name(version, target):
    return f"dmg-{version}-{target}{TARGETS[target]}"


def executable_name(target):
    return f"dmg{TARGETS[target]}"


def release_archive_name(version, target):
    return f"dmg-{version}-{target}{NATIVE_RELEASE_FORMATS[target]}"
