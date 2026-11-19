"""Build twice, validate, and smoke a native CI package from one reviewed commit."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from jsonschema import Draft7Validator, ValidationError
from referencing import Registry, Resource

from native_distribution import PREVIOUS_SOURCE, create_archive
from package_common import NATIVE_RELEASE_FORMATS, TARGETS, VERSION, executable_name, release_archive_name, sha256


ROOT = Path(__file__).resolve().parents[1]
PRODUCER = "CycloneDX/cyclonedx-rust-cargo"
GENERATOR_TAG = "cargo-cyclonedx-0.5.9"
GENERATOR_COMMIT = "e58bd5590212f82c5b7e16dd3e2e819b0dbea5b1"
SCHEMA_COMMIT = "c320fc0f0b46873864927d9d5684eea7ba439728"
GENERATOR_DIGESTS = {
    "x86_64-unknown-linux-gnu": "fb8dbee9f182173e062a64a387b21a0badc6fab8b2abf9294973f012972bf6d8",
    "x86_64-pc-windows-msvc": "8750e00775661dcb75bc482c1a298839fd94e8a0c033b49905ba0f246ffed202",
    "aarch64-apple-darwin": "4c53dfa21e70b65bf7f8d2592aadde3bcb02c1a40b6ec63b877e5ca65a29e180",
}
SCHEMA_DIGESTS = {
    "bom-1.5.schema.json": "067f7824b08653839ea050ae9e09ca48375eadc2652b0e2a299476e7db90335b",
    "jsf-0.82.schema.json": "8bae002c25e723db7ee1f26afde680ae1a2b1a8f6b4b4b0fd65dc3becb090aae",
    "spdx.schema.json": "4f6e2b05c05d26a4f2dc5879fbc2fca94b0a28db46289d0c51345621b71cfbfc",
}


def fetch(url, limit=4 * 1024 * 1024):
    return subprocess.run(
        ["curl", "--disable", "--fail", "--silent", "--show-error", "--location",
         "--proto", "=https", "--max-time", "30", "--retry", "2", "--retry-max-time", "45",
         "--max-filesize", str(limit), "--", url],
        cwd=ROOT, check=True, timeout=80, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
    ).stdout


def fetch_pinned(url, path, expected):
    path.write_bytes(fetch(url))
    if sha256(path) != expected:
        raise ValueError(f"Upstream SHA-256 differs for {path.name}")


def run(*args, cwd=ROOT, env=None, timeout=120):
    subprocess.run(args, cwd=cwd, env=env, check=True, timeout=timeout, stdin=subprocess.DEVNULL)


def python(script, *args, timeout=120):
    run(sys.executable, "-B", ROOT / script, *args, timeout=timeout)


def extract_generator(archive, target, output):
    name = f"cargo-cyclonedx{TARGETS[target]}"
    if archive.suffix == ".zip":
        with zipfile.ZipFile(archive) as source:
            output.write_bytes(source.read(name))
    else:
        with tarfile.open(archive, "r:xz") as source:
            member = source.getmember(f"cargo-cyclonedx-{target}/{name}")
            if not member.isfile():
                raise ValueError("SBOM generator archive member must be a regular file")
            with source.extractfile(member) as stream:
                output.write_bytes(stream.read())
    output.chmod(0o755)


def install_generator(target, work):
    suffix = "zip" if TARGETS[target] else "tar.xz"
    archive = work / f"cargo-cyclonedx-{target}.{suffix}"
    digest = GENERATOR_DIGESTS[target]
    fetch_pinned(f"https://github.com/{PRODUCER}/releases/download/{GENERATOR_TAG}/{archive.name}", archive, digest)
    # Use a reviewed public bundle when the hosted runner shares an anonymous API quota.
    bundle = ROOT / "scripts" / "attestations" / f"{GENERATOR_TAG}-{target}.jsonl"
    if not bundle.is_file():
        response = json.loads(fetch(f"https://api.github.com/repos/{PRODUCER}/attestations/sha256:{digest}"))
        bundle = work / "generator-attestation.jsonl"
        bundle.write_text("".join(json.dumps(item["bundle"]) + "\n" for item in response["attestations"]), encoding="utf-8")
    run("gh", "attestation", "verify", archive, "--bundle", bundle, "--repo", PRODUCER,
        "--signer-workflow", f"{PRODUCER}/.github/workflows/release.yml", "--signer-digest", GENERATOR_COMMIT,
        "--source-ref", f"refs/tags/{GENERATOR_TAG}", "--source-digest", GENERATOR_COMMIT, "--deny-self-hosted-runners")
    executable = work / f"cargo-cyclonedx{TARGETS[target]}"
    extract_generator(archive, target, executable)
    return executable


def sbom_validator(work):
    schemas = {}
    for name, digest in SCHEMA_DIGESTS.items():
        schema_file = work / name
        fetch_pinned(f"https://raw.githubusercontent.com/CycloneDX/specification/{SCHEMA_COMMIT}/schema/{name}", schema_file, digest)
        schemas[name] = json.loads(schema_file.read_bytes())
    resources = [(schema["$id"], Resource.from_contents(schema)) for schema in schemas.values()]
    return Draft7Validator(schemas["bom-1.5.schema.json"], registry=Registry().with_resources(resources))


def validate_sbom(path, work):
    report = json.loads(path.read_bytes())
    if report.get("specVersion") != "1.5":
        raise ValueError("SBOM must declare CycloneDX 1.5")
    sbom_validator(work).validate(report)


def stage(target, commit, generator, path):
    python("scripts/stage-release.py", "stage", "--source", ROOT, "--expected-commit", commit,
           "--target", target, "--cyclonedx", generator, "--output", path, timeout=960)
    pin = sha256(path / "package.json")
    python("scripts/stage-release.py", "verify", "--package", path, "--manifest-sha256", pin)
    return pin


def check_reproducible(first, second):
    if {p.name: sha256(p) for p in first.iterdir()} != {p.name: sha256(p) for p in second.iterdir()}:
        raise ValueError("Independent source extractions produced different package files")


def build_rollback(target, work, destination):
    checkout = work / "rollback-source"
    target_dir = work / "rollback-target"
    package = work / "rollback-package"
    run("git", "worktree", "add", "--quiet", "--detach", checkout, PREVIOUS_SOURCE)
    try:
        env = dict(os.environ, CARGO_TARGET_DIR=str(target_dir), DOCS_RS="1")
        run("cargo", "fetch", "--locked", "--target", target, cwd=checkout, env=env, timeout=180)
        run("cargo", "build", "--locked", "--offline", "--release", "--target", target, "-p", "dmg-cli",
            cwd=checkout, env=env, timeout=900)
        python("scripts/stage-pilot.py", "--source", checkout, "--target", target,
               "--binary", target_dir / target / "release" / executable_name(target), "--output", package)
    finally:
        run("git", "worktree", "remove", checkout)
    previous = json.loads((package / "package.json").read_bytes())
    if (previous["source_commit"], previous["target"]) != (PREVIOUS_SOURCE, target):
        raise ValueError("The rollback binary was not built from the pinned pilot source.")
    shutil.copyfile(package / previous["binary"]["name"], destination)
    if sha256(destination) != previous["binary"]["sha256"]:
        raise ValueError("The staged rollback executable differs from its pilot manifest.")
    return previous["binary"]["sha256"]


def build_distribution(target, commit, version, candidate, pin, output, work):
    notices, layout = work / "notices", work / "distribution-package"
    notices.mkdir()
    python("scripts/package-notices.py", "--package", candidate, "--manifest-sha256", pin,
           "--output", notices, timeout=300)
    license_path = work / "LICENSE"
    license_path.write_bytes(subprocess.run(
        ["git", "--no-replace-objects", "cat-file", "blob", f"{commit}:LICENSE"],
        cwd=ROOT, check=True, timeout=30, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
    ).stdout)
    python("scripts/stage-release.py", "distribution", "--package", candidate,
           "--manifest-sha256", pin, "--license", license_path, "--notices", notices, "--output", layout)
    create_archive(layout, output, version, target)
    rollback = output.parent / f"rollback-{executable_name(target)}"
    previous = build_rollback(target, work, rollback)
    pins = {
        "source_commit": commit,
        "version": version,
        "target": target,
        "archive_sha256": sha256(output),
        "manifest_sha256": sha256(layout / "package.json"),
        "previous_source": PREVIOUS_SOURCE,
        "previous_sha256": previous,
    }
    (output.parent / "pins.json").write_text(
        json.dumps(pins, indent=2, sort_keys=True) + "\n", encoding="utf-8", newline="\n",
    )


def verify(target, commit, output, work, distribution=None):
    if output.exists() or output.is_symlink():
        raise ValueError("Choose a new artifact path")
    if distribution and (
        not VERSION.fullmatch(distribution) or target not in NATIVE_RELEASE_FORMATS
        or output.name != release_archive_name(distribution, target)
        or any(
            path.exists() or path.is_symlink()
            for path in (output.parent / "pins.json", output.parent / f"rollback-{executable_name(target)}")
        )
    ):
        raise ValueError("Choose a new Linux or Windows release path with the expected version and filename.")
    run("git", "--version")
    run("rustc", "-Vv")
    run(sys.executable, "--version")
    run("cargo", "fetch", "--locked", "--target", target, timeout=180)
    generator = install_generator(target, work)
    first, second = work / "first", work / "second"
    pin = stage(target, commit, generator, first)
    stage(target, commit, generator, second)
    check_reproducible(first, second)
    validate_sbom(first / "dmg.cdx.json", work)

    manifest = json.loads((first / "package.json").read_bytes())
    if distribution and manifest["version"] != distribution:
        raise ValueError("The candidate version differs from the requested release version.")
    installed = work / "installed"
    installed.mkdir()
    binary = installed / f"dmg{TARGETS[target]}"
    shutil.copy2(first / manifest["binary"]["name"], binary)
    if sha256(binary) != manifest["binary"]["sha256"]:
        raise ValueError("Installed executable digest differs from the verified package")
    python("scripts/smoke-release.py", binary)
    for provider in ("github", "gitlab"):
        python(f"integrations/{provider}/tests/replay.py", "--binary", binary, timeout=300)

    if distribution:
        build_distribution(target, commit, distribution, first, pin, output, work)
    else:
        with tarfile.open(output, "x:gz") as archive:
            for member in sorted(first.iterdir()):
                archive.add(member, arcname=member.name)
    print(f"Repeated package manifest SHA-256 {pin}")
    print(f"Artifact archive SHA-256 {sha256(output)}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("target", choices=TARGETS)
    parser.add_argument("commit")
    parser.add_argument("output", type=Path)
    parser.add_argument("--distribution", metavar="VERSION", help="include official distribution and rollback inputs")
    args = parser.parse_args()
    try:
        with tempfile.TemporaryDirectory(prefix="dmg-native-ci-", dir=args.output.parent) as directory:
            verify(args.target, args.commit, args.output, Path(directory).resolve(), args.distribution)
    except (OSError, ValueError, KeyError, subprocess.SubprocessError, tarfile.TarError, zipfile.BadZipFile, ValidationError) as error:
        parser.exit(1, f"Native package verification failed. {error}\n")


if __name__ == "__main__":
    main()
