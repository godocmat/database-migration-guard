# Release preparation

A local candidate is ready for review after its native package, smoke checks, and evidence have been staged. Publication requires the release owner's approval and completion of the acceptance matrix. [Implementation plan](../PLAN.md#15-deployment-and-release-strategy)

## Build a native candidate

Use a clean checkout of the exact reviewed commit on the target platform. Install Git 2.43 or newer, Python 3.11 or newer with its latest maintenance fixes, and the platform's [native parser prerequisites](../CONTRIBUTING.md#native-postgresql-parser). Use the toolchain in the checked-in rust-toolchain.toml and the reviewed cargo-cyclonedx 0.5.9 binary. Verify the generator download with the producer's release attestation before using it. Record the OS image, Rust toolchain, Git, C compiler and linker, libclang, and any build flags or profile overrides in the release record. [cargo-cyclonedx release](https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/tag/cargo-cyclonedx-0.5.9)

Fetch the locked Cargo dependencies before the offline build. Run from the staging scripts' checkout and set the source to the reviewed build checkout. Choose a protected output parent directory that already exists. For an Ubuntu 24.04 x86-64 candidate:

~~~sh
TARGET=x86_64-unknown-linux-gnu
LIBCLANG_PATH=/usr/lib/llvm-18/lib
CLANG_PATH=/usr/bin/clang-18
export LIBCLANG_PATH CLANG_PATH
cargo fetch --locked --target "$TARGET" --manifest-path "$BUILD_SOURCE/Cargo.toml"
python3 -B scripts/stage-release.py stage \
  --source "$BUILD_SOURCE" --expected-commit "$REVIEWED_COMMIT" --target "$TARGET" \
  --cyclonedx "$CYCLONEDX_EXE" --output "$PACKAGE_DIR"
~~~

On Windows PowerShell, select x86_64-pc-windows-msvc with the MSVC developer environment and the Windows LLVM directory. On macOS arm64, select aarch64-apple-darwin with the Xcode tools and arm64 LLVM. Pass arguments to Python in the same order. The staging command verifies the commit, checkout cleanliness and native Rust host, then builds from the tracked Git archive with the locked offline release profile. Staging sets LF as Git's default text line ending and disables automatic CRLF conversion for the archive. It verifies the archived Cargo.lock and rust-toolchain.toml against their committed Git blobs before building. It sets SOURCE_DATE_EPOCH from the source commit and remaps the source, build and Cargo cache paths for Rust and C. On Windows MSVC it also uses deterministic C path mapping and linker options. Supply any additional Rust flags through CARGO_ENCODED_RUSTFLAGS when an argument contains spaces.

| Package file | Contents |
| --- | --- |
| dmg-VERSION-TARGET[.exe] | Native release executable |
| package.json | Commit, source timestamp, target, compiler version, lock, toolchain and SBOM generator hashes, and exact executable and SBOM sizes and hashes |
| dmg.cdx.json | CycloneDX 1.5 inventory for the target with artifact and source provenance |
| SHA256SUMS | SHA-256 values for the executable, SBOM and package.json |

Save the package.json digest printed by the command through a trusted review channel. The manifest documents a local build. Signed attestations identify the official build workflow. Rebuild with matching OS image, linker, SDK, compiler flags, and pinned inputs, then compare the unsigned executable and SBOM SHA-256 values. Record any mismatch before promotion.

The protected default branch includes manual workflows for native packaging, locked Rust dependencies, and the PostgreSQL rehearsal matrix. A configured GitHub repository is required to dispatch them. [Workflow setup and local verification](research/release-verification-ci.md)

### SBOM and notices

The native SBOM combines the locked cargo-cyclonedx inventory with packages Cargo compiled during this clean build. Target sources are marked required and host-only build dependencies are marked excluded. The record identifies relevant vendored PostgreSQL parser and cryptographic code and replaces workspace file URIs with stable local component identifiers. Its incomplete composition reflects the scope of a source inventory. Validate it against the CycloneDX 1.5 JSON schema or with a reviewed cyclonedx-cli validate --input-version v1_5 --fail-on-errors invocation. Optimized binary contents, the Rust toolchain and standard library, dynamic system libraries, host Git, and deployment components need separate evidence. [CycloneDX 1.5](https://github.com/CycloneDX/specification/blob/1.5/schema/bom-1.5.schema.json) · [CycloneDX CLI](https://github.com/CycloneDX/cyclonedx-cli#validate-command)

Review imported native libraries on each platform with a native inspection tool such as dumpbin /DEPENDENTS, readelf -d, or otool -L. Record and verify the expected OS runtimes on the clean test host, including the installed Visual C++ runtime where applicable. Review registry package licenses and compile the required redistribution notices for the exact release. Include the pinned parser's upstream licenses and vendored notices and the ring native source notices. For the CLI image, generate a separate filesystem SBOM with a reviewed Syft release or equivalent and retain the application SBOM for the executable. Scan the final locked Cargo graph and the image packages with reviewed advisory sources and record their database revisions. [Parser notices](research/postgresql-parser.md#build-and-redistribution-evidence) · [Ring notices](https://docs.rs/crate/ring/0.17.14/source/LICENSE) · [Syft documentation](https://oss.anchore.com/docs/overview/sbom/)

The native package notice review records the locked crate archives, included upstream texts, and outstanding runtime and image scope. The project license and final redistribution notices are pending an owner decision. Resolve both before distributing the release. [Third-party notice review](research/release-notices-review.md) · [Release acceptance](../PLAN.md#20-definition-of-done)

## Verify and smoke test an installation

Verify the manifest against the digest supplied through the trusted review channel before running the executable. The verifier checks the executable and SBOM and compares SHA256SUMS with the verified package. Use the staging scripts from a trusted checkout. They require Python for verification. The installed executable does not require Python or Rust.

~~~sh
python3 -B scripts/stage-release.py verify \
  --package "$PACKAGE_DIR" --manifest-sha256 "$PACKAGE_MANIFEST_SHA256"
~~~

On the clean host for the selected native target, install the verified executable in a separate directory. Supply local Git for revision checks. Keep the host offline after staging the reviewed package and prerequisites. Use the installed path for the following test. The smoke script creates its own repository, checks exact Git revisions and exercises pass, deny and incomplete status, then renders the saved analysis as HTML, SARIF and GitLab Code Quality. Each replay uses temporary local files and synthetic migrations.

~~~sh
"$INSTALLED_DMG" --version
python3 -B scripts/smoke-release.py "$INSTALLED_DMG"
python3 -B integrations/github/tests/replay.py --binary "$INSTALLED_DMG"
python3 -B integrations/gitlab/tests/replay.py --binary "$INSTALLED_DMG"
~~~

For an upgrade test, keep the previous verified executable and its exact Git, policy, and evidence pins. Replay the protected check on both binaries and smoke test the new candidate. Check expected rule changes in the release notes before updating a protected workflow. The GitHub replay works with the earlier pilot package as well. Update the binary and checksum together. For a rollback, restore the prior executable and its exact pins from the previous verified release and re-run the gate on the current reviewed revisions.

Use the platform runbooks for the pinned Windows replay and the native macOS arm64 acceptance check. [Windows install, upgrade and rollback](research/windows-offline-acceptance.md) · [Native macOS build and installation](research/hosted-release-acceptance.md#native-macos-arm64)

## Linux x86-64 CLI container

Start with the verified Linux x86-64 executable built on the documented glibc baseline. The CLI runs as UID and GID 65532 and records the reviewed executable's source commit in the OCI revision label. Its context holds the binary and the locked Git HTTPS runtime packages. The GitLab job adds Bash, Python and the reviewed gate script. Both recipes use a digest-pinned Chainguard Wolfi base and verify the executable SHA-256, repository signature and locked package archives during the build. [CLI Containerfile](../integrations/cli/Containerfile) · [GitLab job image](gitlab-ci.md#prepare-the-pinned-image)

~~~sh
context="$(mktemp -d)"
python3 -B scripts/stage-container-packages.py cli "$context/runtime-packages" --from-dir "$WOLFI_SNAPSHOT"
install -m 0555 "$VERIFIED_LINUX_DMG" "$context/dmg"
docker buildx build --platform linux/amd64 --network none --load \
  --file integrations/cli/Containerfile \
  --build-arg "DMG_BINARY_SHA256=$DMG_BINARY_SHA256" \
  --build-arg "DMG_SOURCE_COMMIT=$REVIEWED_COMMIT" \
  --tag dmg:review "$context"
~~~

The builder in this example must be able to resolve the pinned base. For disconnected builds, supply the verified base OCI layout as described below. Verify the final image's manifest digest, architecture, base, entry point, default user, executable and image SBOM. Use the reviewed image by digest after promotion. Smoke test `--version` and a representative offline check. With a Linux checkout owned by the invoking nonroot user, run the container using that UID and GID so Git recognizes the source owner. Use a read-only checkout mount and send the report to stdout. Use a complete local checkout with its `.git` directory for this example.

~~~sh
docker run --rm --network none dmg:review --version
docker run --rm --network none --read-only --tmpfs /tmp:rw,noexec,nosuid,nodev,mode=1777 \
  --cap-drop ALL --security-opt no-new-privileges \
  --user "$(id -u):$(id -g)" --mount "type=bind,src=$PWD,dst=/workspace,readonly" \
  dmg:review check --repo /workspace --config /workspace/.dmg/dmg.toml \
  --policy /workspace/.dmg/policy.toml --base "$BASE_SHA" --head "$HEAD_SHA" \
  --format json > report.json
~~~

Use trusted policy and config files in the checkout in this example. Mount separate protected inputs when the source checkout is untrusted. Keep worker access separate. Use the native Linux CLI with an explicitly selected isolated Docker worker for rehearsal. [Rehearsal setup](simulation.md#run-from-the-cli)

### Retain the container build inputs

Use the retained snapshot from the release inputs as `WOLFI_SNAPSHOT`. Its `x86_64/` directory holds the exact signed `APKINDEX.tar.gz` and all APK filenames listed in the two locks. For an online acquisition, run the following from the reviewed checkout. The GitLab set includes every archive needed by both images. [Common lock](../integrations/wolfi/cli.sha256) · [GitLab additions](../integrations/wolfi/gitlab-extra.sha256)

~~~sh
python3 -B scripts/stage-container-packages.py gitlab "$WOLFI_SNAPSHOT"
~~~

The public Wolfi index rolls forward and older archives eventually expire. A changed index or missing package requires the retained snapshot for this review. When refreshing a pin, verify the new base signature, stage the matching signed index and archives, update both locks and the recipes' lock digests, rebuild both images and rescan them. Retain the new signed inputs with the release evidence. [Wolfi package retention](https://github.com/wolfi-dev/#what-packages-are-included-in-wolfi)

Verify the exact `FROM` image with Cosign before copying it to a local OCI layout. The copy must preserve the upstream digest. [Chainguard signing identity](https://edu.chainguard.dev/open-source/sigstore/policy-controller/policies/using-policy-controller-to-verify-signed-chainguard-images/) · [Skopeo copy](https://github.com/podman-container-tools/skopeo/blob/main/docs/skopeo-copy.1.md)

~~~sh
wolfi_image=$(awk '$1 == "FROM" {print $2}' integrations/cli/Containerfile)
cosign verify \
  --certificate-identity 'https://github.com/chainguard-images/images/.github/workflows/release.yaml@refs/heads/main' \
  --certificate-oidc-issuer 'https://token.actions.githubusercontent.com' "$wolfi_image"
skopeo copy --preserve-digests "docker://$wolfi_image" "oci:$WOLFI_BASE_LAYOUT:review"
~~~

On the disconnected Linux build host, use the staged CLI context above with BuildKit and the saved OCI layout. For GitLab, stage its context and set `role=gitlab`. Use the recipe and staging helper from the recorded `RECIPE_COMMIT` and the same BuildKit version, build inputs and exporter name across independent builds. Set `REVIEWED_COMMIT` to the executable's source revision. [GitLab context](gitlab-ci.md#prepare-the-pinned-image) · [BuildKit local OCI layouts](https://github.com/moby/buildkit/blob/v0.34.1/docs/reference/buildctl.md#local-sources) · [BuildKit timestamp controls](https://github.com/moby/buildkit/blob/v0.34.1/docs/build-repro.md#source_date_epoch)

~~~sh
role=cli
wolfi_image=$(awk '$1 == "FROM" {print $2}' "integrations/$role/Containerfile")
epoch=$(git show -s --format=%ct "$RECIPE_COMMIT")
image_name="dmg-$role:$(printf '%.7s' "$REVIEWED_COMMIT")"
touch -d "@$epoch" "$context/dmg"
if [ "$role" = gitlab ]; then
  touch -d "@$epoch" "$context/integrations/gitlab/gate.py"
fi
buildctl build --frontend dockerfile.v0 \
  --local "context=$context" --local "dockerfile=integrations/$role" \
  --oci-layout "wolfi-base=$WOLFI_BASE_LAYOUT" \
  --opt "context:$wolfi_image=oci-layout://wolfi-base@${wolfi_image##*@}" \
  --opt filename=Containerfile --opt platform=linux/amd64 --opt force-network-mode=none \
  --opt "build-arg:DMG_BINARY_SHA256=$DMG_BINARY_SHA256" \
  --opt "build-arg:DMG_SOURCE_COMMIT=$REVIEWED_COMMIT" \
  --opt "build-arg:SOURCE_DATE_EPOCH=$epoch" \
  --output "type=oci,name=$image_name,dest=dmg-$role.oci.tar,rewrite-timestamp=true"
~~~

Compare the OCI manifest and archive SHA-256 from each independent build. The recipes retain the runtime loader cache and discard the regenerable `ldconfig` auxiliary cache during package installation. Its file identities include filesystem-specific inode and device numbers and change times. [glibc auxiliary cache implementation](https://github.com/gnutools/glibc/blob/48fa38b9218add3a1e5c38c349a3e481a5fcf245/elf/cache.c#L968-L987)

## Attest and verify approved artifacts

Have the release owner build and attest the exact artifacts from the reviewed source in a protected workflow. Keep build and verification jobs read-only. Grant OIDC signing and attestation permissions only to the authorized attestation job and registry or release write access only to the authorized publishing job. Pin actions to reviewed commit IDs. Attest the binary and package.json as build outputs, bind the SBOM to the binary with an SBOM attestation, and attest the image using its immutable registry digest. Keep the approved workflow identity, source commit, artifact digests and signed evidence with the release. [GitHub artifact attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations)

In a GitHub Actions-based release, use the separately published trusted repository, workflow and full source commit when verifying. For offline verification, acquire the attestation bundles and trusted roots before disconnecting and follow the GitHub CLI instructions for local bundles.

~~~sh
gh attestation verify "$VERIFIED_NATIVE_DMG" --repo "$RELEASE_REPO" \
  --signer-workflow "$RELEASE_WORKFLOW" --source-digest "$REVIEWED_COMMIT"
gh attestation verify "$PACKAGE_DIR/package.json" --repo "$RELEASE_REPO" \
  --signer-workflow "$RELEASE_WORKFLOW" --source-digest "$REVIEWED_COMMIT"
gh attestation verify "oci://$IMAGE_REF_BY_DIGEST" --repo "$RELEASE_REPO" \
  --signer-workflow "$RELEASE_WORKFLOW" --source-digest "$REVIEWED_COMMIT"
~~~

Verify the SBOM attestation's predicate type and confirm that its document matches the package SBOM. Use the attested package.json digest for local package verification. Verify downloads before installation or use in CI. [GitHub CLI verification options](https://cli.github.com/manual/gh_attestation_verify)

### Local Windows staging evidence

Two native stages of revision `ae83f53227ad02ad20c1b7e0dcdb090192574c75` used separate tracked source extractions and build directories on the same Windows 11 x86-64 host. They used Rust 1.100.0, MSVC 19.44.35230, Windows SDK 10.0.26100.0, libclang 21.1.8 and the same locked dependency cache. Both produced a 13,603,328-byte executable with SHA-256 `002a0b0e83f3249c7d93861b9dfbcc1b25daa7fdfec6d3b8f108d21cee35e149` and a CycloneDX 1.5 SBOM with SHA-256 `8a3ce5b54c5074236744a31381406f2efd429129a9be7b63001597d6e8de913c`. The second stage recorded the SBOM generator hash in the manifest. This evidence covers one matched build environment.

The package pin and the official CycloneDX 1.5 schema validated. The installed-path smoke check passed 0, 1 and 3 gate outcomes and saved-report rendering with the host's installed Git and Visual C++ runtime. The verifier rejected an incorrect package manifest pin, changed checksums, and changed binary and SBOM bytes. A clean disconnected installation remains an acceptance item.

## Acceptance matrix

Record the exact source revision, artifact SHA-256 or OCI digest, environment, and commands for each final check. Existing evidence applies to the revisions listed in [PROGRESS.md](../PROGRESS.md).

| Release check | Evidence and remaining work |
| --- | --- |
| Static analysis and supported adapters on PostgreSQL 15–18 | Local parser and native runner fixtures recorded. Integrated Windows `af21401` matched all 130 public CLI cases and all 100 frozen holdout gate outcomes. [Accuracy and scope](../PROGRESS.md#dmg-15-frozen-holdout-rescore) |
| Authorized inspection and schema comparison | Restricted-role TLS inspection, native history and Windows inspection-to-diff matrix recorded across PostgreSQL 15–18. |
| Disposable rehearsal | PostgreSQL 15–18 passed cgroup v2 lifecycle, deadlines, and 36 rehearsal cases. PostgreSQL 18.7 also passed the live seeded and schema-only CLI workflows and the exact manual CI wrapper. [Cgroup v2 evidence](research/simulation-cgroup-v2-verification.md) · [Wrapper and schema-only checks](research/release-verification-ci.md#offline-wrapper-and-schema-only-replay) |
| Policy and report formats | Windows tests, fixture replay and OASIS SARIF validation recorded. Hosted consumption remains pending. |
| Protected GitHub and GitLab | Local replays and template validation passed. The GitHub pilot uploader uses the reviewed Node 24 pin. Both providers still need configured pilot projects to verify protected merges, fork behavior and artifact access. [Hosted acceptance runbook](research/hosted-release-acceptance.md) |
| Windows x86-64 package | The normalized `2d3e5e0` candidate reproduced across two native stages. Package verification, installation, upgrade and rollback passed on the existing host. A disconnected Windows x86-64 guest is needed for the clean installation run. [Windows verification](research/integrated-windows-verification.md) · [Disconnected runbook](research/windows-offline-acceptance.md) |
| Linux x86-64 package | Integrated `af21401` staged twice with byte-identical artifacts in a disconnected Ubuntu 24.04.5 guest. Offline install, local CI replays, the public corpus, rollback, and CycloneDX 1.5 checks passed. [Linux verification](research/integrated-linux-verification.md) |
| macOS arm64 package | Native build, clean installation, upgrade and rollback remain pending. [macOS acceptance runbook](research/hosted-release-acceptance.md#native-macos-arm64) |
| Linux x86-64 CLI and GitLab images | Both Wolfi OCI archives reproduced byte for byte in fresh disconnected guests. CLI decisions, all ten installed GitLab replays, and Git HTTPS trust checks passed. Both SBOMs validated. The original eight advisories are resolved in these images. A separate CPython check records one Low residual in GitLab. [Runtime and reproducibility](research/updated-container-runtime.md) · [Advisory evidence](research/image-advisory-acceptance.md) |
| Performance and accuracy | The cross-built Linux `af21401` executable met the 2-vCPU, 8-GiB planning budgets. The native Windows `af21401` holdout matched 100 of 100 gate outcomes and 56 of 64 exact findings. Review the remaining discrepancies for release. [Benchmark evidence](../fixtures/dmg-15/README.md) · [Holdout and boundary results](../PROGRESS.md#dmg-15-frozen-holdout-rescore) |
| Release integrity and licensing | Package hashes, validated SBOMs and draft notices are available. Completing the corresponding-source bundle, selecting a project license, approving notices, signing provenance and approving publication remain pending. [Native notice review](research/release-notices-review.md) · [Wolfi notice and source review](research/image-redistribution-review.md) |
