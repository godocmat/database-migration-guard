# Database Migration Guard

Database Migration Guard reviews PostgreSQL migrations before deployment. The `dmg` CLI finds risks such as table drops, likely rewrites, and blocking index builds. It can compare exact Git commits and apply a policy to the findings, migration history, and analysis coverage. Local `scan` and `check` run offline.

Guard supports PostgreSQL 15–18 and documented plain SQL, SQLx, Flyway, Goose, and Supabase migration profiles. It marks analysis incomplete when it encounters unsupported SQL or lacks required evidence. [Supported SQL](docs/postgresql-support.md) · [Migration formats](docs/migration-formats.md) · [Risk rules](docs/rules.md)

The project is free to use, modify, and redistribute under the [MIT License](LICENSE).

## Download v0.1.1

| Platform | Archive |
| --- | --- |
| Linux x86-64, tested on Ubuntu 24.04 with glibc 2.39 | [Download tar.gz](https://github.com/godocmat/database-migration-guard/releases/download/v0.1.1/dmg-0.1.1-x86_64-unknown-linux-gnu.tar.gz) |
| Windows x86-64 | [Download ZIP](https://github.com/godocmat/database-migration-guard/releases/download/v0.1.1/dmg-0.1.1-x86_64-pc-windows-msvc.zip) |
| macOS on Apple silicon (arm64), tested on macOS 15 | [Download from the release assets](https://github.com/godocmat/database-migration-guard/releases/tag/v0.1.1#assets) |

The extracted CLI runs offline for local migration checks. Install Git to compare commits. Windows uses the x64 Visual C++ Redistributable. [Release notes](https://github.com/godocmat/database-migration-guard/releases/tag/v0.1.1) · [Microsoft runtime downloads](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist)

For Linux and Windows, save the archive and `SHA256SUMS` in the same empty folder. Verify the checksum list's attestation before extracting a download. Replace `COMMIT` with the approved full source commit from the release notes, then use the commands for your platform below. [SHA256SUMS](https://github.com/godocmat/database-migration-guard/releases/download/v0.1.1/SHA256SUMS) · [GitHub CLI attestation verification](https://cli.github.com/manual/gh_attestation_verify)

~~~sh
gh attestation verify SHA256SUMS --repo godocmat/database-migration-guard --signer-workflow godocmat/database-migration-guard/.github/workflows/native-release.yml --source-ref refs/heads/main --source-digest COMMIT --signer-digest COMMIT --deny-self-hosted-runners
~~~

To verify with the downloaded attestation bundle, also pass `--bundle provenance.jsonl` to the same command. [Attestation bundle](https://github.com/godocmat/database-migration-guard/releases/download/v0.1.1/provenance.jsonl)

### Linux

~~~sh
archive=dmg-0.1.1-x86_64-unknown-linux-gnu.tar.gz
awk -v name="$archive" 'NF == 2 && $2 == name { print }' SHA256SUMS |
  sha256sum --check --strict &&
  mkdir dmg-install &&
  tar -xzf "$archive" -C dmg-install &&
  ./dmg-install/dmg --version
~~~

### Windows PowerShell

~~~powershell
$archive = 'dmg-0.1.1-x86_64-pc-windows-msvc.zip'
$pattern = '^([a-f0-9]{64})  ' + [regex]::Escape($archive) + '$'
$checksumEntries = @(Select-String -LiteralPath SHA256SUMS -Pattern $pattern)
if ($checksumEntries.Count -ne 1) { throw 'Expected one archive checksum' }
$expected = $checksumEntries[0].Matches[0].Groups[1].Value
if ((Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash -ne $expected) { throw 'Archive checksum mismatch' }
Expand-Archive -LiteralPath $archive -DestinationPath dmg-install
& .\dmg-install\dmg.exe --version
~~~

### macOS on Apple silicon

From an empty folder, use the authenticated GitHub CLI to download the archive and verify its attestation before extraction. Replace `WORKFLOW_COMMIT` with the full macOS signing workflow commit from the release notes. The archive's authenticated package manifest records the v0.1.1 source commit. The executable uses a linker-generated ad hoc signature. [Release notes](https://github.com/godocmat/database-migration-guard/releases/tag/v0.1.1)

~~~sh
repo=godocmat/database-migration-guard
archive=dmg-0.1.1-aarch64-apple-darwin.tar.gz
workflow_commit=WORKFLOW_COMMIT
gh release download v0.1.1 --repo "$repo" --pattern "$archive" --pattern macos-provenance.jsonl &&
  gh attestation verify "$archive" --bundle macos-provenance.jsonl --repo "$repo" \
    --signer-workflow "$repo/.github/workflows/macos-release.yml" --source-ref refs/heads/main \
    --source-digest "$workflow_commit" --signer-digest "$workflow_commit" --deny-self-hosted-runners &&
  mkdir dmg-install &&
  tar -xzf "$archive" -C dmg-install &&
  ./dmg-install/dmg --version
~~~

The archives include the MIT license, third-party notices, SBOM, and build manifest.

## Build from source

Install Git, rustup, and the native compiler and libclang for your platform. Rustup selects the pinned Rust toolchain, and Cargo downloads the locked dependencies on the first build. [Linux, Windows, and macOS build prerequisites](CONTRIBUTING.md#native-postgresql-parser)

~~~sh
git clone https://github.com/godocmat/database-migration-guard.git
cd database-migration-guard
cargo build --locked --release -p dmg-cli
~~~

The executable is `target/release/dmg` on Linux and macOS or `target/release/dmg.exe` on Windows. [Release status](docs/release.md#acceptance-matrix)

## Try an offline check

Run the example from the folder containing `dmg-install`. For a source build, run from the Guard checkout and replace `dmg-install` in the executable path with `target/release`. The example writes and checks a sample migration in a temporary directory. For your own repository, select its PostgreSQL major, migration roots, and runner profile.

### Linux or macOS

~~~sh
dmg="$(pwd)/dmg-install/dmg"
cd "$(mktemp -d)"
"$dmg" init --target 18
mkdir migrations
printf '%s\n' 'CREATE TABLE public.accounts (id bigint PRIMARY KEY);' > migrations/001_create_accounts.sql
printf '%s\n' 'schema_version = 1' > policy.toml
"$dmg" check --policy policy.toml --format json --output report.json
~~~

### Windows PowerShell

~~~powershell
$dmg = (Resolve-Path 'dmg-install/dmg.exe').Path
$demo = Join-Path ([IO.Path]::GetTempPath()) "dmg-demo-$([guid]::NewGuid())"
New-Item -ItemType Directory -Path $demo | Out-Null
Set-Location -LiteralPath $demo
& $dmg init --target 18
New-Item -ItemType Directory -Path migrations | Out-Null
'CREATE TABLE public.accounts (id bigint PRIMARY KEY);' | Set-Content -Encoding ascii migrations/001_create_accounts.sql
'schema_version = 1' | Set-Content -Encoding ascii policy.toml
& $dmg check --policy policy.toml --format json --output report.json
~~~

The example returns exit `0` and writes a `pass` decision to `report.json`. Checks can also return `1` for a policy denial, `2` for invalid input, or `3` for incomplete coverage or an operational failure. For revision checks, supply full commit IDs with `--base` and `--head` and load configuration and policy from a trusted source. [CLI reference](docs/cli.md)

## Further workflows

- Collect scoped catalog and migration history evidence over verified TLS, or compare saved schemas and application contracts. [Inspection](docs/inspect.md) · [Schema diffs](docs/diff.md)
- Plan a rehearsal offline or run it in a disposable PostgreSQL worker on Linux x86-64. [Rehearsal](docs/simulation.md)
- Render saved reports as Markdown, HTML, SARIF, GitHub annotations, or GitLab Code Quality. [Report formats](docs/report-formats.md)
- Adapt the GitHub and GitLab CI examples to your repository. Protected merge enforcement requires provider-specific settings and live pilot verification. [GitHub](docs/github-pilot.md) · [GitLab](docs/gitlab-ci.md)

Run `cargo test --locked --workspace` from the Guard checkout. [Contributing and verification](CONTRIBUTING.md)

Use GitHub private vulnerability reporting for security issues. [Security policy](SECURITY.md)
