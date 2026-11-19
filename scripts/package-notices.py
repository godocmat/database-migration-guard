"""Assemble the reviewed third-party notices for a staged x86-64 native package."""

from __future__ import annotations

import argparse
from hashlib import sha256 as digest
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import sys
import tarfile
import tomllib

from package_common import sha256

ROOT = Path(__file__).resolve().parents[1]
TARGETS = {"x86_64-unknown-linux-gnu", "x86_64-pc-windows-msvc"}
LOCK_DIGEST = "3d66cf1d4edfb7abd9629fdefef5aed09d1db5ee21bc494660a190afc5ecee8c"
RUST_VERSION = "1.100.0"
RUST_REPORT_DIGEST = "626556344c12679d7cab2c1aa2ce80de01937b62c2c88c62f0650a469be18799"
REGISTRY_SOURCE = "registry+https://github.com/rust-lang/crates.io-index"
CRATE_NAME = re.compile(r"[A-Za-z0-9_-]+")
CRATE_VERSION = re.compile(r"[0-9][A-Za-z0-9.+-]*")
NOTICE_NAME = re.compile(
    r"^(licen[cs]e|unlicense|copyright|copying|notice|authors|third.party.notices|patents)(?:$|[_.-])",
    re.IGNORECASE,
)
VENDORED = {"libpg_query", "postgresql-parser", "xxhash", "upb", "utf8-range", "ring-native"}
PARSER_HEADERS = (
    ("vendor/xxhash/xxhash.c", b"/*", b"*/"),
    ("vendor/xxhash/xxhash.h", b"/*", b"*/"),
    ("src/postgres/src_port_snprintf.c", b"/*\n * Copyright (c) 1983, 1995, 1996 Eric P. Allman", b"*/"),
    ("src/postgres/src_port_strlcpy.c", b"/*\n * Copyright (c) 1998 Todd C. Miller", b"*/"),
    ("src/postgres/include/common/hashfn_unstable.h", b"/* The MIT License", b"*/"),
    ("src/postgres/include/lib/hyperloglog.h", b"/*\n * Copyright (c) 2013 Hideaki Ohno", b"*/"),
    ("src/postgres/include/regex/regex.h", b"/*\n * regular expressions", b"*/"),
    ("src/postgres/src_backend_parser_gram.c", b"/* Skeleton implementation for Bison",
     b"version 2.2 of Bison.  */"),
    ("src/postgres/src_pl_plpgsql_src_pl_gram.c", b"/* Skeleton implementation for Bison",
     b"version 2.2 of Bison.  */"),
)


def git_blob(commit, filename):
    return subprocess.check_output(
        ["git", "--no-replace-objects", "cat-file", "blob", f"{commit}:{filename}"],
        cwd=ROOT, timeout=30, stdin=subprocess.DEVNULL,
    )


def release_input(package, pin):
    subprocess.run(
        [sys.executable, "-B", ROOT / "scripts/stage-release.py", "verify",
         "--package", package, "--manifest-sha256", pin],
        check=True, cwd=ROOT, timeout=30, stdin=subprocess.DEVNULL,
    )
    manifest = json.loads((package / "package.json").read_bytes())
    target = manifest["target"]
    commit = manifest["source_commit"]
    if target not in TARGETS:
        raise ValueError("Native notices are reviewed for Linux and Windows x86-64.")
    lock = git_blob(commit, "Cargo.lock")
    if digest(lock).hexdigest() != manifest["cargo_lock_sha256"] or manifest["cargo_lock_sha256"] != LOCK_DIGEST:
        raise ValueError("Review native redistribution notices for this Cargo.lock.")
    toolchain = git_blob(commit, "rust-toolchain.toml")
    if (
        digest(toolchain).hexdigest() != manifest["rust_toolchain_sha256"]
        or tomllib.loads(toolchain.decode())["toolchain"]["channel"] != RUST_VERSION
        or not manifest["rustc"].startswith(f"rustc {RUST_VERSION} ")
    ):
        raise ValueError("Review native redistribution notices for this Rust toolchain.")

    bom = json.loads((package / manifest["sbom"]["name"]).read_bytes())
    metadata = {row["name"]: row["value"] for row in bom["metadata"]["properties"]}
    if (
        (bom["bomFormat"], bom["specVersion"]) != ("CycloneDX", "1.5")
        or metadata["dmg:source-commit"] != commit
        or metadata["cdx:rustc:sbom:target:triple"] != target
        or bom["metadata"]["component"]["hashes"] != [
            {"alg": "SHA-256", "content": manifest["binary"]["sha256"]}
        ]
    ):
        raise ValueError("The SBOM must describe the exact staged build.")
    return manifest, bom, tomllib.loads(lock.decode())["package"]


def registry_inventory(bom, lock_packages):
    pins = {
        (row["name"], row["version"]): row["checksum"]
        for row in lock_packages if row.get("source") == REGISTRY_SOURCE
    }
    registry = {}
    vendored = set()
    for component in bom["components"]:
        ref = component["bom-ref"]
        role = next(p["value"] for p in component["properties"] if p["name"] == "dmg:artifact-role")
        if ref.startswith("dmg:vendored:") and role == "vendored-target-source":
            vendored.add(ref.removeprefix("dmg:vendored:"))
        elif ref.startswith("dmg:workspace:"):
            continue
        elif component.get("purl", "").startswith("pkg:cargo/") and role in {"target-source", "build-only"}:
            name, version = component["name"], component["version"]
            if (
                not CRATE_NAME.fullmatch(name) or not CRATE_VERSION.fullmatch(version)
                or (name, version) in registry or (name, version) not in pins
            ):
                raise ValueError(f"Expected a unique locked crates.io component for {ref}.")
            registry[name, version] = (pins[name, version], role, component["licenses"])
        else:
            raise ValueError(f"Unrecognized SBOM component for native notices: {ref}")
    if vendored != VENDORED or not {("pg_query", "18.1.0"), ("ring", "0.17.14")} <= registry.keys():
        raise ValueError("Review the vendored native inputs for this build.")
    return registry


def cache_archive(cache, crate, pin):
    for path in sorted(cache.glob(f"*/{crate}.crate")):
        if path.is_file() and sha256(path) == pin:
            return path
    raise ValueError(f"Missing checksum-verified Cargo archive for {crate}. Run cargo fetch --locked.")


def member_bytes(archive, member):
    if isinstance(member, str):
        member = archive.getmember(member)
    if not member.isfile() or not 0 < member.size <= 16 * 1024 * 1024:
        raise ValueError(f"Unusable notice input: {member.name}")
    with archive.extractfile(member) as stream:
        return stream.read()


def add_text(sections, name, content):
    sections.extend([f"--- {name} ---\n".encode(), content, b"\n\n"])


def parser_notices(archive, prefix, sections):
    for path, start, end in PARSER_HEADERS:
        name = f"libpg_query/{path}"
        content = member_bytes(archive, prefix + name)
        begin = content.index(start)
        add_text(sections, name, content[begin:content.index(end, begin) + len(end)])


def ring_notices(archive, prefix, sections):
    generators = set()
    for member in sorted(archive.getmembers(), key=lambda entry: entry.name):
        if not member.isfile() or not member.name.startswith(prefix):
            continue
        name = member.name.removeprefix(prefix)
        is_generator = "x86_64" in name and name.endswith(".pl")
        if not is_generator and PurePosixPath(name).suffix not in {".c", ".h", ".rs"}:
            continue
        source = member_bytes(archive, member)
        head = re.match(
            rb"(?:[ \t\r\n]*(?://[^\r\n]*|#[^\r\n]*|/\*[\s\S]*?\*/))+",
            source,
        )
        if head and (b"copyright" in head[0].lower() or b"SPDX-License-Identifier" in head[0]):
            add_text(sections, name, head[0])
            if is_generator:
                generators.add(name)
    if len(generators) != 12:
        raise ValueError("Review the ring x86-64 assembly generator notices.")


def crate_notices(cache, name, version, pin, role, declared, sections):
    crate = f"{name}-{version}"
    prefix = crate + "/"
    with tarfile.open(cache_archive(cache, crate, pin), "r:gz") as archive:
        cargo = tomllib.loads(member_bytes(archive, prefix + "Cargo.toml").decode())["package"]
        license_expression = cargo["license"].replace("/", " OR ")
        if (
            (cargo["name"], cargo["version"]) != (name, version)
            or declared != [{"expression": license_expression}]
        ):
            raise ValueError(f"SBOM license or identity differs from the pinned sources for {crate}.")

        sections.append(
            (f"===== {name} {version} ({role}) =====\n"
             f"License expression: {license_expression}\n"
             f"Exact source archive: https://static.crates.io/crates/{name}/{crate}.crate\n"
             f"Source archive SHA-256: {pin}\n\n").encode()
        )
        members = sorted(
            (entry for entry in archive.getmembers() if entry.isfile() and entry.name.startswith(prefix)
             and NOTICE_NAME.match(PurePosixPath(entry.name).name)),
            key=lambda entry: entry.name,
        )
        if not members and crate != "pg_query-18.1.0":
            raise ValueError(f"Review the missing standalone notices in {crate}.")
        for member in members:
            add_text(sections, member.name.removeprefix(prefix), member_bytes(archive, member))

        if crate == "pg_query-18.1.0":
            parser_notices(archive, prefix, sections)
        if crate == "ring-0.17.14":
            ring_notices(archive, prefix, sections)


def generate(package, pin, output, cargo_home, rust_sysroot):
    manifest, bom, lock_packages = release_input(package, pin)
    registry = registry_inventory(bom, lock_packages)
    commit = manifest["source_commit"]
    sections = [
        (f"Database Migration Guard {manifest['version']} third-party notices\n"
         f"Build target: {manifest['target']}\n"
         f"Project source: https://github.com/godocmat/database-migration-guard/tree/{commit}\n"
         f"Source archive: https://github.com/godocmat/database-migration-guard/archive/{commit}.tar.gz\n"
         f"Cargo.lock SHA-256: {manifest['cargo_lock_sha256']}\n\n"
         "These notices cover the Cargo sources selected by the build, including build-only dependencies,\n"
         "the identified vendored sources, and the Rust standard library. Host Git and system libraries\n"
         "are supplied by the installation environment. See the accompanying RUST-STDLIB-COPYRIGHT.html\n"
         "for the standard-library report, and dmg.cdx.json for build-source roles and dependencies.\n\n").encode()
    ]
    cache = cargo_home / "registry/cache"
    for (name, version), (archive_pin, role, licenses) in sorted(registry.items()):
        crate_notices(cache, name, version, archive_pin, role, licenses, sections)

    sections.append((ROOT / "scripts/native-notices.txt").read_text(encoding="utf-8").encode())
    report = (rust_sysroot / "share/doc/rust/COPYRIGHT-library.html").read_bytes()
    if digest(report).hexdigest() != RUST_REPORT_DIGEST:
        raise ValueError("Review the installed Rust standard-library copyright report.")

    with (output / "THIRD-PARTY-NOTICES.txt").open("xb") as stream:
        stream.writelines(sections)
    with (output / "RUST-STDLIB-COPYRIGHT.html").open("xb") as stream:
        stream.write(report)
    print(f"Collected notices for {len(registry)} registry components and {len(VENDORED)} vendored sources.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--package", type=Path, required=True)
    parser.add_argument("--manifest-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True, help="Existing directory for the generated notices.")
    parser.add_argument("--cargo-home", type=Path, default=Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo")))
    parser.add_argument("--rust-sysroot", type=Path)
    args = parser.parse_args()
    try:
        if args.rust_sysroot is None:
            args.rust_sysroot = Path(subprocess.check_output(
                ["rustc", "--print", "sysroot"], cwd=ROOT, timeout=30,
                encoding="utf-8", stdin=subprocess.DEVNULL,
            ).strip())
        generate(args.package.resolve(), args.manifest_sha256, args.output, args.cargo_home, args.rust_sysroot)
    except (OSError, ValueError, KeyError, StopIteration, subprocess.SubprocessError, tarfile.TarError) as error:
        parser.exit(1, f"Native notice collection failed. {error}\n")


if __name__ == "__main__":
    main()
