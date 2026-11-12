"""Validate and materialize the DMG-02 development fixtures."""

from __future__ import annotations

import argparse
from collections import Counter, defaultdict
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parent
FILENAME = re.compile(r"([0-9]+)_([a-z][a-z0-9_]*)(?:\.(up|down))?\.sql")
RULES = {f"PG{number:03d}" for number in range(1, 8)}
SEVERITIES = ("INFO", "LOW", "MEDIUM", "HIGH", "CRITICAL")
STATUSES = ("unchanged", "new", "modified", "missing")
DIRECTIVE = b"-- no-transaction"
ATTRIBUTES = b"*.sql -text\n"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def unique_pairs(pairs):
    mapping = dict(pairs)
    require(len(mapping) == len(pairs), f"duplicate keys in {pairs!r}")
    return mapping


def read_json(name):
    return json.loads((ROOT / name).read_bytes(), object_pairs_hook=unique_pairs)


def read_corpus():
    manifest = read_json("manifest.json")
    require(manifest["schema_version"] == 1 and manifest["split"] == "development",
            "expected the version 1 development manifest")
    require(set(manifest["postgres"]) == {"17", "18"}, "target matrix must cover 17 and 18")
    digests = read_json("sha256.json")
    inputs = {p.relative_to(ROOT).as_posix(): p.read_bytes()
              for p in sorted((ROOT / "inputs").rglob("*.sql"))}
    require(set(digests) == set(inputs), "digest inventory and SQL inputs differ")
    for path, data in inputs.items():
        data.decode("utf-8")
        require(data.endswith(b"\n") and not data.startswith(b"\xef\xbb\xbf"),
                f"{path} needs a final newline and UTF-8 without BOM")
        require(hashlib.sha256(data).hexdigest() == digests[path], f"digest mismatch for {path}")
    for stem, source in (("base", "base"), ("spans", "spans-lf")):
        lf = inputs[f"inputs/{source}.sql"]
        require(b"\r" not in lf and inputs[f"inputs/{stem}-crlf.sql"] == lf.replace(b"\n", b"\r\n"),
                f"{stem} must have identical LF and CRLF content after normalization")
    base_comment, base_sql = inputs["inputs/base.sql"].split(b"\n", 1)
    edited_comment, edited_sql = inputs["inputs/base-comment.sql"].split(b"\n", 1)
    require(base_comment.startswith(b"-- ") and edited_comment.startswith(b"-- ")
            and base_comment != edited_comment and base_sql == edited_sql,
            "G02 comment edit must change only the leading comment")
    return manifest, inputs


def git(repo, *args, check=True):
    env = {key: value for key, value in os.environ.items() if not key.upper().startswith("GIT_")}
    env.update(GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
               GIT_AUTHOR_NAME="DMG Fixture", GIT_AUTHOR_EMAIL="fixture@example.invalid",
               GIT_COMMITTER_NAME="DMG Fixture", GIT_COMMITTER_EMAIL="fixture@example.invalid",
               GIT_AUTHOR_DATE="2000-01-01T00:00:00+00:00", GIT_COMMITTER_DATE="2000-01-01T00:00:00+00:00")
    return subprocess.run(["git", "-C", str(repo), *args], env=env, capture_output=True, check=check)


def file_map(entries, migration_root):
    files = unique_pairs(entries)
    for path in files:
        parts = path.split("/")
        require(parts[0] == migration_root and len(parts) >= 2
                and all(re.fullmatch(r"[a-zA-Z0-9_][a-zA-Z0-9_.-]*", part) for part in parts),
                f"invalid migration path {path!r}")
    return files


def write_inputs(repo, entries, inputs):
    for path, source in entries.items():
        target = repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(inputs[source])


def commit(repo, message):
    git(repo, "add", "--all")
    git(repo, "commit", "--quiet", "--no-gpg-sign", "-m", message)
    return git(repo, "rev-parse", "HEAD").stdout.decode().strip()


def restore_checkout(repo, paths):
    for path in paths:
        (repo / path).unlink()
    git(repo, "checkout-index", "--all", "--force")


def verify_checkout(repo, revision, expected, inputs, migration_root):
    require(git(repo, "rev-parse", "HEAD").stdout.decode().strip() == revision,
            f"{repo} has an unexpected HEAD")
    paths = git(repo, "ls-tree", "-r", "--name-only", "-z", revision, "--", migration_root)
    actual_paths = paths.stdout.decode("utf-8").split("\0")[:-1]
    require(set(actual_paths) == set(expected), f"unexpected paths at {revision}")
    actual = {}
    for path in actual_paths:
        blob = git(repo, "cat-file", "blob", f"{revision}:{path}").stdout
        require(blob == inputs[expected[path]] == (repo / path).read_bytes(),
                f"blob or checkout bytes differ at {path}")
        actual[path] = blob
    require(not git(repo, "status", "--porcelain").stdout, f"dirty fixture at {revision}")
    return actual


def inventory(files, profile, migration_root):
    migrations = []
    ignored = []
    for path in sorted(files):
        relative = PurePosixPath(path)
        match = FILENAME.fullmatch(relative.name)
        if relative.parent.as_posix() != migration_root or not match:
            ignored.append(path)
            continue
        prefix, stem, suffix = match.groups()
        version = int(prefix)
        require(0 < version < 2**63, f"version outside fixture range in {path}")
        require((suffix is not None) == (profile["format"] == "reversible"),
                f"{path} uses a different layout from its fixture profile")
        if profile["adapter"] == "plain":
            require(re.fullmatch(profile["filename_pattern"], relative.name)
                    and profile["version_order"] == "positive-integer",
                    f"{path} differs from the declared plain SQL discovery config")
        migrations.append({"path": path, "version": version, "direction": suffix or "simple",
                           "stem": stem})
    return sorted(migrations, key=lambda m: (m["version"], m["path"])), ignored


def discovery(migrations, ignored):
    forward = [m for m in migrations if m["direction"] != "down"]
    counts = Counter(m["version"] for m in forward)
    pairs = defaultdict(set)
    for m in migrations:
        if m["direction"] != "simple":
            pairs[m["version"], m["stem"]].add(m["direction"])
    return {
        "forward": [m["version"] for m in forward],
        "down": [m["version"] for m in migrations if m["direction"] == "down"],
        "ignored": ignored,
        "duplicate_forward": sorted(m["path"] for m in forward if counts[m["version"]] > 1),
        "unpaired": sorted(m["path"] for m in migrations
                           if m["direction"] != "simple"
                           and pairs[m["version"], m["stem"]] != {"up", "down"}),
    }


def git_identity_status(base, candidate, baseline, inventory):
    by_revision = []
    for files, migrations in ((base, baseline), (candidate, inventory)):
        identities = defaultdict(dict)
        for m in migrations:
            identities[m["version"]][m["direction"]] = files[m["path"]]
        by_revision.append(identities)
    before, after = by_revision
    return {
        "unchanged": sorted(v for v in before.keys() & after.keys() if before[v] == after[v]),
        "new": sorted(after.keys() - before.keys()),
        "modified": sorted(v for v in before.keys() & after.keys() if before[v] != after[v]),
        "missing": sorted(before.keys() - after.keys()),
    }


def verify_diff(repo, base_id, head_id, base, candidate, root):
    changes = {}
    for path in base.keys() | candidate.keys():
        if path not in base:
            changes[path] = "A"
        elif path not in candidate:
            changes[path] = "D"
        elif base[path] != candidate[path]:
            changes[path] = "M"
    output = git(repo, "diff", "--name-status", "--no-renames", "-z", base_id, head_id, "--", root)
    fields = output.stdout.decode().split("\0")[:-1]
    require(dict(zip(fields[1::2], fields[::2])) == changes, "raw Git changes differ from snapshots")


def verify_source_spans(variant, candidate, selection):
    for span in variant.get("source_spans", []):
        path = span["path"]
        require(path in selection, f"span on unselected path {path}")
        data = candidate[path]
        start, end = span["bytes"]
        require(0 <= start < end <= len(data) and data[start:end] == span["text"].encode("utf-8"),
                f"wrong byte range in {path} for {span['kind']}")
        prefix = data[:start]
        line = prefix.count(b"\n") + 1
        column = len(prefix.rsplit(b"\n", 1)[-1].decode("utf-8")) + 1
        require((line, column) == (span["line"], span["column"]), f"wrong display position in {path}")


def verify_outcome(expected, policy, candidate, selection, statuses):
    findings = expected.get("findings", [])
    absent = set(expected.get("absent", []))
    diagnostics = expected.get("diagnostics", [])
    coverage = expected["coverage"]
    require(coverage in {"complete", "incomplete"}, f"invalid coverage {coverage}")
    require(absent <= RULES and absent.isdisjoint(f["rule"] for f in findings),
            "expected finding and control assertions conflict")
    for f in findings:
        require(f["rule"] in RULES and f["severity"] in SEVERITIES and f["object"] and f["basis"],
                f"incomplete finding label {f}")
        require(selection is not None and f["path"] in selection
                and f["anchor"] and f["anchor"].encode() in candidate[f["path"]],
                f"invalid finding anchor {f}")
    for diagnostic in diagnostics:
        paths = diagnostic["paths"]
        require(set(paths) <= candidate.keys() and diagnostic["kind"] and diagnostic["detail"],
                f"invalid diagnostic {diagnostic}")
        if "anchor" in diagnostic:
            require(len(paths) == 1 and diagnostic["anchor"].encode() in candidate[paths[0]],
                    f"invalid diagnostic anchor {diagnostic}")

    threshold = SEVERITIES.index(policy["deny_at_or_above"])
    deny = any(SEVERITIES.index(f["severity"]) >= threshold for f in findings)
    deny = deny or any(statuses and statuses[key] for key in policy["deny_git_status"])
    if coverage == "incomplete":
        require(diagnostics and policy["coverage_required"], "incomplete outcome needs a coverage reason")
        exit_code = 3
    else:
        require(selection is not None and not diagnostics, "complete outcome needs resolved selection")
        exit_code = 1 if deny else 0
    require(expected["check_exit"] == exit_code, f"policy expects exit {exit_code}, got {expected}")
    if "fallback" in expected:
        require(coverage == "complete" and expected["fallback"]["coverage"] == "incomplete",
                "fallback must describe a coverage gap")
        verify_outcome(expected["fallback"], policy, candidate, selection, statuses)


def verify_modes(manifest, profile_id, variant, migrations, candidate):
    profile = manifest["profiles"][profile_id]
    overrides = variant.get("modes", {})
    require(overrides.keys() <= set(variant.get("profiles", [])), "mode override names an inapplicable profile")
    written = dict(variant.get("write", []))
    for migration in migrations:
        path = migration["path"]
        mode = profile["mode"]
        if path in written and migration["direction"] != "down":
            mode = overrides.get(profile_id, mode)
        require(manifest["modes"][mode]["runner"] == profile["runner"], f"runner mismatch in {mode}")
        data = candidate[path]
        if profile["adapter"] == "sqlx":
            opts_out = data.startswith(DIRECTIVE)
            require(opts_out == mode.startswith("sqlx-opt-out-"), f"directive mismatch in {path}")
            if opts_out:
                count = variant.get("forward_statement_count", 0)
                require((mode == "sqlx-opt-out-single" and count == 1)
                        or (mode == "sqlx-opt-out-batch" and count > 1),
                        f"missing or incompatible declared statement count in {path}")
        if mode == "psql-explicit":
            require(data.startswith(b"BEGIN;\n") and data.endswith(b"COMMIT;\n"),
                    f"missing explicit transaction boundaries in {path}")
        migration["mode"] = mode


def make_fixture(manifest, inputs, variant, profile_id, destination):
    profile = manifest["profiles"][profile_id]
    root = manifest["migration_root"]
    base_entries = manifest["baselines"][profile["baseline"]] + variant.get("baseline_extra", [])
    base_sources = file_map(base_entries, root)
    written = file_map(variant.get("write", []), root)
    removed = variant.get("remove", [])
    require(len(removed) == len(set(removed)) and set(removed) <= base_sources.keys()
            and not written.keys() & set(removed), "invalid removal in candidate recipe")
    candidate_sources = {p: source for p, source in base_sources.items() if p not in removed}
    candidate_sources.update(written)

    base_available = variant.get("git_base") != "shallow-missing"
    repo = destination / "repo"
    with tempfile.TemporaryDirectory(dir=destination) as tmp:
        full = Path(tmp) / "full"
        full.mkdir()
        git(full, "init", "--quiet", "--initial-branch=main", "--object-format=sha1", "--template=")
        git(full, "config", "core.autocrlf", "true")
        (full / ".gitattributes").write_bytes(ATTRIBUTES)
        write_inputs(full, base_sources, inputs)
        base_id = commit(full, "Fixture baseline")
        restore_checkout(full, base_sources)
        base = verify_checkout(full, base_id, base_sources, inputs, root)

        for path in removed:
            (full / path).unlink()
        write_inputs(full, written, inputs)
        head_id = commit(full, "Fixture candidate")
        restore_checkout(full, candidate_sources)
        candidate = verify_checkout(full, head_id, candidate_sources, inputs, root)
        verify_diff(full, base_id, head_id, base, candidate, root)
        if base_available:
            full.rename(repo)
        else:
            git(destination, "clone", "--quiet", "--depth=1", "--no-local",
                "--config", "core.autocrlf=true", str(full), str(repo))
            git(repo, "remote", "remove", "origin")
            require(git(repo, "rev-parse", "--is-shallow-repository").stdout.strip() == b"true"
                    and git(repo, "cat-file", "-e", f"{base_id}^{{commit}}", check=False).returncode != 0
                    and f"parent {base_id}\n".encode() in git(repo, "cat-file", "-p", head_id).stdout,
                    "trusted base must be an unavailable parent in the shallow clone")
            verify_checkout(repo, head_id, candidate_sources, inputs, root)

    baseline, base_ignored = inventory(base, profile, root)
    migrations, ignored = inventory(candidate, profile, root)
    baseline_discovery = discovery(baseline, base_ignored)
    require(not baseline_discovery["duplicate_forward"] and not baseline_discovery["unpaired"],
            "baseline must be unambiguous")
    actual = discovery(migrations, ignored)
    comparable = base_available and not actual["duplicate_forward"] and not actual["unpaired"]
    statuses = git_identity_status(base, candidate, baseline, migrations) if comparable else None
    selected = sorted(statuses["new"] + statuses["modified"]) if comparable else None
    actual["selected"] = selected
    declared = {"down": [], "ignored": [], "duplicate_forward": [], "unpaired": [], **variant["discovery"]}
    require(actual == declared, f"discovery mismatch. Observed {actual}, declared {declared}")
    declared_statuses = dict.fromkeys(STATUSES, [])
    if variant["git"] is not None:
        declared_statuses.update(variant["git"])
    require(statuses == (declared_statuses if variant["git"] is not None else None),
            f"Git status mismatch. Observed {statuses}, declared {variant['git']}")
    selection = ([m["path"] for m in migrations if m["version"] in selected and m["direction"] != "down"]
                 if comparable else None)
    verify_source_spans(variant, candidate, selection)
    verify_modes(manifest, profile_id, {}, baseline, base)
    verify_modes(manifest, profile_id, variant, migrations, candidate)
    outcomes = variant["outcomes"]
    require(set(outcomes) in ({"all"}, set(manifest["postgres"])), "outcome majors do not cover matrix")
    for outcome in outcomes.values():
        verify_outcome(outcome, manifest["policy"], candidate, selection, statuses)

    def describe(files, sources, inventory):
        forward = {m["path"] for m in inventory if m["direction"] != "down"}
        records = {}
        for path, data in files.items():
            record = {"source": sources[path], "sha256": hashlib.sha256(data).hexdigest()}
            if profile["adapter"] == "sqlx" and path in forward:
                record["sqlx_sha384"] = hashlib.sha384(data).hexdigest()
            records[path] = record
        return records

    return {
        "migration_root": root,
        "git": {"base": base_id, "head": head_id, "base_available": base_available,
                "identity_status": statuses},
        "applied_status": manifest["applied_status"],
        "baseline": baseline,
        "candidate": migrations,
        "files": {"base": describe(base, base_sources, baseline),
                  "candidate": describe(candidate, candidate_sources, migrations)},
        "discovery": actual,
        "selected_forward_paths": selection,
    }


def variants(manifest):
    for case in manifest["cases"]:
        for variant in case["variants"]:
            for profile in variant["profiles"]:
                yield f"{case['id']}/{variant['id']}/{profile}", variant, profile


def validate(manifest, inputs):
    cases = manifest["cases"]
    ids = {case["id"] for case in cases}
    reviewed = ("D01 D02 D03 D04 D05 G01 G02 G03 G04 R01 R02 R03a R03b R04a R04b R04c "
                "R05 R06 R07 T01 T02 T03 T04 S01 S02 S03 V01 V02")
    require(ids == set(reviewed.split()) and len(cases) == len(ids), "reviewed case IDs differ")
    require(manifest["applied_status"] == "unknown" and not manifest["policy"]["applied_history_required"],
            "Git-only labels must leave applied status unknown")
    require(manifest["policy"]["trusted_git_base_required"]
            and manifest["policy"]["check_precedence"] == [2, 3, 1, 0],
            "unexpected Git evidence or gate precedence")
    require(manifest["seeds_after_version"] == 1, "apply fixture seeds after version 1")
    referenced = {source for files in manifest["baselines"].values() for _, source in files}
    referenced.update(source for seeds in manifest["seeds"].values() for source in seeds)
    sqlx_only = {"D04", "D05", "T03", "T04", "S01"}
    for case in cases:
        pairs = set(case.get("paired_with", []))
        require(pairs <= ids - {case["id"]}, f"bad control reference in {case['id']}")
        require(case["variants"], f"no variants in {case['id']}")
        variant_ids = [v["id"] for v in case["variants"]]
        require(len(set(variant_ids)) == len(variant_ids), f"duplicate variant in {case['id']}")
        adapters = set()
        for variant in case["variants"]:
            profiles = variant["profiles"]
            require(profiles and len(profiles) == len(set(profiles))
                    and set(profiles) <= manifest["profiles"].keys(), "invalid variant profiles")
            require(set(variant.get("seeds", ["clean"])) <= manifest["seeds"].keys(),
                    f"unknown seed in {case['id']}")
            adapters.update(manifest["profiles"][profile]["adapter"] for profile in profiles)
            referenced.update(source for key in ("baseline_extra", "write")
                              for _, source in variant.get(key, []))
        expected_adapters = {"sqlx"} if case["id"] in sqlx_only else {"plain", "sqlx"}
        require(adapters == expected_adapters, f"incorrect adapter coverage in {case['id']}")
    require(referenced == inputs.keys(), "SQL input references and digest inventory differ")
    pairs = 0
    for label, variant, profile in variants(manifest):
        with tempfile.TemporaryDirectory(prefix="dmg02-") as tmp:
            try:
                make_fixture(manifest, inputs, variant, profile, Path(tmp))
            except (ValueError, subprocess.CalledProcessError) as error:
                raise ValueError(f"{label} failed verification. {error}") from error
        pairs += 1
    print(f"Validated {len(cases)} cases, {sum(len(c['variants']) for c in cases)} variants, "
          f"{pairs} Git pairs, {pairs * len(manifest['postgres'])} adapter/major combinations, "
          f"and {len(inputs)} SQL inputs.")


def materialize(manifest, inputs, run_id, destination):
    label, _, target = run_id.rpartition("/pg")
    require(target in manifest["postgres"], f"unknown target in {run_id!r}")
    match = next(((variant, profile) for name, variant, profile in variants(manifest) if name == label), None)
    require(match is not None, f"unknown fixture {run_id!r}")
    variant, profile_id = match
    profile = manifest["profiles"][profile_id]
    require(not destination.exists(), f"choose a new output path, {destination} already exists")
    with tempfile.TemporaryDirectory(dir=destination.parent, prefix="dmg02-") as tmp:
        staging = Path(tmp) / "fixture"
        staging.mkdir()
        record = make_fixture(manifest, inputs, variant, profile_id, staging)
        seeds = {name: [{"source": source, "sha256": hashlib.sha256(inputs[source]).hexdigest()}
                        for source in manifest["seeds"][name]]
                 for name in variant.get("seeds", ["clean"])}
        record.update(
            id=run_id, split=manifest["split"], profile=profile_id,
            adapter=profile["adapter"], runner=profile["runner"], target_major=int(target),
            target_version=manifest["postgres"][target],
            runner_version=manifest["sqlx_version"] if profile["runner"] == "sqlx"
            else manifest["postgres"][target],
            policy=manifest["policy"], expected=variant["outcomes"].get("all", variant["outcomes"].get(target)),
            source_spans=variant.get("source_spans", []),
            forward_statement_count=variant.get("forward_statement_count"),
            seed_after_version=manifest["seeds_after_version"], seeds=seeds,
        )
        (staging / "case.json").write_bytes((json.dumps(record, indent=2) + "\n").encode("utf-8"))
        staging.rename(destination)
    return record


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("list", help="list runnable fixture IDs")
    commands.add_parser("validate", help="verify all fixture inputs and Git pairs")
    build = commands.add_parser("materialize", help="create a verified Git repository and case.json")
    build.add_argument("id")
    build.add_argument("destination", type=Path)
    args = parser.parse_args()
    try:
        manifest, inputs = read_corpus()
        if args.command == "list":
            for label, _, _ in variants(manifest):
                for target in manifest["postgres"]:
                    print(f"{label}/pg{target}")
        elif args.command == "validate":
            validate(manifest, inputs)
        else:
            materialize(manifest, inputs, args.id, args.destination.resolve())
            print(args.destination.resolve())
    except (KeyError, OSError, ValueError, subprocess.CalledProcessError) as error:
        if isinstance(error, subprocess.CalledProcessError):
            parser.exit(1, error.stderr.decode(errors="replace"))
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
