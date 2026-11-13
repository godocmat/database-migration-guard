# Integrated native Linux x86-64 release candidate

The unsigned Linux package was staged on 2026-11-13 from the clean reviewed commit `af2140159686067995a4d374ea824400f5453d07`. It reports `dmg 0.1.0` and uses ruleset 2. [Release staging procedure](../release.md#build-a-native-candidate)

## Build inputs and environment

The build ran inside a fresh headless Ubuntu Server 24.04.5 LTS x86-64 QEMU 11.1.0 guest accelerated by Windows Hypervisor Platform. It used kernel `6.8.0-147-generic`, two vCPUs, 8 GiB of configured RAM and an ext4 root. QEMU supplied no network interface. The guest checked that `lo` was the only interface. The Canonical image dated 2026-10-17 had SHA-256 `9a1e3959c9f4ad9d4752101fd4ff9a43191fb7d72e61fa20e033f9fd235ce794` and was verified against the signed cloud-image checksum list. [Ubuntu cloud-image release](https://cloud-images.ubuntu.com/releases/noble/release-20261017/)

The fixture used Rust 1.100.0 (`0ef1bc386`), Cargo 1.100.0 (`3acc79c0c`), GCC 13.3.0, binutils 2.42, Clang and libclang 18.1.3, glibc `2.39-0ubuntu8.9`, Git 2.43.0 and Python 3.12.3. It installed 42 compiler and dependency packages from the Ubuntu snapshot `20261112T180000Z` after checking the signed index and each package digest. Clang resource headers supplied by `libclang-common-18-dev` were needed by bindgen to resolve `stdbool.h`. The guest verified all input checksums, cloned the exact source from a local Git bundle, and used the locked Cargo cache offline. Both stages ran as an unprivileged build user. The cargo-cyclonedx 0.5.9 release archive was checked against its upstream SHA-256 and release-workflow SLSA attestation before use. [Ubuntu Snapshot Service](https://snapshot.ubuntu.com/) · [cargo-cyclonedx 0.5.9 release](https://github.com/CycloneDX/cyclonedx-rust-cargo/releases/tag/cargo-cyclonedx-0.5.9)

The script stages the package from a clean Git archive with fresh build directories and the locked offline release profile. Its Linux path remapping applies to Rust and C. The native package build is distinct from the [cross-built Linux performance measurement](../../fixtures/dmg-15/README.md).

## Package verification

| Package file | Bytes | SHA-256 |
| --- | ---: | --- |
| `dmg-0.1.0-x86_64-unknown-linux-gnu` | 18,207,000 | `503ccfcd47ba561d826a4d0cea0b0da6723d260a421f26677b9bc98175093ea2` |
| `dmg.cdx.json` | 229,719 | `a1ee140a14ddbe44fe2d75c09cf08fcff1f94056b38e39207104f45d26df0671` |
| `package.json` | — | `ed658151bc2e96bc1ed3a5842936667c68517bf581ee80bd4424a8fcf1fe729e` |
| `SHA256SUMS` | — | `dc9c55927ee1f1a71a65cd78da044da83c25c6b5b0f65e60f4c1797868cfa6e7` |

The two separate archive extractions and target directories produced byte-identical executables, SBOMs, manifests and checksum files. The verifier checked both manifests and all staged file bytes in the guest. On the host it checked the exported package again against the first stage's serial-recorded manifest pin. The exported gzip archive had SHA-256 `4f68373a6749556cd56969ff582b58d5505a2ac6ffcba3727ce4d6ba9ac8c99c`. The CycloneDX 1.5 schema and dependency-reference checks passed. The SBOM binds the executable hash and records 132 target sources, 45 build-only components and six vendored target sources. The incomplete composition records the scope of the source inventory. [CycloneDX 1.5 schema](https://github.com/CycloneDX/specification/blob/1.5/schema/bom-1.5.schema.json)

The Linux archive's `Cargo.lock` SHA-256 was `aa374723aa1e78466cc54a42bf63195e7e66790aa843495d6e8263f6895b4221`. Git for Windows applied its system `core.autocrlf=true` setting when archiving the same commit. Converting the Windows archive's `Cargo.lock` and `rust-toolchain.toml` CRLF bytes to LF matched the Linux archive hashes. Package manifests record each build's extracted bytes. The matching build evidence uses the same Git settings within each platform.

The ELF is a dynamically linked x86-64 PIE with GNU build ID `b3987fb336ac906ea565afa122e90528a52ffacd`. It requires `libgcc_s.so.1`, `libc.so.6` and `ld-linux-x86-64.so.2`, all resolved on the guest.

## Disconnected installation checks

- A separate unprivileged user installed the verified candidate and the pinned pilot from `c4974cd11180440d710c0843a952caa11ce786e3` to separate directories. The GitHub local replay passed on the pilot, the installed candidate, and the restored pilot. Each run verified the exact merge parents and original exits 0, 1 and 3. [GitHub replay](../../integrations/github/tests/replay.py)
- The candidate installed smoke verified pass, deny and incomplete reports and their HTML, SARIF and GitLab Code Quality rendering. All ten GitLab local replay tests passed. [Installed smoke](../../scripts/smoke-release.py) · [GitLab replay](../../integrations/gitlab/tests/replay.py)
- The installed binary matched all 130 public CLI outcomes, with 48 passes, 58 denials and 24 incomplete results. Temporary test directories were empty at teardown, and the VM exported the package and powered down cleanly. [Public fixtures](../../fixtures/dmg-02/README.md)

The reproducibility result covers a matched build environment with one Ubuntu image and locked Cargo cache. Hosted CI enforcement and signed release acceptance remain pending.
