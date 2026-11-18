# Manual release verification on GitHub

Dispatch either workflow from the protected default branch on GitHub.com. Each job uses the commit recorded by the dispatch and checks out that exact revision with read-only repository access. GitHub must see the workflow on the default branch to enable manual dispatch. [Workflow dispatch](https://docs.github.com/en/actions/how-tos/manage-workflow-runs/manually-run-a-workflow) · [Hosted runner labels](https://docs.github.com/en/actions/how-tos/write-workflows/choose-where-workflows-run/choose-the-runner-for-a-job)

## Native packages

The [native verification workflow](../../.github/workflows/release-verification.yml) stages an unsigned package twice on each of Ubuntu 24.04 x64, Windows Server 2025 x64, and macOS 15 arm64. It uses separate Git archive extractions and build directories on the same runner image. It requires the two executables, manifests, SBOMs and checksum lists to match. The installed copy runs the local smoke check and the GitHub and GitLab synthetic replays. A passing job retains a tar archive with the binary's executable mode. Verify the package against its manifest SHA-256 from the trusted job log after unpacking. [Package contents and local verification](../release.md#verify-and-smoke-test-an-installation)

The workflow fetches the locked Cargo dependencies before staging. It verifies each cargo-cyclonedx 0.5.9 archive against its pinned SHA-256 and the producer workflow attestation. It validates the SBOM with the pinned CycloneDX 1.5 schemas and hash-pinned Python wheels. The RustSec job checks the exact `Cargo.lock` against its pinned advisory database using cargo-audit 0.22.2. It keeps its JSON output and database revision with the run and skips live registry yank checks. [Generator release and attestations](https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/tag/cargo-cyclonedx-0.5.9) · [RustSec advisory database](https://github.com/RustSec/advisory-db)

The build runner supplies the operating system, compiler, and SDK. The logs record the compiler, linker or SDK details, Rust toolchain, and package digests. Matching artifacts from one runner establish reproducibility for those matching inputs. Separate disconnected installations, upgrade and rollback on each target, signed provenance, and publication approval are release acceptance items. [Acceptance matrix](../release.md#acceptance-matrix)

## Rehearsal

The [disposable rehearsal workflow](../../.github/workflows/rehearsal-verification.yml) runs one Ubuntu 24.04 job for each approved PostgreSQL 15–18 image. Each job builds the existing integration fixture with locked Cargo dependencies and preloads the exact linux/amd64 image. The fixture starts Docker Engine 29.9.3 with a private Unix socket and data directory in a separate mount and loopback-only network namespace. It checks the daemon version, storage and cgroup drivers, and loaded image configuration ID. The three test groups exercise lifecycle, separate server and wall deadlines, and transactional rehearsal. The job requires an empty container inventory after each group and stops its daemon on exit. [Image pins and fixture controls](simulation-worker.md#images-and-resources) · [Offline cgroup v2 evidence](simulation-cgroup-v2-verification.md)

Local verification covered actionlint 1.7.12, ShellCheck 0.11.0, Bash and PowerShell parsing, and all three generator attestations. The pinned CycloneDX schemas accepted the staged Windows and Linux `af21401` packages. The package helper verified the two locally built Windows packages, replayed both CI providers from a temporary install, and rejected mismatched digests and invalid SBOMs. The pinned RustSec scan found no advisories or warnings in that lockfile. GitHub-hosted dispatch and hosted branch protection enforcement still need verification in a configured repository.

GitHub CLI 2.103.3 verified the retained CycloneDX Linux generator archive with its local attestation bundle, staged Sigstore roots, and pinned source and signer commits. The same command rejected an incorrect signer commit. The Guard release will need its own approved signer and bundles. [Attest and verify approved artifacts](../release.md#attest-and-verify-approved-artifacts)

### Offline wrapper and schema-only replay

On 2026-11-13, a fresh Ubuntu 24.04.5 x86-64 guest ran the exact [CI fixture script](../../scripts/verify-rehearsal-ci.sh) from `37f46dac5bdd1e7ca0e45c4dbc3a9a6b92529c2a` via `sudo --non-interactive -- unshare --mount --net --propagation private -- /usr/bin/bash`. The guest used the verified Canonical cloud image and kernel `6.8.0-147-generic` under QEMU 11.1.0 with WHPX, two vCPUs, 4 GiB of RAM, and no network interface beyond loopback. The script checked the locally preloaded PostgreSQL 18.7 image configuration ID and confirmed Docker Engine 29.9.3 with `vfs`, cgroup v2, and systemd. [Image configuration pin](simulation-worker.md#images-and-resources) · [Offline host and test binary provenance](simulation-cgroup-v2-verification.md)

The same script exited 2 outside the private mount namespace. The isolated run passed lifecycle in 15.54 seconds, server and wall deadlines in 5.68 seconds, and all nine rehearsal cases in 27.55 seconds. Container inventory was empty after every group. A synthetic existing containerd socket kept its inode in the outer namespace, exercising the socket-masking path. The wrapper exited with its fixture directory and both daemon processes gone.

A second daemon in the same guest ran a live schema-only rehearsal with the native Linux `af21401` CLI. The planned binding matched the saved report, including the schema baseline digest. The report recorded no seed, a committed column addition, and complete before and after observations and schema diff. An authenticated `dmg check` passed with exit 0. Omitting the report returned 3. The report rendered to HTML and Markdown without changing its bytes. The second daemon also had zero containers and exited. The guest recorded status 0 and powered down. [Native Linux CLI](integrated-linux-verification.md)

| Verification input or output | SHA-256 |
| --- | --- |
| CI fixture script | `983b5835df6c014805473943e0808236634b869112262fa4e00ca7d720a5714b` |
| PostgreSQL 18.7 image archive | `c2fcd8cf1f6b9e38c4e63ed30cc7a175c8f3e538852207eea467cd5b604c2cc9` |
| Cross-built integration test binary from `8edf66f` | `014a858f422b7f1daa01626e27b93e703fc764e7f2243a30c5625257529836cd` |
| Native Linux CLI | `503ccfcd47ba561d826a4d0cea0b0da6723d260a421f26677b9bc98175093ea2` |
| Schema-only rehearsal report | `4390fbf40b4c2b99a464e6fded1025a8703ff28efa9c09eee68839cd465d6141` |
| Guest serial log | `2c0c521e29182ed6e55d797cdfa9b3ddcbba4e17a6ede1c5190e2c5167e009b0` |

The local replay used the previously verified locked test binary with matching simulation and dependency source trees. It exercised the CI runtime wrapper on PostgreSQL 18.7. The [earlier cgroup v2 matrix](simulation-cgroup-v2-verification.md#worker-test-scope) covers all four releases with a separate local wrapper.
