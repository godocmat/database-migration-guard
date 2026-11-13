# Native Windows build and installation verification

On 2026-11-13, the native Windows x64 CLI was built from clean revision `0bb56e501eee7234cb61bbc1f3a815a74a6b924d`. This provisional revision preceded the final release integration. The two unsigned builds with release-specific path mapping and reproducible linking matched byte for byte. The staged executable passed a local installation and rollback check against the retained Windows pilot from `c4974cd11180440d710c0843a952caa11ce786e3`.

## Build inputs

| Input | Verified value |
| --- | --- |
| Windows | Windows 11 Pro x64, build 26200.9974 |
| Target | `x86_64-pc-windows-msvc` |
| Rust | rustc 1.100.0 `0ef1bc386059596dada8fb65b50425aab2ea637a` with LLVM 23.1.5, Cargo 1.100.0 `3acc79c0c` |
| Native build tools | MSVC compiler 19.44.35230.0, MSVC linker 14.44.35230.0, Windows SDK 10.0.26100.0, libclang 21.1.8 |
| Lockfile SHA-256 | `52e64d2a9c272af997bbc8163330c81b19ddbfc4239124a50bfcc519e07572d6` |
| Toolchain file SHA-256 | `131a9598b9ff5e45f9d8a6157f3d9b2e38703aa31644b74012e5cdc9fa63e23d` |
| Source commit Unix time | `1794587380` |

The file digests are from the Windows checkout bytes. Every build used the same clean checkout, toolchain, and populated Cargo cache, with a fresh target directory and a separate temporary directory. Cargo ran with `--locked --offline` and `DOCS_RS=1`, which selects the parser's bundled generated Protobuf bindings.

| Build | Bytes | SHA-256 |
| --- | ---: | --- |
| Baseline A | 13,623,808 | `605fd2d24544ed683d024a96755590dea06091fa0886de83e01b11b0df4b2805` |
| Baseline B | 13,623,808 | `87424e51fbb947d340d73f3dfff830f1905ad8b1d932865e2d2be30b60928037` |
| Mapped and reproducibly linked C and D | 13,612,032 each | `93a671cdcf80bfdf8361fa66f3b5c2ec7f25c474341d4a10dc481095e4a5c106` |

The baseline files differed at 43 bytes in the PE `.rdata` section. They held 27 target-directory characters in native parser paths, including three UTF-16 occurrences, and the 16-byte CodeView PDB identifier. The mapped pair stored the 27 path occurrences under the fixed `release-build` prefix and retained neither temporary target name. Both executables had the same CodeView identifier and a PE reproducible debug record. Rust path remapping covers Rust-generated paths. MSVC C requires its own mapping for native source paths. With the tested compiler, `/pathmap` took effect with `/experimental:deterministic`. [Rust path remapping](https://doc.rust-lang.org/nightly/rustc/remap-source-paths.html) · [PE debug directory format](https://learn.microsoft.com/en-us/windows/win32/debug/pe-format)

## Repeat the build

Run PowerShell from the reviewed checkout with the [native parser prerequisites](../../CONTRIBUTING.md#native-postgresql-parser). Select the same x64 compiler, linker, Windows SDK, libclang, Rust toolchain and local Cargo cache. Set `CARGO_HOME` to that cache. Set `$dmgBuildRoot` to a fresh directory outside the checkout. The checkout, cache and build paths used in this recipe contain no spaces.

~~~powershell
$dmgSource = $PWD.Path
$dmgSourceSlash = $dmgSource.Replace('\', '/')
$dmgCache = $env:CARGO_HOME
$dmgCacheNative = [IO.Path]::GetFullPath($dmgCache)
$env:CARGO_BUILD_JOBS = '3'
$env:CARGO_INCREMENTAL = '0'
$env:CARGO_NET_OFFLINE = 'true'
$env:DOCS_RS = '1'
$env:SOURCE_DATE_EPOCH = (git show -s --format=%ct HEAD).Trim()

foreach ($dmgRun in @('c', 'd')) {
    $dmgTarget = "$dmgBuildRoot/target-$dmgRun"
    $dmgTemp = "$dmgBuildRoot/tmp-$dmgRun"
    New-Item -ItemType Directory -Path $dmgTarget, $dmgTemp -ErrorAction Stop | Out-Null
    $dmgTargetNative = [IO.Path]::GetFullPath($dmgTarget)
    $env:CARGO_TARGET_DIR = $dmgTarget
    $env:TMP = $dmgTemp
    $env:TEMP = $dmgTemp

    $dmgMappings = @(
        "$dmgTargetNative=release-build"
        "$dmgTarget=release-build"
        "$dmgSource=release-source"
        "$dmgSourceSlash=release-source"
        "$dmgCacheNative=release-crates"
        "$dmgCache=release-crates"
    )
    $dmgCFlags = @('/experimental:deterministic') + @(
        $dmgMappings | ForEach-Object { "/pathmap:$_" }
    )
    $dmgRustFlags = @('-Clink-arg=/Brepro') + @(
        $dmgMappings | ForEach-Object { "--remap-path-prefix=$_" }
    )
    $env:CFLAGS_x86_64_pc_windows_msvc = $dmgCFlags -join ' '
    $env:CARGO_ENCODED_RUSTFLAGS = $dmgRustFlags -join [char]31

    cargo build --locked --offline --release --target x86_64-pc-windows-msvc -p dmg-cli
    if ($LASTEXITCODE -ne 0) { throw "Windows release build failed for $dmgRun" }
    Get-FileHash -Algorithm SHA256 -LiteralPath "$dmgTarget/x86_64-pc-windows-msvc/release/dmg.exe"
}
~~~

The build comparison covered fresh output directories with a shared checkout and dependency cache on one host. Rebuilding from independent checkouts or on different hosts requires separate evidence.

## Installation and rollback

[The pilot staging command](../../scripts/stage-pilot.py) copied the executable to a fresh package directory and wrote `package.json` and `SHA256SUMS`. The verification checked the manifest's source revision, target, filename, size and SHA-256 against the staged bytes and checked both copied executables against their reviewed digests before invocation. The retained pilot executable was 7,268,352 bytes and matched SHA-256 `e1c374c1a0c2b08c2fdd34f04f3ebe38dc811fbd5077000ff8f5853449be3c97`.

Both packages reported `dmg 0.1.0`. The installation directories were named for the source revisions so the candidate could run beside the retained pilot. The smoke test ran the pilot, installed and tested the candidate, then returned to the pilot for the same checks in a new fixture directory. Both installed executable digests stayed unchanged. The fixture removed its temporary installations and Git repositories at exit.

- Both installations passed version, help, project initialization for PostgreSQL 18, policy validation and a complete filesystem scan without Git on the process `PATH`.
- Both installations checked a local Git repository with no remotes. An additive migration passed with exit 0, a column drop produced `PG001` and exit 1, and malformed SQL made coverage incomplete with exit 3. Each JSON report recorded the exact requested base and head and the supplied policy digest even though the working tree contained the malformed migration. Missing `--head` returned exit 2 and preserved the existing report. Markdown retained the finding.
- The candidate rendered the saved pilot JSON as HTML and emitted SARIF 2.1.0 and GitLab Code Quality with the expected finding. It compared identical local snapshots with complete coverage and bound a plan-only rehearsal to the exact Git revisions and config and policy digests. `inspect --help` passed.

The staged candidate also passed the three-case local GitHub replay and all ten local GitLab replay tests using temporary Git repositories. They exercised protected base inputs, exact revision checks, preserved gate exits and review artifacts. [GitHub replay](../../integrations/github/tests/replay.py) · [GitLab replay](../../integrations/gitlab/tests/replay.py)

The installed commands used an isolated temporary home, Git configuration and temp directory. The process `PATH` contained Windows system directories and, where needed, Git for Windows 2.45.1. The executable imported the Windows UCRT and `VCRUNTIME140.dll`. The host supplied `ucrtbase.dll` 10.0.26100.9711 and `VCRUNTIME140.dll` 14.51.36247.0. Host networking stayed enabled. Clean installation in a disconnected Windows VM, final integrated build acceptance, and signed release artifacts remain pending.
