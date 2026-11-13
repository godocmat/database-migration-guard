"""Describe the Cargo sources selected by one release build."""

from __future__ import annotations

import json
from pathlib import Path

GENERATOR_VERSION = "0.5.9"
MAX_SBOM_BYTES = 8 * 1024 * 1024

PG_QUERY_SOURCE = "https://github.com/pganalyze/libpg_query/tree/c1546e7e97edc93fe8474d7a59881e91fbf48cfc"


def cargo_artifacts(output, target_dir, target):
    """Identify packages Cargo compiled for the target or only for the build host."""
    runtime_dir = target_dir / target / "release"
    packages = {}
    for line in output.splitlines():
        event = json.loads(line)
        if event["reason"] != "compiler-artifact":
            continue
        kind = event["target"]["kind"]
        is_target = not {"custom-build", "proc-macro"}.intersection(kind) and any(
            Path(filename).is_relative_to(runtime_dir) for filename in event["filenames"]
        )
        package = event["package_id"]
        packages[package] = packages.get(package, False) or is_target
    return packages


def property_value(name, value):
    return {"name": name, "value": value}


def vendored_component(ref, name, version, license_id, url):
    component = {
        "bom-ref": ref,
        "type": "library",
        "name": name,
        "scope": "required",
        "externalReferences": [{"type": "vcs", "url": url}],
        "properties": [property_value("dmg:artifact-role", "vendored-target-source")],
    }
    if version is not None:
        component["version"] = version
    if license_id is not None:
        component["licenses"] = [{"license": {"id": license_id}}]
    return component


def augment_native_components(components, dependencies):
    refs = {(component["name"], component["version"]): component["bom-ref"]
            for component in components}
    parser = refs.get(("pg_query", "18.1.0"))
    ring = refs.get(("ring", "0.17.14"))
    if parser is None or ring is None:
        raise ValueError("Review the vendored SBOM components after a native dependency update.")

    native = "dmg:vendored:libpg_query"
    postgres = "dmg:vendored:postgresql-parser"
    xxhash = "dmg:vendored:xxhash"
    upb = "dmg:vendored:upb"
    utf8 = "dmg:vendored:utf8-range"
    ring_native = "dmg:vendored:ring-native"
    components.extend([
        vendored_component(native, "libpg_query", "18.1.0", "BSD-3-Clause", PG_QUERY_SOURCE),
        vendored_component(postgres, "PostgreSQL parser sources", "18.6", "PostgreSQL",
                           f"{PG_QUERY_SOURCE}/src/postgres"),
        vendored_component(xxhash, "xxHash", "0.8.0", "BSD-2-Clause",
                           f"{PG_QUERY_SOURCE}/vendor/xxhash"),
        vendored_component(upb, "upb", None, "BSD-3-Clause", f"{PG_QUERY_SOURCE}/vendor/upb"),
        vendored_component(utf8, "utf8_range", None, "MIT",
                           f"{PG_QUERY_SOURCE}/vendor/upb/third_party/utf8_range"),
        vendored_component(ring_native, "ring vendored native sources", "0.17.14", None,
                           "https://docs.rs/crate/ring/0.17.14/source/"),
    ])
    dependencies[parser].add(native)
    dependencies[ring].add(ring_native)
    dependencies[native] = {postgres, xxhash, upb}
    dependencies[upb] = {utf8}
    for ref in (postgres, xxhash, utf8, ring_native):
        dependencies[ref] = set()


def normalize(raw, packages, target, commit, binary_digest):
    bom = json.loads(raw)
    metadata = bom["metadata"]
    main = metadata["component"]
    if (bom["bomFormat"], bom["specVersion"], main["type"], main["name"]) != (
        "CycloneDX", "1.5", "application", "dmg"
    ):
        raise ValueError("Expected a CycloneDX 1.5 BOM for the dmg binary.")
    if property_value("cdx:rustc:sbom:target:triple", target) not in metadata["properties"]:
        raise ValueError("The SBOM target does not match the build target.")
    original = {component["bom-ref"]: component for component in [main, *bom["components"]]}
    if not packages.get(main["bom-ref"]) or packages.keys() - original.keys():
        raise ValueError("The SBOM does not cover every package compiled by Cargo.")

    aliases = {}
    for ref, component in original.items():
        if ref not in packages:
            continue
        if ref.startswith("path+file:"):
            name = "dmg-cli" if component is main else component["name"]
            aliases[ref] = f"dmg:workspace:{name}@{component['version']}"
            component.pop("purl", None)
        else:
            aliases[ref] = ref
        component["bom-ref"] = aliases[ref]
        component["scope"] = "required" if packages[ref] else "excluded"
        role = "target-source" if packages[ref] else "build-only"
        component.setdefault("properties", []).append(property_value("dmg:artifact-role", role))
    if len(set(aliases.values())) != len(aliases):
        raise ValueError("The normalized SBOM references are ambiguous.")

    dependencies = {ref: set() for ref in aliases.values()}
    for entry in bom["dependencies"]:
        if entry["ref"] in aliases:
            dependencies[aliases[entry["ref"]]].update(
                aliases[ref] for ref in entry.get("dependsOn", ()) if ref in aliases
            )
    components = [component for ref, component in original.items()
                  if ref in aliases and component is not main]
    augment_native_components(components, dependencies)

    main["hashes"] = [{"alg": "SHA-256", "content": binary_digest}]
    metadata["properties"].extend([
        property_value("dmg:source-commit", commit),
        property_value("dmg:sbom-scope", "Cargo build sources and identified vendored sources"),
    ])
    bom["components"] = sorted(components, key=lambda component: component["bom-ref"])
    bom["dependencies"] = [{"ref": ref, "dependsOn": sorted(edges)}
                           for ref, edges in sorted(dependencies.items())]
    bom["compositions"] = [{"aggregate": "incomplete", "assemblies": [main["bom-ref"]]}]
    result = json.dumps(bom, ensure_ascii=False, indent=2, sort_keys=True) + "\n"
    if "file:/" in result or len(result.encode("utf-8")) > MAX_SBOM_BYTES:
        raise ValueError("The normalized SBOM contains a local file URI or exceeds its size limit.")
    return result
