"""Smoke test an installed Guard binary with a disposable local Git repository."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import tempfile

EXAMPLE = Path(__file__).resolve().parents[1] / "examples" / "github"
CASES = [
    (0, "pass", b"ALTER TABLE public.demo ADD COLUMN note text;\n"),
    (1, "denied", b"DROP TABLE public.demo;\n"),
    (3, "incomplete", b"ALTER TABLE public.demo ADD COLUMN;\n"),
]


def run(args, directory, env, expected=0):
    completed = subprocess.run(
        args, cwd=directory, env=env, capture_output=True, stdin=subprocess.DEVNULL, timeout=30,
    )
    if completed.returncode != expected:
        raise ValueError(
            f"Expected exit {expected} for {Path(args[0]).name} {args[1]}, "
            f"received {completed.returncode}."
        )
    return completed.stdout


def smoke(binary, work):
    env = {name: os.environ[name] for name in ("PATH", "SYSTEMROOT", "WINDIR", "COMSPEC")
           if name in os.environ}
    env.update(
        HOME=str(work), USERPROFILE=str(work), TMP=str(work), TEMP=str(work), TMPDIR=str(work),
        GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull, GIT_TERMINAL_PROMPT="0",
        GIT_AUTHOR_NAME="Guard smoke", GIT_AUTHOR_EMAIL="guard@example.invalid",
        GIT_COMMITTER_NAME="Guard smoke", GIT_COMMITTER_EMAIL="guard@example.invalid",
    )
    repo = work / "repo"
    repo.mkdir()

    def git(*args):
        return run(["git", *args], repo, env)

    git("init", "--quiet", "--initial-branch=main", "--object-format=sha1")
    git("config", "--local", "core.autocrlf", "false")
    migrations = repo / "migrations"
    migrations.mkdir()
    (migrations / "001_base.sql").write_bytes(b"CREATE TABLE public.demo (id bigint);\n")
    git("add", "migrations")
    git("commit", "--quiet", "--no-gpg-sign", "-m", "Protected base")
    base = git("rev-parse", "HEAD").decode("ascii").strip()
    for filename in ("dmg.toml", "policy.toml"):
        (work / filename).write_bytes((EXAMPLE / filename).read_bytes())
    version = run([str(binary), "--version"], work, env).decode("ascii").strip()
    print(f"Installed {version}")
    for exit_code, status, sql in CASES:
        (migrations / "002_candidate.sql").write_bytes(sql)
        git("add", "migrations")
        git("commit", "--quiet", "--no-gpg-sign", "-m", status)
        head = git("rev-parse", "HEAD").decode("ascii").strip()
        report = work / f"{status}.json"
        run(
            [str(binary), "check", "--repo", str(repo), "--config", "dmg.toml",
             "--policy", "policy.toml", "--base", base, "--head", head, "--format", "json",
             "--output", report.name],
            work, env, expected=exit_code,
        )
        saved = json.loads(report.read_bytes())
        if saved["decision"]["status"] != status:
            raise ValueError(f"Saved check decision differs from {status}.")
        for renderer in ("html", "sarif", "gitlab"):
            rendered = run(
                [str(binary), "report", "--input", report.name, "--format", renderer], work, env,
            )
            if not rendered.strip():
                raise ValueError(f"The {renderer} report was empty.")
        print(f"Verified {status} with exit {exit_code} and saved report rendering.")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    args = parser.parse_args()
    try:
        binary = args.binary.resolve(strict=True)
        with tempfile.TemporaryDirectory(prefix="dmg-install-smoke-") as work:
            smoke(binary, Path(work))
    except (OSError, ValueError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f"Release smoke failed. {error}\n")


if __name__ == "__main__":
    main()
