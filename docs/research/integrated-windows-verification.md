# Integrated Windows x64 release candidate

The local native release package was staged on 2026-11-13 from clean commit `af2140159686067995a4d374ea824400f5453d07`. It reports `dmg 0.1.0` and uses ruleset 2. [Release staging procedure](../release.md#build-a-native-candidate)

| Package file | Bytes | SHA-256 |
| --- | ---: | --- |
| `dmg-0.1.0-x86_64-pc-windows-msvc.exe` | 13,614,080 | `350d9c21207115080a4cd5050a48e1ee7fa730e60642c50067c588fd4f8049a2` |
| `dmg.cdx.json` | 234,564 | `ee61029a8f1cb0d8414f95f980f61cd7e7e81a6c86fda043de1af86df769793d` |
| `package.json` | — | `36e80b472f0f8cd48f5bd0921cd6a45b223ab0b1b66b12aec768462abcdb9544` |

The build used a fresh Git archive of the reviewed commit and a fresh target directory with an existing locked Cargo cache on Windows 11 Pro x64 build 26200.9974. Inputs were Rust and Cargo 1.100.0, MSVC 19.44.35230.0, linker 14.44.35230.0, Windows SDK 10.0.26100.0 and libclang 21.1.8. The archive's Cargo.lock SHA-256 was `52e64d2a9c272af997bbc8163330c81b19ddbfc4239124a50bfcc519e07572d6`. The stage used source and build path remapping, deterministic C compilation and the MSVC `/Brepro` linker option. PE inspection found the reproducible debug record and dependencies on the Visual C++ runtime and UCRT. The test host supplied `VCRUNTIME140.dll` 14.51.36247.0 and `ucrtbase.dll` 10.0.26100.9711.

The verifier checked the manifest pin, exact executable and SBOM bytes, and SHA256SUMS. Disposable copies with a wrong manifest pin, modified checksum list, or changed executable or SBOM failed verification. The stage also rejected a mismatched Git revision and an occupied output directory. The CycloneDX 1.5 schema and dependency-reference checks passed with cargo-cyclonedx 0.5.9, generator SHA-256 `4715a01af8fa7b5fb8bfc211ebffdadcfdaf75f21528e41e8baee95ad87d4808`. The SBOM binds the executable hash and records 136 target sources, 46 build-only components and six vendored target sources. Its incomplete composition describes the scope of the source inventory. [CycloneDX 1.5 schema](https://github.com/CycloneDX/specification/blob/1.5/schema/bom-1.5.schema.json)

## Local checks

- Rust formatting, locked offline workspace tests and strict all-target Clippy passed. The default workspace run passed 152 tests and skipped six explicit opt-in fixtures.
- The opt-in discovery and rule corpus tests passed 24 distinct discovery recipes and all 130 public discovery, parser and rule combinations. The staged executable independently matched all 130 expected CLI outcomes, with 48 passes, 58 denials and 24 incomplete results. [Public fixtures](../../fixtures/dmg-02/README.md)
- The GitHub replay verified pass, denied and incomplete with original exits 0, 1 and 3. All ten GitLab replay tests passed, including exact revisions and parents, protected base inputs, fork advisory and report retention. [GitHub replay](../../integrations/github/tests/replay.py) · [GitLab replay](../../integrations/gitlab/tests/replay.py)
- The verified Windows pilot from `c4974cd11180440d710c0843a952caa11ce786e3` and the candidate ran from separate temporary install paths. Both passed offline initialization, policy validation, worktree scan and exact Git check exits 0, 1, 2 and 3. The candidate rendered the pilot's saved ruleset 1 report and produced separate PG001 and PG002 findings in a ruleset 2 report. SARIF passed its 2.1.0 schema. The installed smoke covered HTML, SARIF and GitLab Code Quality, and the candidate also ran a saved schema comparison and bound a plan-only rehearsal. Restoring the previous executable preserved its tested behavior and both binary digests. [Installed smoke](../../scripts/smoke-release.py) · [Earlier Windows verification](clean-install-verification.md)

A second stage used a separate Git archive extraction and target directory on the same host with the same tools and cache. Both stages produced byte-identical executables, SBOMs, manifests and SHA256SUMS. Both package manifest and SBOM checks passed.

The archive line-ending fix was verified on branch revision `2d3e5e017600b7553fb3377c42633ce4d09992d9`. Two independent stages with opposing caller CRLF and LF defaults produced byte-identical packages. Both archived lockfile and toolchain hashes matched the committed LF blobs and the native Linux inputs. The rebuilt Windows executable SHA-256 is `4f318dfaf46678a716e8a66a6e03b8f833ed14b2cc3bf3946bfb01fe8e7d576d` and its manifest SHA-256 is `eb875e3be11e1504d683849ff19def662c6658c7a377c7b2937db82c95f6c619`. Manifest, CycloneDX, installed smoke, both local CI replays, and all 130 public CLI checks passed again. [Native Linux inputs](integrated-linux-verification.md#package-verification)

The installation tests used isolated local profiles and temporary Git repositories with Git for Windows 2.45.1 and Python 3.11.6. Host networking stayed enabled. The package is unsigned. Clean installation on a disconnected Windows VM, hosted CI enforcement and signed release acceptance remain pending.
