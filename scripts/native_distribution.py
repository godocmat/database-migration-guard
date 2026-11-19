"""Create and exercise the archives distributed on GitHub Releases."""

from __future__ import annotations

import argparse
import gzip
import json
import os
from pathlib import Path
import platform
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import zipfile

from package_common import (
    DISTRIBUTION_DOCUMENTS, MAX_BINARY_BYTES, NATIVE_RELEASE_FORMATS, VERSION,
    executable_name, release_archive_name, sha256,
)
from release_sbom import MAX_SBOM_BYTES

ROOT = Path(__file__).resolve().parents[1]
PREVIOUS_SOURCE = "95f825c5d34524e2e68062155fe01cf9acea5603"  # v0.1.0
METADATA_LIMIT = 64 * 1024


def member_limits(target):
    return {
        executable_name(target): MAX_BINARY_BYTES,
        "dmg.cdx.json": MAX_SBOM_BYTES,
        "package.json": METADATA_LIMIT,
        "SHA256SUMS": METADATA_LIMIT,
        **{name: limit for name, limit in DISTRIBUTION_DOCUMENTS.values()},
    }


def create_archive(package, output, version, target):
    if output.name != release_archive_name(version, target):
        raise ValueError("The output filename must identify the release version and target.")
    names = member_limits(target)
    if {item.name for item in package.iterdir()} != names.keys():
        raise ValueError("Unexpected files in the distribution package.")
    if output.exists() or output.is_symlink():
        raise ValueError("Choose a new archive path.")
    with tempfile.TemporaryDirectory(prefix="dmg-archive-", dir=output.parent) as scratch:
        partial = Path(scratch) / output.name
        if output.suffix == ".zip":
            with zipfile.ZipFile(partial, "x") as archive:
                for name in sorted(names):
                    mode = 0o755 if name == executable_name(target) else 0o644
                    member = zipfile.ZipInfo(name)
                    member.create_system = 3
                    member.external_attr = (stat.S_IFREG | mode) << 16
                    member.compress_type = zipfile.ZIP_DEFLATED
                    archive.writestr(member, (package / name).read_bytes())
        else:
            with partial.open("xb") as output_stream:
                with gzip.GzipFile(filename="", mode="wb", fileobj=output_stream, mtime=0) as compressed:
                    with tarfile.open(fileobj=compressed, mode="w", format=tarfile.USTAR_FORMAT) as archive:
                        for name in sorted(names):
                            source = package / name
                            member = tarfile.TarInfo(name)
                            member.size = source.stat().st_size
                            member.mode = 0o755 if name == executable_name(target) else 0o644
                            with source.open("rb") as input_stream:
                                archive.addfile(member, input_stream)
        if output.exists() or output.is_symlink():
            raise ValueError("The archive destination was created while staging.")
        partial.rename(output)


def require_pin(path, expected, limit):
    if not isinstance(expected, str) or not re.fullmatch(r"[0-9a-f]{64}", expected):
        raise ValueError(f"Expected a SHA-256 digest for {path.name}.")
    if path.is_symlink() or not path.is_file() or not 0 < path.stat().st_size <= limit:
        raise ValueError(f"{path.name} must be a regular file within its size limit.")
    if sha256(path) != expected:
        raise ValueError(f"{path.name} differs from its trusted digest.")


def extract_archive(path, digest, target, package):
    limits = member_limits(target)
    require_pin(path, digest, sum(limits.values()) + METADATA_LIMIT)
    package.mkdir()
    seen = set()

    def extract(name, size, regular, open_member):
        if not regular or name in seen or name not in limits or not 0 < size <= limits[name]:
            raise ValueError(f"Unexpected member in the release archive: {name}")
        seen.add(name)
        with open_member() as source, (package / name).open("xb") as destination:
            shutil.copyfileobj(source, destination)

    if path.suffix == ".zip":
        with zipfile.ZipFile(path) as archive:
            for member in archive.infolist():
                regular = stat.S_ISREG(member.external_attr >> 16) and not (member.flag_bits & 1)
                extract(member.filename, member.file_size, regular, lambda: archive.open(member))
    else:
        with tarfile.open(path, "r:gz") as archive:
            for member in archive:
                extract(member.name, member.size, member.isfile(), lambda: archive.extractfile(member))
    if seen != limits.keys():
        raise ValueError("The release archive does not contain all expected members.")
    (package / executable_name(target)).chmod(0o755)


def run(*args, work, env, capture=False):
    return subprocess.run(
        args, cwd=work, env=env, check=True, timeout=300, stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE if capture else None, text=True,
    ).stdout


def install(source, expected, active):
    staged = active.with_name(".dmg-next")
    shutil.copyfile(source, staged)
    staged.chmod(0o755)
    require_pin(staged, expected, MAX_BINARY_BYTES)
    staged.replace(active)


def verify_install(inputs, source, version, target, work):
    expected_platform = "Windows" if executable_name(target).endswith(".exe") else "Linux"
    if platform.system() != expected_platform or platform.machine().lower() not in ("amd64", "x86_64"):
        raise ValueError(f"Use a native {expected_platform} x86-64 verification runner.")

    pins_path = inputs / "pins.json"
    if not pins_path.is_file() or pins_path.is_symlink() or not 0 < pins_path.stat().st_size <= METADATA_LIMIT:
        raise ValueError("Invalid pins.json in the workflow input.")
    pins = json.loads(pins_path.read_bytes())
    if (pins["source_commit"], pins["version"], pins["target"], pins["previous_source"]) != (
        source, version, target, PREVIOUS_SOURCE,
    ):
        raise ValueError("The staged inputs differ from the expected release or rollback source.")
    archive = inputs / release_archive_name(version, target)
    package = work / "package"
    extract_archive(archive, pins["archive_sha256"], target, package)
    previous = inputs / f"rollback-{executable_name(target)}"
    require_pin(previous, pins["previous_sha256"], MAX_BINARY_BYTES)
    require_pin(package / "package.json", pins["manifest_sha256"], METADATA_LIMIT)

    env = {name: os.environ[name] for name in ("PATH", "SYSTEMROOT", "WINDIR", "COMSPEC") if name in os.environ}
    env.update(
        TMP=str(work), TEMP=str(work), TMPDIR=str(work),
        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0",
    )

    def python(script, *args):
        run(sys.executable, "-B", ROOT / script, *args, work=work, env=env)

    python("scripts/stage-release.py", "verify", "--package", package,
           "--manifest-sha256", pins["manifest_sha256"])
    manifest = json.loads((package / "package.json").read_bytes())
    if (manifest["schema_version"], manifest["source_commit"], manifest["version"], manifest["target"]) != (
        2, source, version, target,
    ):
        raise ValueError("The package manifest differs from the expected release.")
    source_license = subprocess.run(
        ["git", "--no-replace-objects", "cat-file", "blob", f"{source}:LICENSE"],
        cwd=ROOT, env=env, stdin=subprocess.DEVNULL, check=True, stdout=subprocess.PIPE, timeout=30,
    ).stdout
    if (package / "LICENSE").read_bytes() != source_license:
        raise ValueError("The built package license differs from the protected source.")
    candidate = package / executable_name(target)
    candidate_digest = manifest["binary"]["sha256"]
    if candidate_digest == pins["previous_sha256"]:
        raise ValueError("The candidate and rollback binaries must differ.")

    print(f"{platform.platform()}: verified {archive.name} at {pins['archive_sha256']}", flush=True)
    run("git", "--version", work=work, env=env)
    active = work / executable_name(target)
    for phase, binary, digest in (
        ("clean installation", candidate, candidate_digest),
        ("previous version installation", previous, pins["previous_sha256"]),
        ("same-path upgrade", candidate, candidate_digest),
        ("rollback", previous, pins["previous_sha256"]),
    ):
        install(binary, digest, active)
        reported = run(active, "--version", work=work, env=env, capture=True).strip()
        if binary == candidate and reported != f"dmg {version}":
            raise ValueError("The installed candidate reported an unexpected version.")
        if phase == "clean installation":
            if expected_platform == "Windows":
                run("pwsh", "-NoProfile", "-NonInteractive", "-Command",
                    'Write-Output ("Authenticode status " + '
                    '(Get-AuthenticodeSignature -LiteralPath $env:DMG_EXE).Status)',
                    work=work, env=dict(env, DMG_EXE=str(active)))
            else:
                run("ldd", active, work=work, env=env)
            python("scripts/smoke-release.py", active)
        else:
            python("integrations/github/tests/replay.py", "--binary", active)
            if phase == "same-path upgrade":
                python("integrations/gitlab/tests/replay.py", "--binary", active)
        require_pin(active, digest, MAX_BINARY_BYTES)
        print(f"Verified {phase} with {reported} at SHA-256 {digest}", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--inputs", required=True, type=Path)
    parser.add_argument("--scratch", required=True, type=Path)
    parser.add_argument("--source", required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", required=True, choices=NATIVE_RELEASE_FORMATS)
    args = parser.parse_args()
    if not re.fullmatch(r"[0-9a-f]{40}", args.source) or not VERSION.fullmatch(args.version):
        parser.error("Specify the exact protected source commit and release version.")
    try:
        with tempfile.TemporaryDirectory(prefix="dmg-verify-release-", dir=args.scratch) as directory:
            verify_install(args.inputs.resolve(strict=True), args.source, args.version, args.target, Path(directory))
    except (OSError, ValueError, TypeError, KeyError, subprocess.SubprocessError,
            tarfile.TarError, zipfile.BadZipFile) as error:
        parser.exit(1, f"Release installation verification failed. {error}\n")


if __name__ == "__main__":
    main()
