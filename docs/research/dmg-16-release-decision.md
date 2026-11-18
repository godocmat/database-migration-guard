# DMG-16 release decision

**Hold Core v1 distribution.** The proposed source is protected public `main` at `3b0c23de74c6a7d1e03206dbd7b3d60229f14bde`. Local `master` was clean at `4dc8d4e4720c033af99a946b1921b91f9ca1f214` at handoff. The diff from the public candidate to that local revision is documentation only. The candidate's committed Cargo.lock SHA-256 is `aa374723aa1e78466cc54a42bf63195e7e66790aa843495d6e8263f6895b4221`. [Public candidate](https://github.com/godocmat/database-migration-guard/commit/3b0c23de74c6a7d1e03206dbd7b3d60229f14bde) · [Acceptance matrix](../release.md#acceptance-matrix)

## Native verification

Both manual workflows were dispatched in parallel on 18 November 2026 using free standard runners in the public repository. Both finished successfully at the proposed source on `main`. The proposed native packages are the hosted Linux package, the independently reproduced local Windows package, and the macOS package from the full installation run. [Native verification](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/attempts/2) · [Rehearsal verification](https://github.com/godocmat/database-migration-guard/actions/runs/44409812537)

| Job | Result |
| --- | --- |
| [Linux x86-64](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/job/135174619445) | Passed. Two matching native stages, CycloneDX 1.5 validation, installed smoke, and local GitHub and GitLab replays. |
| Windows x86-64 | [First attempt](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/job/135174619786) stopped before packaging when the public GitHub API returned HTTP 403 for the pinned CycloneDX generator's producer attestation. [Second attempt](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/job/135184632890) passed two matching native stages, SBOM validation, installed smoke and both local provider replays. |
| macOS arm64 | [First attempt](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/job/135174619933) ended canceled before a runner was assigned. [Second attempt](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/job/135184632553) passed two matching native stages, SBOM validation, installed smoke and both local provider replays. |
| [Locked Rust dependency audit](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/job/135174620096) | Passed. Cargo Audit 0.22.2 found zero vulnerabilities and zero warnings across 226 locked packages against advisory database `4ef2df8e806a65e2f1ecc14f56f51249062b23d6`. The audit skipped online yank checks. |

GitHub refused an individual Windows rerun while the workflow was active. The failed-job retry after completion recovered both Windows and macOS at the same source, with the earlier Linux and audit jobs carried forward. The first attempt's retained log ZIP hashes to `287e30a8f704e70a74793559318fbe24a63e0a9cc4a32556535545926e270783`. The first Windows job log hashes to `5d2936459c00ec13718bfff5674de3b0b2df271b8a7ff8aeaa331814346e160f`. The second attempt's retained log ZIP hashes to `6f7afe98b21dc266caffef24d27107ee6a5be870c84835dbc0b592badc4d5cf2`. [First attempt](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/attempts/1) · [Second attempt](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/attempts/2)

The downloaded Linux ZIP matched GitHub's API digest, its inner tar and manifest matched the runner log, and the package verifier checked the executable, SBOM and checksum file. The executable is byte-identical to the older Linux build already used for clean offline installation, same-path upgrade, rollback and both local CI replays in Ubuntu 24.04.5. The current manifest and SBOM carry the current source revision. [Linux Actions artifact](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/artifacts/14477232790) · [Disconnected Linux evidence and byte comparison](dmg-16-candidate-evidence.md#exact-x86-64-install-revalidation)

| Linux package or transport | SHA-256 |
| --- | --- |
| Executable | `503ccfcd47ba561d826a4d0cea0b0da6723d260a421f26677b9bc98175093ea2` |
| Package manifest | `87de1ab7582eaad7fa140ae6abf1235d03d6943654a46682718b2d7560cb62df` |
| CycloneDX SBOM | `d4ef04a624d77d889435f886dc73ec95eff5b75511b6e1c34874d4b8e0e8c2a0` |
| Inner tar.gz | `051c5029d3148762387823b612f9ed14b47ee715ebfefd19954c9aa8d09b6963` |
| GitHub artifact ZIP | `714ec2d2c9aa67aee8d18f676e9f17c5c3bb2c3663d61efac73cbc119d4ecf9a` |
| Retained Linux job log | `c9d4ab55ebb79724bd22782ea0a7ed92e37e121df0aad55ef3dfbf7d7766c6a6` |

The downloaded audit ZIP matched its GitHub SHA-256 `e97f5d5f615dc7f9f7879a914e53737932d15dc02f63556fb8fcbf6849e9ce0d`. The retrieved `cargo-audit.json` hashes to `c5a0f47fb78e83976a6ff4512988fe33b56dfa3117f35a67dc7a7f2d480c109f`. [Audit artifact](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/artifacts/14475783046)

A separate clean Windows worktree at the exact `3b0c23d` source reproduced all four package files across two independent stages on Windows 11 Pro build 26200.9974 with the verified Cargo CycloneDX generator, locked offline dependencies, Rust 1.100.0, MSVC 19.44.35230, Windows SDK 10.0.26100.0 and libclang 21.1.8. The SBOM passed the pinned CycloneDX 1.5 schema. A local installed-path run passed installation, upgrade, saved-report rendering, both provider replays and rollback. The executable matched both the retained `2d3e5e0` package and the exact candidate member of the transfer ZIP used in the Windows Server 2025 Evaluation Core guest with no virtual NIC. Its verified disconnected install, upgrade and rollback apply to those same executable bytes. The new manifest and SBOM bind them to `3b0c23d`. [Disconnected runtime evidence](windows-offline-acceptance.md#local-windows-server-evaluation-guest)

| Local Windows package file | SHA-256 |
| --- | --- |
| Executable | `4f318dfaf46678a716e8a66a6e03b8f833ed14b2cc3bf3946bfb01fe8e7d576d` |
| Package manifest | `1dfc25d348967047bc1375186cac32b911f809e2e12a27648bf48f3b9563394e` |
| CycloneDX SBOM | `74fd107f5663a07eda2bf6cc7b1f3903ca68b3233e51b1037f59cf490e848d65` |
| SHA256SUMS | `da0f64a772d284b8f966398bf9555db5dcc4a93aa615786f058b8db5b156ad49` |
| Installed-path replay log | `d29424dfd8ca4a0b585fa9fb63e0e7973304a9d903a557cbb3f3d1bf77288c5d` |
| Retained disconnected guest log | `bdfef93855012e42de6021f1c089ae33e6fdf4d27e7281cb451cd16380c32a65` |

The selected Windows and Linux SBOMs retain the same component, dependency and composition records as the earlier same-byte packages. The SBOM metadata now records the current source commit and timestamp. The earlier draft notice coverage carries forward to the matching components. [Native notice inventory](release-notices-review.md#normalized-windows-and-native-linux-review-bundle)

The hosted Windows retry used MSVC 19.51.36260.0, linker 14.51.36260.0 and libclang 20.1.8. It produced a different PE x86-64 executable digest. The hosted and local SBOMs at `3b0c23d` match except for the metadata that binds each SBOM to its executable. The tested hosted artifact passed its installed smoke and both provider replays. Its exact digest would need a disconnected installation check if selected for release. The downloaded ZIP, inner tar and manifest verified against their respective pins. [Hosted Windows artifact](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/artifacts/14478936404)

| Windows retry package or transport | SHA-256 |
| --- | --- |
| Executable | `04cbea08a88a6522a55021e7d9df3f4cca42733ad29b747f49dbdf2356774eea` |
| Package manifest | `f3bbebf77c59b84575918ea8c199b35665d06ef044cb6c76ef0be2145cc2837b` |
| CycloneDX SBOM | `2f1ce5f9070650b9bb1c7fc07d2ae4ff874cae7e8cfb8f278851dd29868bd15e` |
| Inner tar.gz | `21bf0ecb5ed79692b91540030b04d5ef337b78b2ab300f897ed988b095136e81` |
| GitHub artifact ZIP | `06fa84d0354e8ce6f46163ada9183427874c43bd655f498cb305f10a591466cf` |
| Retained Windows job log | `966e91a66aad72686f103ff9d0b5a8a89133341b1a48708cb5e9b77f7180c180` |

The proposed macOS package comes from the full macOS run `44406264422`. That run reproduced the native package and passed clean install, upgrade, rollback, report rendering and both local provider replays on a fresh runner with Guard process networking denied. Its executable SHA-256 is `95eb4233ab8793850ad6272eb3f4c71da1ef2fbc7318e5db67174fb63a725e72`, package manifest `1413b7a1b8a357c0f1584232884aca0a3e64117a7e4ec72e1da2a5e6da3516c6`, and SBOM `cb872d637bb256e67898fa641baec13cb9de00ba92f8fdf6cbcdcef701dbaba5`. The retained download ZIPs, inner tar and transcript match their published digests. [Packaging job](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422/job/135162193608) · [Install job](https://github.com/godocmat/database-migration-guard/actions/runs/44406264422/job/135166754673) · [Artifact checks](hosted-release-acceptance.md#verified-hosted-run)

The passing native matrix retry built the same macOS source with a different executable digest. Inspection accounted for all 48 differing bytes. The 16-byte Mach-O UUID changed, and the corresponding 32-byte SHA-256 in the linker's ad hoc signature changed for the first 4 KiB page. Every other byte, including the executable's code and data sections, matched. Both jobs reproduced within their own runner. The full installation and exact notice evidence applies to the selected earlier digest. The downloaded retry ZIP, inner tar and manifest verified against their respective pins. [Retry artifact](https://github.com/godocmat/database-migration-guard/actions/runs/44409813029/artifacts/14478871084)

| macOS retry package or transport | SHA-256 |
| --- | --- |
| Executable | `ff365e92519c709e0bf480d27c80812fcca99aa137a7287a4147f86356acb6af` |
| Package manifest | `5416a8c3cbd2df1d8482018d7a77be40b4840f0909a2cdbee449ebf27c1af5d6` |
| CycloneDX SBOM | `39a152b1446e9d72416d740eede2ad5dcbd6337fdbf20bfa382781370ea9f207` |
| Inner tar.gz | `8943357434805c2f84c6bd27276678722104e9ffd7a25402f5a469b30a3ff00b` |
| GitHub artifact ZIP | `8419f703e794b01543f05f38e69fc7d6b3322e69624fb65e6d02f5cbb6fdc0be` |
| Retained macOS job log | `70d22f07d0a0dba28c88df0dd75008ed6f344889c97446845ee96391c40f58d1` |

The local reliability fix `dabc230144a2acb800e9f6c481700d62cec32c2c` on `codex/dmg-16-hosted-verification-recovery` retains the verified Windows and Linux producer bundles for future runs. Both original archive hashes and attestations were verified against the CycloneDX release workflow, source and signer commit `e58bd5590212f82c5b7e16dd3e2e819b0dbea5b1`, tag `cargo-cyclonedx-0.5.9` and GitHub-hosted runner identity. The existing helper installed both generators in a local check that rejected any anonymous API fallback. The Windows and Linux bundle SHA-256 values are `11a76a707a6587554e08ffb617d84af34be1dfd571b993e5ef442900e5ea8385` and `958ba6e08e9a1552181d192174e9400840c5e2973c28a803f19ee0fdf66e87c7`. Publishing the commit for review needs approval. Merging it would create a new candidate revision with new source-bound package metadata. [Pinned producer release](https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/tag/cargo-cyclonedx-0.5.9) · [Verification helper](../../scripts/verify-release-ci.py)

## Disposable PostgreSQL rehearsal

Each hosted Ubuntu 24.04 job builds the locked fixture, loads an exact PostgreSQL image and runs lifecycle, server and wall deadlines, transactional rehearsal, and zero-container checks after each test group. The wrapper uses Docker Engine 29.9.3, vfs and cgroup v2 with the systemd driver inside a private mount and loopback-only network namespace. The wrapper SHA-256 is `983b5835df6c014805473943e0808236634b869112262fa4e00ca7d720a5714b`. The Docker archive SHA-256 is `3fb5d1f13014c18cc54945bc177c58a9056a482dada031e17d12cf2e6922c973` and Crane 0.22.1 archive SHA-256 is `0ab7a1d6932a213aed964ce97666c3077fe691c8606413674a8b3e0b9ec4cda0`. See the workflow for each OCI manifest and config pin. [Pinned workflow at the candidate](https://github.com/godocmat/database-migration-guard/blob/3b0c23de74c6a7d1e03206dbd7b3d60229f14bde/.github/workflows/rehearsal-verification.yml)

| PostgreSQL | Hosted job | Retained job log SHA-256 |
| --- | --- | --- |
| 15.20 | [Passed](https://github.com/godocmat/database-migration-guard/actions/runs/44409812537/job/135174619136) | `3689e05933d78b3eabcd07be434ba9a3ed9e53322320375189dc38ec1e6d4b8e` |
| 16.16 | [Passed](https://github.com/godocmat/database-migration-guard/actions/runs/44409812537/job/135174618844) | `8a79092acdb6cd007ba46fb661af97bf8b62cf01a65fa0d08a6b4e8119027469` |
| 17.12 | [Passed](https://github.com/godocmat/database-migration-guard/actions/runs/44409812537/job/135174618870) | `5fbd330a7912c0c4c9c6082fd09ebc293d2de614a7216309776865397e9c5d7f` |
| 18.7 | [Passed](https://github.com/godocmat/database-migration-guard/actions/runs/44409812537/job/135174619224) | `07aa7088a34a15fcb61f7839b8f36117de328a28d619ee43bf9d2ab7188839f5` |

The workflow completed successfully. All four jobs verified zero containers after each group. It retains logs through Actions. The downloaded run-log ZIP has SHA-256 `343d4166f23552d31e900e7bc020a54ab213e4baa3bc47d302ffd89c4f7aecaa`. [Completed rehearsal run](https://github.com/godocmat/database-migration-guard/actions/runs/44409812537)

The existing disconnected PostgreSQL 15–18 matrix used identical simulation and dependency sources. Its binary and logs remain pinned in the earlier run. The exact wrapper also passed on PostgreSQL 18.7 in the disconnected Ubuntu guest. [Disconnected cgroup v2 matrix](simulation-cgroup-v2-verification.md) · [Disconnected 18.7 wrapper](release-verification-ci.md#offline-wrapper-and-schema-only-replay)

## Live merge enforcement

GitHub's public release repository has protected `main`. Its settings showed no Guard merge requirement, and the repo has no organization or enterprise ruleset to require the controlled workflow. GitHub Free can require an independently reported check from a dedicated GitHub App on a public repository. That version needs a trusted scanner and reporter bound to the exact test merge and a new live pilot. The test archives were downloadable to the authorized GitHub account and denied anonymously. Those downloads establish release-workflow artifact access. [Live GitHub findings and free alternative](dmg-16-pilot-prerequisites.md#live-observations)

No authorized GitLab pilot origin and project were identified. Merged-results pipelines require Premium or Ultimate, and the example's protected pipeline override and direct-fork status check require Ultimate. GitLab Free supports a detached advisory pipeline, protected branches and a maintainer-controlled merge route. A revised trusted gate for that route would need its own acceptance. A qualifying public open-source project could apply for no-charge GitLab for Open Source access after deciding the license and eligibility. [GitLab tier and access details](dmg-16-pilot-prerequisites.md#required-capabilities-and-free-alternatives)

Live pass, deny, incomplete, refreshed-merge, fork and PR/MR report-access cases remain open for both providers. The candidate's local replays cover CLI decisions and report provenance. To close the current profiles at no charge, supply authorized disposable projects with the required protections, controlled configuration sources, a maintainer and fork contributor, and approved artifact delivery with the raw Linux executable URL or immutable GitLab job image digest. The owner can also approve a Free-tier pilot profile and its own live tests. [Detailed case coverage and prerequisites](dmg-16-pilot-prerequisites.md#acceptance-coverage)

## Distribution decisions

- **Project license and notices.** The candidate has no project license file or Cargo license declaration. The rights holder needs to choose the distribution terms, confirm rights to license the contributions, and approve the exact notice set. Commit the license and aligned package metadata through review before final packaging. [Distribution review](dmg-16-distribution-review.md)
- **macOS distribution.** The exact package uses a linker ad hoc signature and imports the OS-provided SystemConfiguration and CoreFoundation frameworks, `libiconv.2.dylib` and `libSystem.B.dylib`. A byte-reproducible 376-file notice draft has ZIP SHA-256 `f3c9ed7d7e9fec2e249223da0fe1f24058c1d411fefb6930a79ab5adc8978d87`. Review the SDK-derived bindings and include the approved arm64 materials with the final package. Developer ID and Apple notarization need an already authorized eligible account to stay within the free constraint. If none is available, choose between deferring the macOS binary or approving a different scoped distribution method and validating its documented installation behavior. [Native notice review](dmg-16-distribution-review.md#macos-arm64-notice-draft) · [Apple notarization](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution) · [Developer Program terms](https://developer.apple.com/programs/whats-included/)
- **Container redistribution.** The two reproduced Wolfi OCI archives contain the exact Linux executable bytes verified at `3b0c23d`. Their original build evidence binds them to recipes at `eb19d1c` and the binary staged from `af21401`. Preserve those source identities for any approved reuse. Both exact images contain the signed Wolfi `busybox-1.38.0-r3` x86-64 APK. Obtain the original downstream configuration, 14 patches and matching build inputs from the producer, or select a base with verifiable corresponding source and rebuild, scan and verify both images. [Exact OCI digests and lineage](dmg-16-candidate-evidence.md#wolfi-image-identities) · [BusyBox source gap](dmg-16-distribution-review.md)
- **Low GitLab advisory.** The exact GitLab image still contains CVE-2026-103995, CVSS 4.0 Low (2.0), in CPython's HTTP client. The protected gate uses Git for HTTPS. The owner can accept the residual for that pinned gate-only profile, stage a verified package containing the CPython 3.15 backport and repeat image acceptance, or defer the image. [Advisory scope and backport](dmg-16-candidate-evidence.md#residual-gitlab-advisory)
- **Holdout and performance.** The frozen Windows `af21401` rescore matched all 100 gate and coverage outcomes and all 16 destructive findings. It matched 56 of 64 exact required findings. Four PG003 object identities, four PG007 anchors, four PG004 expected absences and eight of ten exact diagnostic anchors remained discrepant. Review those limitations for the tested plain SQL and SQLx scope or correct them and have the frozen set rescored independently. The recorded Linux benchmark met the planning budgets on a separate cross-built binary with matched source inputs. [Accuracy and performance scope](dmg-16-candidate-evidence.md#accuracy-and-performance-disposition)

## Signing and provenance plan

1. Approve a protected source ref, complete notice payloads, subject digests and an immutable signing workflow revision. Build and verify on public standard GitHub-hosted runners. Use read-only permissions in verification and keep signing and publication credentials in individually authorized jobs.
2. In the approved workflow, pin `actions/attest` by commit and grant `id-token: write` and `attestations: write` only to the attestation job. Require a reviewer on the public release environment and prevent self-review where the approved staffing permits. Verify the staged artifacts against the trusted build manifest before attesting. Public GitHub repositories qualify for artifact attestations and environment reviewers on Free. [GitHub attestations](https://docs.github.com/en/actions/how-tos/secure-your-work/use-artifact-attestations/use-artifact-attestations) · [Public deployment environments](https://docs.github.com/en/actions/how-tos/deploy/configure-and-manage-deployments/manage-environments)
3. Attest the bytes that users receive, including final binaries, package manifests and archives, and the OCI manifests by immutable digest. Bind the matching CycloneDX SBOM to each executable or OCI digest with the reviewed predicate type. If platform signing changes binary bytes, regenerate package digests and SBOM subject bindings and repeat installed-path verification with the final bytes.
4. Verify all subject attestations against the approved repository, signer workflow and signer commit, source ref and full source commit. For disconnected verification, retain each Sigstore bundle and approved trusted roots with the checksums, notices and SBOMs. Verify OCI digests again after transfer. GitHub/Sigstore provenance establishes the build identity. Platform trust also requires the approved macOS distribution procedure and any Windows trust requirements the owner sets. [Verification runbook](../release.md#attest-and-verify-approved-artifacts) · [GitHub CLI verifier](https://cli.github.com/manual/gh_attestation_verify)
5. Obtain separate approval for any follow-up publication, signing, tag creation or release binary and image uploads. Promote the verified digests through a dedicated job with only the required release or registry write permission.
