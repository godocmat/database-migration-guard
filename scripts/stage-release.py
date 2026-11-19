"""Build and verify a local Guard release candidate without publishing it."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import tarfile
import tempfile
import tomllib

from package_common import (
    DISTRIBUTION_DOCUMENTS, MAX_BINARY_BYTES, NATIVE_RELEASE_FORMATS, TARGETS, VERSION,
    binary_name, executable_name, sha256,
)
from release_sbom import GENERATOR_VERSION, MAX_SBOM_BYTES, cargo_artifacts, normalize

COMMIT = re.compile(r"(?:[0-9a-f]{40}|[0-9a-f]{64})")
DIGEST = re.compile(r"[0-9a-f]{64}")
MAX_MANIFEST_BYTES = 64 * 1024
SBOM = "dmg.cdx.json"


def command(*args, cwd, env=None, timeout=30):
    result = subprocess.run(
        args, cwd=cwd, env=env, check=False, timeout=timeout, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, encoding="utf-8", errors="replace",
    )
    if result.returncode:
        detail = result.stderr.strip()[-4000:]
        raise ValueError(f"{Path(args[0]).name} failed with status {result.returncode}\n{detail}")
    return result.stdout.strip()


def source_details(source, expected_commit):
    if Path(command("git", "rev-parse", "--show-toplevel", cwd=source)).resolve() != source:
        raise ValueError("The build source must be the checkout root.")
    commit = command("git", "--no-replace-objects", "rev-parse", "--verify", "HEAD^{commit}", cwd=source)
    if commit != expected_commit:
        raise ValueError("The checkout does not match the reviewed source commit.")
    if command("git", "status", "--porcelain", "--untracked-files=normal", cwd=source):
        raise ValueError("Build from a clean checkout of the reviewed source commit.")
    timestamp = int(command("git", "--no-replace-objects", "log", "-1", "--format=%ct", commit, cwd=source))
    return commit, timestamp


def extract_source(source, commit, work):
    archive = work / "source.tar"
    command(
        "git", "--no-replace-objects", "-c", "core.autocrlf=false", "-c", "core.eol=lf",
        "archive", "--format=tar", f"--output={archive}", commit, cwd=source,
    )
    checkout = work / "source"
    checkout.mkdir()
    with tarfile.open(archive) as stream:
        stream.extractall(checkout, filter="data")
    for name in ("Cargo.lock", "rust-toolchain.toml"):
        path = checkout / name
        if path.is_symlink() or not path.is_file():
            raise ValueError(f"{name} must be a regular file in the source archive.")
        reviewed = command(
            "git", "--no-replace-objects", "rev-parse", "--verify", f"{commit}:{name}", cwd=source,
        )
        archived = command("git", "hash-object", "--no-filters", "--", str(path), cwd=source)
        if archived != reviewed:
            raise ValueError(f"Archived {name} differs from the reviewed Git blob.")
    return checkout


def component(path):
    return {"name": path.name, "sha256": sha256(path), "bytes": path.stat().st_size}


def checksums(package, manifest):
    names = ["package.json", manifest["binary"]["name"], SBOM]
    if manifest["schema_version"] == 2:
        names.extend(name for name, _ in DISTRIBUTION_DOCUMENTS.values())
    return "".join(f"{sha256(package / name)}  {name}\n" for name in sorted(names))


def write_package_metadata(package, manifest):
    manifest_path = package / "package.json"
    manifest_path.write_text(json.dumps(manifest, indent=2, sort_keys=True) + "\n",
                             encoding="utf-8", newline="\n")
    (package / "SHA256SUMS").write_text(checksums(package, manifest),
                                        encoding="ascii", newline="\n")
    return sha256(manifest_path)


def build_environment(checkout, target_dir, target, timestamp):
    env = dict(
        os.environ, CARGO_TARGET_DIR=str(target_dir), CARGO_INCREMENTAL="0",
        CARGO_NET_OFFLINE="true", DOCS_RS="1", SOURCE_DATE_EPOCH=str(timestamp),
    )
    cargo_home = Path(env.get("CARGO_HOME", Path.home() / ".cargo")).resolve()
    env["CARGO_HOME"] = str(cargo_home)
    remaps = [(target_dir, "/dmg/target"), (checkout, "/dmg/source"),
              (cargo_home, "/dmg/cargo")]
    mappings = [(spelling, to) for path, to in remaps
                for spelling in sorted({str(path), path.as_posix()})]
    rust_flags = env.get("CARGO_ENCODED_RUSTFLAGS", "").split("\x1f")
    if "CARGO_ENCODED_RUSTFLAGS" not in env:
        rust_flags = env.get("RUSTFLAGS", "").split()
    rust_flags.extend(f"--remap-path-prefix={source}={dest}" for source, dest in mappings)
    flag_var = "CFLAGS_" + target.replace("-", "_")
    c_flags = [env.get(flag_var, "")]
    if target == "x86_64-pc-windows-msvc":
        rust_flags.append("-Clink-arg=/Brepro")
        c_flags.append("/experimental:deterministic")
        prefix = "/pathmap:"
    else:
        prefix = "-ffile-prefix-map="
    for source, dest in mappings:
        escaped = source.replace("\\", "\\\\").replace('"', '\\"')
        c_flags.append(f'"{prefix}{escaped}={dest}"')
    env[flag_var] = " ".join(flag for flag in c_flags if flag)
    env["CC_SHELL_ESCAPED_FLAGS"] = "1"
    env["CARGO_ENCODED_RUSTFLAGS"] = "\x1f".join(flag for flag in rust_flags if flag)
    return env


def stage(source, output, target, cyclonedx, expected_commit):
    source = source.resolve(strict=True)
    cyclonedx = cyclonedx.resolve(strict=True)
    output = output.absolute()
    if output.exists() or output.is_symlink():
        raise ValueError("Choose a new output directory for the release candidate.")
    commit, timestamp = source_details(source, expected_commit)
    with tempfile.TemporaryDirectory(prefix="dmg-stage-", dir=output.parent) as scratch:
        work = Path(scratch).resolve()
        checkout = extract_source(source, commit, work)
        target_dir = work / "target"
        package = work / "package"
        package.mkdir()
        env = build_environment(checkout, target_dir, target, timestamp)
        rustc = command(env.get("RUSTC", "rustc"), "-Vv", cwd=checkout, env=env)
        toolchain = checkout / "rust-toolchain.toml"
        pinned = tomllib.loads(toolchain.read_text(encoding="utf-8"))["toolchain"]["channel"]
        if f"host: {target}" not in rustc.splitlines() or f"release: {pinned}" not in rustc.splitlines():
            raise ValueError("Build on the requested native target using the pinned Rust toolchain.")
        generator = command(str(cyclonedx), "cyclonedx", "--version", cwd=checkout, env=env)
        if generator.split()[-1:] != [GENERATOR_VERSION]:
            raise ValueError(f"Use the reviewed cargo-cyclonedx {GENERATOR_VERSION} tool.")

        artifacts = command(
            "cargo", "build", "--locked", "--offline", "--release", "--target", target,
            "-p", "dmg-cli", "--message-format=json", cwd=checkout, env=env, timeout=900,
        )
        built = target_dir / target / "release" / f"dmg{TARGETS[target]}"
        if not built.is_file() or not 0 < built.stat().st_size <= MAX_BINARY_BYTES:
            raise ValueError("The build did not produce the expected executable.")
        executable = package / built.name
        shutil.copyfile(built, executable)
        executable.chmod(0o755)
        identity = command(str(executable), "--version", cwd=work).split()
        if len(identity) != 2 or identity[0] != "dmg" or not VERSION.fullmatch(identity[1]):
            raise ValueError("The built executable did not report the expected dmg version.")
        version = identity[1]
        executable = executable.rename(package / binary_name(version, target))
        binary = component(executable)

        cli = checkout / "crates" / "dmg-cli"
        raw_sbom = cli / f"dmg_bin_{target}.cdx.json"
        command(
            str(cyclonedx), "cyclonedx", "--manifest-path", str(cli / "Cargo.toml"),
            "--describe", "binaries", "--format", "json", "--spec-version", "1.5",
            "--target", target, "--target-in-filename", cwd=checkout, env=env, timeout=120,
        )
        if not 0 < raw_sbom.stat().st_size <= MAX_SBOM_BYTES:
            raise ValueError("The generated SBOM exceeds its size limit.")
        sbom = package / SBOM
        sbom.write_text(
            normalize(raw_sbom.read_bytes(), cargo_artifacts(artifacts, target_dir, target),
                      target, commit, binary["sha256"]),
            encoding="utf-8", newline="\n",
        )
        manifest = {
            "schema_version": 1,
            "name": "dmg",
            "version": version,
            "target": target,
            "source_commit": commit,
            "source_date_epoch": timestamp,
            "cargo_lock_sha256": sha256(checkout / "Cargo.lock"),
            "rust_toolchain_sha256": sha256(toolchain),
            "rustc": rustc.splitlines()[0],
            "profile": "release",
            "sbom_generator_sha256": sha256(cyclonedx),
            "binary": binary,
            "sbom": component(sbom),
        }
        manifest_digest = write_package_metadata(package, manifest)
        verify(package, manifest_digest)
        if output.exists() or output.is_symlink():
            raise ValueError("The destination was created while staging.")
        package.rename(output)
    return manifest, manifest_digest


def prepare_distribution(package, pin, license_path, notices, output):
    manifest = verify(package, pin)
    target = manifest["target"]
    if manifest["schema_version"] != 1 or target not in NATIVE_RELEASE_FORMATS:
        raise ValueError("Use a verified Linux or Windows x86-64 native candidate.")
    if output.exists() or output.is_symlink():
        raise ValueError("Choose a new distribution directory.")
    with tempfile.TemporaryDirectory(prefix="dmg-distribution-", dir=output.parent) as scratch:
        layout = Path(scratch) / "package"
        layout.mkdir()
        executable = layout / executable_name(target)
        shutil.copyfile(package / manifest["binary"]["name"], executable)
        executable.chmod(0o755)
        if sha256(executable) != manifest["binary"]["sha256"]:
            raise ValueError("The copied executable differs from the verified candidate.")
        shutil.copyfile(package / SBOM, layout / SBOM)
        if component(layout / SBOM) != manifest["sbom"]:
            raise ValueError("The copied SBOM differs from the verified candidate.")
        manifest.update(schema_version=2, binary=component(executable))
        for key, (name, limit) in DISTRIBUTION_DOCUMENTS.items():
            source = license_path if key == "license" else notices / name
            destination = layout / name
            destination.write_bytes(read_metadata(source, limit))
            manifest[key] = component(destination)
        digest = write_package_metadata(layout, manifest)
        verify(layout, digest)
        if output.exists() or output.is_symlink():
            raise ValueError("The distribution destination was created while preparing it.")
        layout.rename(output)
    return manifest, digest


def read_metadata(path, limit):
    if path.is_symlink() or not path.is_file():
        raise ValueError(f"{path.name} must be a regular file.")
    with path.open("rb") as stream:
        data = stream.read(limit + 1)
    if len(data) > limit:
        raise ValueError(f"{path.name} exceeds its size limit.")
    return data


def verify_component(package, description, expected_name, limit):
    if description["name"] != expected_name:
        raise ValueError("The manifest artifact name differs from the expected package layout.")
    path = package / expected_name
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= limit:
        raise ValueError(f"{expected_name} is not a regular file within its size limit.")
    if component(path) != description:
        raise ValueError(f"{expected_name} differs from its pinned size or SHA-256.")


def verify(package, expected_digest):
    raw = read_metadata(package / "package.json", MAX_MANIFEST_BYTES)
    if hashlib.sha256(raw).hexdigest() != expected_digest:
        raise ValueError("package.json SHA-256 differs from its trusted pin.")
    manifest = json.loads(raw)
    version = manifest["version"]
    target = manifest["target"]
    if (
        manifest["schema_version"] not in (1, 2) or manifest["name"] != "dmg"
        or target not in TARGETS or not VERSION.fullmatch(version)
        or not COMMIT.fullmatch(manifest["source_commit"])
        or not DIGEST.fullmatch(manifest["cargo_lock_sha256"])
        or not DIGEST.fullmatch(manifest["rust_toolchain_sha256"])
    ):
        raise ValueError("Invalid Guard package manifest.")
    if manifest["schema_version"] == 2:
        if target not in NATIVE_RELEASE_FORMATS:
            raise ValueError("The distribution package target is unsupported.")
        for key, (name, limit) in DISTRIBUTION_DOCUMENTS.items():
            verify_component(package, manifest[key], name, limit)
        executable = executable_name(target)
    else:
        executable = binary_name(version, target)
    verify_component(package, manifest["binary"], executable, MAX_BINARY_BYTES)
    verify_component(package, manifest["sbom"], SBOM, MAX_SBOM_BYTES)
    expected = checksums(package, manifest).encode("ascii")
    if read_metadata(package / "SHA256SUMS", MAX_MANIFEST_BYTES) != expected:
        raise ValueError("SHA256SUMS differs from the verified package manifest.")
    return manifest


def pinned(pattern, label):
    def parse(value):
        if not pattern.fullmatch(value):
            raise argparse.ArgumentTypeError(f"Expected {label} in lowercase hexadecimal.")
        return value
    return parse


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    modes = parser.add_subparsers(dest="mode", required=True)
    build = modes.add_parser("stage", help="build from a clean native checkout")
    build.add_argument("--source", type=Path, default=Path.cwd())
    build.add_argument("--expected-commit", required=True, type=pinned(COMMIT, "a full Git commit"))
    build.add_argument("--target", choices=TARGETS, required=True)
    build.add_argument("--cyclonedx", type=Path, required=True, help="reviewed cargo-cyclonedx executable")
    build.add_argument("--output", type=Path, required=True, help="new package directory")
    distribution = modes.add_parser("distribution", help="prepare an archive layout from a verified native candidate")
    distribution.add_argument("--package", type=Path, required=True)
    distribution.add_argument("--manifest-sha256", required=True, type=pinned(DIGEST, "a SHA-256 digest"))
    distribution.add_argument("--license", type=Path, required=True)
    distribution.add_argument("--notices", type=Path, required=True)
    distribution.add_argument("--output", type=Path, required=True, help="new distribution directory")
    check = modes.add_parser("verify", help="compare a package with its trusted manifest pin")
    check.add_argument("--package", type=Path, required=True)
    check.add_argument("--manifest-sha256", required=True, type=pinned(DIGEST, "a SHA-256 digest"))
    args = parser.parse_args()
    try:
        if args.mode == "stage":
            manifest, pin = stage(args.source, args.output, args.target, args.cyclonedx, args.expected_commit)
            print(f"Staged {manifest['binary']['name']}")
            print(f"package.json SHA-256 {pin}")
        elif args.mode == "distribution":
            manifest, pin = prepare_distribution(
                args.package, args.manifest_sha256, args.license, args.notices, args.output,
            )
            print(f"Prepared {manifest['binary']['name']} for distribution")
            print(f"package.json SHA-256 {pin}")
        else:
            manifest = verify(args.package, args.manifest_sha256)
            print(f"Verified {manifest['binary']['name']} from {manifest['source_commit']}")
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError, tarfile.TarError) as error:
        parser.exit(1, f"Release package check failed. {error}\n")


if __name__ == "__main__":
    main()
